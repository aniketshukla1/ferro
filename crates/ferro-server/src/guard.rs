//! Request guards per API.md §§ 1.2–1.3 (B0).
//! Token + cookie bootstrap, Host guard, Origin guard, security headers.
//! Moves to ferro-server in B1; the behavior contract stays the same.

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension,
};
use std::sync::Arc;

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
        }
    }

    fn host_allowed(&self, host: &str) -> bool {
        let name = host_name(host).to_lowercase();
        name == "127.0.0.1"
            || name == "localhost"
            || name == "::1"
            || self.allow_hosts.iter().any(|h| h == &name)
    }
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

fn authed(g: &GuardConfig, req: &Request<Body>) -> bool {
    if g.no_auth {
        return true;
    }
    if let Some(c) = cookie_value(req, &g.cookie_name) {
        if constant_eq(&c, &g.token) {
            return true;
        }
    }
    if let Some(b) = bearer(req) {
        if constant_eq(&b, &g.token) {
            return true;
        }
    }
    false
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
    if path.starts_with("/api/") {
        if !authed(&g, &req) {
            return deny(
                "unauthorized",
                StatusCode::UNAUTHORIZED,
                "missing or invalid credentials",
            )
            .into_response();
        }
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
    let location = if qs.is_empty() {
        "/".to_string()
    } else {
        format!("/?{qs}")
    };
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .header(
            header::SET_COOKIE,
            format!(
                "{}={}; HttpOnly; SameSite=Strict; Path=/",
                g.cookie_name, g.token
            ),
        )
        .body(Body::from(""))
        .ok()
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

    #[test]
    fn constant_time() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
        assert!(!constant_eq("abc", "abcd"));
    }
}
