//! Request guards per API.md §§ 1.2–1.3 (B0).
//! Token + cookie bootstrap, Host guard, Origin guard, security headers.
//! Moves to ferro-server in B1; the behavior contract stays the same.
//!
//! Remembered browsers: opening the token link on a loopback address also sets
//! `ferro_device`, a 30-day cookie (renewed on use) signed with a per-user key
//! kept in the ferro state dir. It survives restarts and port changes, so the
//! link is needed once per browser. `POST /api/v1/auth/logout` forgets this
//! browser; `?all=1` rotates the key and forgets every browser.

use axum::{
    body::Body,
    http::{header, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Remembered-browser cookie. Not port-scoped: it works for every ferro this
/// OS user runs on the same host name.
pub const DEVICE_COOKIE: &str = "ferro_device";
/// A remembered browser stays signed in this long after its last visit.
pub const DEVICE_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// Slide the expiry at most once a day per browser.
const DEVICE_RENEW_SECS: u64 = 24 * 60 * 60;
/// Clock skew tolerated for a cookie that claims to be issued in the future.
const DEVICE_SKEW_SECS: u64 = 5 * 60;

#[derive(Debug, Clone)]
pub struct GuardConfig {
    /// Raw token (also the cookie value and the Bearer value).
    pub token: String,
    /// Cookie name, includes the bound port: `ferro_<port>`.
    pub cookie_name: String,
    /// Extra allowed Host hostnames (beyond loopback).
    pub allow_hosts: Vec<String>,
    /// Skip API auth (loopback binds only).
    pub no_auth: bool,
    /// Signs remembered-browser cookies; `None` keeps sessions browser-lifetime only.
    pub device: Option<DeviceKey>,
    /// Session cookies are derived from the token and this generation, so
    /// "sign out all browsers" can void them while the printed link keeps working.
    session_epoch: Arc<std::sync::atomic::AtomicU64>,
}

impl GuardConfig {
    pub fn new(
        token_opt: Option<String>,
        port: u16,
        allow_hosts: Vec<String>,
        no_auth: bool,
    ) -> Self {
        let token = token_opt
            .filter(|t| !t.is_empty())
            .unwrap_or_else(generate_token);
        Self {
            token,
            cookie_name: format!("ferro_{port}"),
            allow_hosts: allow_hosts.into_iter().map(|h| h.to_lowercase()).collect(),
            no_auth,
            device: None,
            session_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Opaque session cookie value (never the raw token, which also works as Bearer).
    fn session_value(&self) -> String {
        let epoch = self
            .session_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        b64(&hmac_sha256(
            self.token.as_bytes(),
            format!("ferro-session-v1.{epoch}").as_bytes(),
        ))
    }

    /// Sign every browser out: void current session cookies and, when browsers
    /// are remembered, rotate the key. The printed token link still signs in.
    pub fn forget_all_browsers(&self) -> std::io::Result<()> {
        if let Some(d) = &self.device {
            d.rotate()?;
        }
        self.session_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(())
    }

    /// Enable "remember this browser" (the server enables it on loopback binds).
    pub fn with_device_key(mut self, key: DeviceKey) -> Self {
        self.device = Some(key);
        self
    }

    fn host_allowed(&self, host: &str) -> bool {
        let name = host_name(host).to_lowercase();
        name == "127.0.0.1"
            || name == "localhost"
            || name == "::1"
            || self.allow_hosts.iter().any(|h| h == &name)
    }

    /// The per-launch session cookie (lives until the browser closes).
    fn session_cookie(&self) -> String {
        format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            self.cookie_name,
            self.session_value()
        )
    }

    /// Set-Cookie values that sign this browser out of every ferro on this host:
    /// the remembered cookie, this server's session cookie, and the session
    /// cookies of other ports the browser sent (cookies are per host, not port).
    pub fn expired_cookies(&self, cookie_header: Option<&str>) -> Vec<String> {
        let mut names = vec![DEVICE_COOKIE.to_string(), self.cookie_name.clone()];
        for pair in cookie_header.unwrap_or_default().split(';') {
            let name = pair.split_once('=').map_or(pair, |(k, _)| k).trim();
            let is_session = name
                .strip_prefix("ferro_")
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
            if is_session && !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
        names
            .into_iter()
            .map(|n| format!("{n}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"))
            .collect()
    }
}

fn device_cookie(value: &str) -> String {
    format!("{DEVICE_COOKIE}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={DEVICE_TTL_SECS}")
}

/// Remembered browsers are for this machine only: loopback host names.
fn is_loopback_host(host: &str) -> bool {
    matches!(
        host_name(host).to_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

/// True for bind addresses that only this machine can reach.
pub fn is_loopback_bind(host: &str) -> bool {
    matches!(
        host.trim().trim_matches(|c| c == '[' || c == ']'),
        "127.0.0.1" | "localhost" | "::1"
    )
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// HMAC-SHA256 (RFC 2104) on the `sha2` crate already in the tree; checked
/// against the RFC 4231 test vectors below.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= block[i];
        opad[i] ^= block[i];
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// Per-user secret that signs remembered-browser cookies. It lives in
/// `<state_dir>/auth/browser.key` (mode 0600), never inside a repository.
/// Cookie value: `v1.<issued unix secs>.<nonce>.<HMAC-SHA256>`, all base64url.
#[derive(Clone)]
pub struct DeviceKey {
    path: PathBuf,
    key: Arc<parking_lot::RwLock<[u8; 32]>>,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret.
        f.debug_struct("DeviceKey")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl DeviceKey {
    fn with(path: PathBuf, key: [u8; 32]) -> Self {
        Self {
            path,
            key: Arc::new(parking_lot::RwLock::new(key)),
        }
    }

    /// Load the key, creating it on first use. Safe when several ferro
    /// processes start at once: the file is created exclusively and readers
    /// retry while a concurrent writer finishes.
    pub fn load_or_create(state_dir: &Path) -> std::io::Result<Self> {
        let dir = state_dir.join("auth");
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = dir.join("browser.key");
        for _ in 0..50 {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    if let Some(key) = decode_key(&text) {
                        return Ok(Self::with(path, key));
                    }
                    // Empty or partial: a concurrent writer is finishing. Retry.
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let key: [u8; 32] = rand::random();
                    match write_key_new(&path, &key) {
                        Ok(()) => return Ok(Self::with(path, key)),
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Unreadable content that never settles: replace it (forgets remembered browsers).
        let k = Self::with(path, rand::random());
        k.rotate()?;
        Ok(k)
    }

    /// New key, atomically replacing the file: every remembered browser is signed out.
    /// (Other running ferro processes pick the new key up when they restart.)
    pub fn rotate(&self) -> std::io::Result<()> {
        let key: [u8; 32] = rand::random();
        let tmp = self.path.with_extension(format!(
            "key.{}.{}.tmp",
            std::process::id(),
            b64(&rand::random::<[u8; 6]>())
        ));
        write_key_new(&tmp, &key)?;
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        *self.key.write() = key;
        Ok(())
    }

    fn mac(&self, issued: u64, nonce: &str) -> String {
        let msg = format!("ferro-device-v1.{issued}.{nonce}");
        b64(&hmac_sha256(&*self.key.read(), msg.as_bytes()))
    }

    /// A fresh cookie value issued at `now`.
    pub fn issue(&self, now: u64) -> String {
        let nonce = b64(&rand::random::<[u8; 16]>());
        let mac = self.mac(now, &nonce);
        format!("v1.{now}.{nonce}.{mac}")
    }

    /// `Some(issued)` when `value` is a cookie this key signed that has not expired.
    pub fn verify(&self, value: &str, now: u64) -> Option<u64> {
        let mut parts = value.split('.');
        let (v, issued, nonce, mac) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if v != "v1" || parts.next().is_some() || nonce.is_empty() || nonce.len() > 64 {
            return None;
        }
        let issued: u64 = issued.parse().ok()?;
        if issued > now.saturating_add(DEVICE_SKEW_SECS)
            || now.saturating_sub(issued) > DEVICE_TTL_SECS
        {
            return None;
        }
        constant_eq(mac, &self.mac(issued, nonce)).then_some(issued)
    }
}

fn decode_key(text: &str) -> Option<[u8; 32]> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim())
        .ok()?;
    bytes.try_into().ok()
}

/// Create `path` exclusively (fails if it exists), owner-only, and write the key.
fn write_key_new(path: &Path, key: &[u8; 32]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(format!("{}\n", b64(key)).as_bytes())?;
    f.sync_all()
}

/// Strip the port (and brackets) from a Host header value.
fn host_name(host: &str) -> &str {
    let h = host.trim();
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(h);
    }
    match h.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => h,
    }
}

fn constant_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

pub fn generate_token() -> String {
    use base64::Engine as _;
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn bearer(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
}

fn cookie_value(req: &Request<Body>, name: &str) -> Option<String> {
    req.headers()
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k.trim() == name).then(|| v.trim().to_string())
        })
}

/// How a request authenticated, and the remembered-browser cookie to (re)issue.
struct Credentials {
    ok: bool,
    /// Fresh `ferro_device` value for this response: first sign-in on a loopback
    /// host (upgrading a session cookie), or a remembered cookie older than a day.
    renew_device: Option<String>,
}

fn credentials(g: &GuardConfig, req: &Request<Body>, host: &str, now: u64) -> Credentials {
    if g.no_auth {
        return Credentials {
            ok: true,
            renew_device: None,
        };
    }
    let session =
        cookie_value(req, &g.cookie_name).is_some_and(|c| constant_eq(&c, &g.session_value()));
    let device = g.device.as_ref().filter(|_| is_loopback_host(host));
    let remembered =
        device.and_then(|d| cookie_value(req, DEVICE_COOKIE).and_then(|v| d.verify(&v, now)));
    let bearer_ok = bearer(req).is_some_and(|b| constant_eq(&b, &g.token));
    let renew_device = device.and_then(|d| match remembered {
        Some(issued) if now.saturating_sub(issued) >= DEVICE_RENEW_SECS => Some(d.issue(now)),
        Some(_) => None,
        // A browser signed in with this launch's link, not yet remembered.
        None if session => Some(d.issue(now)),
        None => None,
    });
    Credentials {
        ok: session || remembered.is_some() || bearer_ok,
        renew_device,
    }
}

fn deny(code: &str, status: StatusCode, message: impl Into<String>) -> Response {
    let body =
        serde_json::json!({ "error": { "code": code, "message": message.into() } }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .body(Body::from(body))
        .unwrap()
}

/// The single guard layer: Host check for everything, auth + Origin for /api/**.
pub async fn guards(
    Extension(g): Extension<Arc<GuardConfig>>,
    req: Request<Body>,
    next: Next,
) -> impl IntoResponse {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !g.host_allowed(host) {
        return deny("forbidden", StatusCode::FORBIDDEN, "host not allowed").into_response();
    }
    let path = req.uri().path();
    let mut renew_device = None;
    if path.starts_with("/api/") {
        let creds = credentials(&g, &req, host, now_secs());
        if !creds.ok {
            return deny(
                "unauthorized",
                StatusCode::UNAUTHORIZED,
                "missing or invalid credentials",
            )
            .into_response();
        }
        renew_device = creds.renew_device;
        let method = req.method();
        if *method != axum::http::Method::GET && *method != axum::http::Method::HEAD {
            match req
                .headers()
                .get(header::ORIGIN)
                .and_then(|v| v.to_str().ok())
            {
                Some(origin) => {
                    let ok =
                        origin == format!("http://{host}") || origin == format!("https://{host}");
                    if !ok {
                        return deny("forbidden", StatusCode::FORBIDDEN, "origin not allowed")
                            .into_response();
                    }
                }
                None => {
                    // No Origin (curl, scripts): Bearer auth is required.
                    match bearer(&req) {
                        Some(b) if constant_eq(&b, &g.token) => {}
                        _ => {
                            return deny(
                                "forbidden",
                                StatusCode::FORBIDDEN,
                                "bearer auth required without Origin",
                            )
                            .into_response()
                        }
                    }
                }
            }
        }
    }
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    // Remember (or keep remembering) this browser, unless the handler is
    // signing it out (it sets its own expiring cookies).
    if let Some(v) = renew_device.and_then(|v| HeaderValue::from_str(&device_cookie(&v)).ok()) {
        if !headers.contains_key(header::SET_COOKIE) {
            headers.append(header::SET_COOKIE, v);
        }
    }
    headers.insert(
        "X-Content-Type-Options",
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "Referrer-Policy",
        header::HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "Cross-Origin-Opener-Policy",
        header::HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        "Permissions-Policy",
        header::HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if let Some(ct) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        if ct.contains("text/html") {
            headers.insert(
                "Content-Security-Policy",
                header::HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self'; connect-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
            );
        }
    }
    res
}

/// Bootstrap: `GET /?token=<t>` → validate, set cookie, 302 without the token.
/// Returns Some(response) when it handled the request.
pub fn bootstrap(g: &GuardConfig, req: &Request<Body>) -> Option<Response> {
    if req.method() != axum::http::Method::GET {
        return None;
    }
    let query = req.uri().query()?;
    let mut token: Option<&str> = None;
    let mut kept: Vec<(&str, &str)> = Vec::new();
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("token", v)) => token = Some(v),
            Some((k, v)) => kept.push((k, v)),
            None => {}
        }
    }
    let token = token?;
    if !constant_eq(token, &g.token) {
        return Some(deny("forbidden", StatusCode::FORBIDDEN, "invalid token"));
    }
    let qs = kept
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    // Keep the page the link was opened on (`/next.html?token=…`), but never a
    // scheme-relative path (`//host`, `/\host`): that would be an open redirect.
    let path = req.uri().path();
    let path = if path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\") {
        path
    } else {
        "/"
    };
    let location = if qs.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{qs}")
    };
    let mut res = Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .header(header::SET_COOKIE, g.session_cookie());
    // On this machine, also remember the browser (30 days, renewed on use).
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if let Some(d) = g.device.as_ref().filter(|_| is_loopback_host(host)) {
        res = res.header(header::SET_COOKIE, device_cookie(&d.issue(now_secs())));
    }
    res.body(Body::from("")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GuardConfig {
        GuardConfig::new(Some("t".repeat(32)), 7778, vec![], false)
    }

    #[test]
    fn host_names() {
        assert_eq!(host_name("127.0.0.1:7778"), "127.0.0.1");
        assert_eq!(host_name("[::1]:7778"), "::1");
        assert_eq!(host_name("attacker.example"), "attacker.example");
        assert_eq!(host_name("LOCALHOST:1"), "LOCALHOST");
    }

    #[test]
    fn host_allowlist() {
        let g = cfg();
        assert!(g.host_allowed("127.0.0.1:1"));
        assert!(g.host_allowed("localhost"));
        assert!(!g.host_allowed("attacker.example"));
        let g2 = GuardConfig::new(
            Some("t".repeat(32)),
            1,
            vec!["TAILSCALE.ts.net".into()],
            false,
        );
        assert!(g2.host_allowed("tailscale.ts.net:7777"));
    }

    #[test]
    fn token_is_32_random_bytes() {
        use base64::Engine as _;
        for _ in 0..10 {
            let t = generate_token();
            assert_eq!(
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(&t)
                    .unwrap()
                    .len(),
                32
            );
        }
        assert_ne!(generate_token(), generate_token());
    }

    /// Review fix: the redirect keeps the requested page and drops only the token.
    #[test]
    fn bootstrap_keeps_path_but_not_foreign_hosts() {
        let g = cfg();
        let tok = "t".repeat(32);
        let loc = |uri: &str| {
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let res = bootstrap(&g, &req).expect("handled");
            assert_eq!(res.status(), StatusCode::FOUND);
            assert!(res.headers().get(header::SET_COOKIE).is_some());
            res.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(loc(&format!("/next.html?token={tok}")), "/next.html");
        assert_eq!(
            loc(&format!("/next.html?token={tok}&path=a.rs&line=3")),
            "/next.html?path=a.rs&line=3"
        );
        assert_eq!(loc(&format!("/?token={tok}")), "/");
        assert_eq!(loc(&format!("//evil.test/x?token={tok}")), "/");
        assert_eq!(loc(&format!("/\\evil.test/x?token={tok}")), "/");
    }

    #[test]
    fn constant_time() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
        assert!(!constant_eq("abc", "abcd"));
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// RFC 4231 test cases 1, 2 and 6 (key longer than the block).
    #[test]
    fn hmac_sha256_matches_rfc4231() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn device_cookies_verify_and_reject_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let k = DeviceKey::load_or_create(dir.path()).unwrap();
        let now = 1_800_000_000;
        let c = k.issue(now);
        assert_eq!(k.verify(&c, now), Some(now));
        assert_eq!(k.verify(&c, now + DEVICE_TTL_SECS), Some(now));
        // Expired, and issued too far in the future.
        assert_eq!(k.verify(&c, now + DEVICE_TTL_SECS + 1), None);
        assert_eq!(k.verify(&c, now - DEVICE_SKEW_SECS - 1), None);
        // Any edit breaks the MAC: issued time, nonce, mac, version, extra parts.
        let parts: Vec<&str> = c.split('.').collect();
        let bumped = format!("v1.{}.{}.{}", now + 86_400, parts[2], parts[3]);
        assert_eq!(k.verify(&bumped, now + 86_400), None);
        assert_eq!(
            k.verify(&format!("v1.{now}.AAAA{}.{}", parts[2], parts[3]), now),
            None
        );
        assert_eq!(
            k.verify(&format!("v1.{now}.{}.{}x", parts[2], parts[3]), now),
            None
        );
        assert_eq!(k.verify(&c.replacen("v1", "v2", 1), now), None);
        assert_eq!(k.verify(&format!("{c}.x"), now), None);
        for junk in ["", "v1", "v1...", "v1.x.y.z", "💥"] {
            assert_eq!(k.verify(junk, now), None, "{junk}");
        }
        // Another key (another OS user / state dir) does not accept it.
        let other = tempfile::tempdir().unwrap();
        let k2 = DeviceKey::load_or_create(other.path()).unwrap();
        assert_eq!(k2.verify(&c, now), None);
    }

    #[test]
    fn device_key_persists_is_private_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let k1 = DeviceKey::load_or_create(dir.path()).unwrap();
        let c = k1.issue(now_secs());
        // "Restart": a new process loads the same key and accepts the cookie.
        let k2 = DeviceKey::load_or_create(dir.path()).unwrap();
        assert!(k2.verify(&c, now_secs()).is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let meta = std::fs::metadata(dir.path().join("auth/browser.key")).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
        // Debug never prints the secret.
        let dbg = format!("{k1:?}");
        let secret = std::fs::read_to_string(dir.path().join("auth/browser.key")).unwrap();
        assert!(!dbg.contains(secret.trim()), "{dbg}");
        // Rotation voids old cookies, here and after the next restart.
        k1.rotate().unwrap();
        assert!(k1.verify(&c, now_secs()).is_none());
        let k3 = DeviceKey::load_or_create(dir.path()).unwrap();
        assert!(k3.verify(&c, now_secs()).is_none());
        assert!(k3.verify(&k1.issue(now_secs()), now_secs()).is_some());
    }

    #[test]
    fn forgetting_all_browsers_voids_session_cookies_not_the_token() {
        let g = cfg();
        let before = g.session_value();
        assert_ne!(before, g.token, "the cookie never carries the raw token");
        g.forget_all_browsers().unwrap();
        assert_ne!(g.session_value(), before);
        // The printed link (token) still bootstraps a new session.
        let req = Request::builder()
            .uri(format!("/?token={}", g.token))
            .body(Body::empty())
            .unwrap();
        assert_eq!(bootstrap(&g, &req).unwrap().status(), StatusCode::FOUND);
    }

    #[test]
    fn loopback_checks() {
        for h in ["127.0.0.1", "localhost", "::1", "[::1]"] {
            assert!(is_loopback_bind(h), "{h}");
        }
        for h in ["0.0.0.0", "192.168.1.5", "box.ts.net"] {
            assert!(!is_loopback_bind(h), "{h}");
        }
        assert!(is_loopback_host("127.0.0.1:7790"));
        assert!(is_loopback_host("LOCALHOST:1"));
        assert!(is_loopback_host("[::1]:7778"));
        assert!(!is_loopback_host("box.ts.net:7778"));
    }
}
