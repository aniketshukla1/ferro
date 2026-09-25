//! B0 security regressions (BACKEND.md § 6 B0 acceptance + D1).
//! Drives the real router with oneshot requests. No network, no git needed.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::{guard::GuardConfig, server};
use std::sync::Arc;
use tower::ServiceExt;

fn app_with_files(
    token: &str,
    files: &[(&str, &[u8])],
) -> (axum::Router, tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let state = server::build_state(
        dir.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    let guard = Arc::new(GuardConfig::new(Some(token.into()), 7778, vec![], false));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    (
        server::build_router(state, guard, last, true, None),
        dir,
        home,
    )
}

fn app(token: &str) -> axum::Router {
    // Leaks the tempdirs for the test lifetime (oneshot is synchronous here).
    let (router, dir, home) = app_with_files(token, &[]);
    std::mem::forget(dir);
    std::mem::forget(home);
    router
}

fn req(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
}

async fn status(router: axum::Router, req: Request<Body>) -> StatusCode {
    router.oneshot(req).await.unwrap().status()
}

const TOKEN: &str = "01234567890123456789012345678901";

#[tokio::test]
async fn api_without_credentials_is_401() {
    let s = status(
        app(TOKEN),
        req("GET", "/api/stats").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bearer_auth_works() {
    let r = req("GET", "/api/stats")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(status(app(TOKEN), r).await, StatusCode::OK);
}

#[tokio::test]
async fn cookie_auth_works_after_bootstrap() {
    let app = app(TOKEN);
    // Bootstrap sets HttpOnly + SameSite=Strict cookie and strips the token.
    let res = app
        .clone()
        .oneshot(
            req("GET", &format!("/?token={TOKEN}&file=a.rs&line=3"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FOUND);
    let set_cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.contains("ferro_7778="), "{set_cookie}");
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
    assert_eq!(
        res.headers().get(header::LOCATION).unwrap(),
        "/?file=a.rs&line=3"
    );
    assert!(!set_cookie.contains(TOKEN) || set_cookie.matches(TOKEN).count() <= 1);
    // Cookie authenticates API.
    let cookie = set_cookie.split(';').next().unwrap().to_string();
    let r = req("GET", "/api/stats")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let app2 = app;
    assert_eq!(status(app2, r).await, StatusCode::OK);
}

#[tokio::test]
async fn bad_bootstrap_token_is_403() {
    let s = status(
        app(TOKEN),
        req("GET", "/?token=nope").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_origin_post_without_origin_needs_bearer() {
    // Cookie alone is not enough when Origin is absent (curl-style callers need Bearer).
    let app = app(TOKEN);
    let cookie = sign_in(app.clone(), "127.0.0.1:7778").await.join("; ");
    let r = req("POST", "/api/review/drafts")
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(status(app, r).await, StatusCode::FORBIDDEN);
}

// ---------- remembered browsers ----------

/// Router whose guard remembers browsers with a key in `state_dir`.
fn app_remembering(
    token: &str,
    state_dir: &std::path::Path,
    allow_hosts: Vec<String>,
) -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let state = server::build_state(
        dir.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    std::mem::forget(dir);
    std::mem::forget(home);
    let key = ferro_server::guard::DeviceKey::load_or_create(state_dir).unwrap();
    let guard = Arc::new(
        GuardConfig::new(Some(token.into()), 7778, allow_hosts, false).with_device_key(key),
    );
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    server::build_router(state, guard, last, true, None)
}

/// Open the token link on `host`; returns the `name=value` cookie pairs set.
async fn sign_in(app: axum::Router, host: &str) -> Vec<String> {
    let res = app
        .oneshot(
            Request::builder()
                .uri(format!("/?token={TOKEN}"))
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FOUND);
    set_cookies(&res)
}

fn set_cookies(res: &axum::response::Response) -> Vec<String> {
    res.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .collect()
}

fn api_get(uri: &str, host: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::HOST, host)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn remembered_browser_survives_a_restart() {
    let key_dir = tempfile::tempdir().unwrap();
    let app1 = app_remembering(TOKEN, key_dir.path(), vec![]);
    let cookies = sign_in(app1, "127.0.0.1:7778").await;
    let device = cookies
        .iter()
        .find(|c| c.starts_with("ferro_device="))
        .expect("remembered")
        .clone();
    assert!(!device.contains(TOKEN));
    // New launch: new random token and a new port, same OS user state dir.
    let app2 = app_remembering("another-launch-token-0123456789ab", key_dir.path(), vec![]);
    let res = app2
        .oneshot(api_get("/api/v1/meta", "127.0.0.1:7791", &device))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // A fresh cookie is not re-issued on every request (only after a day).
    assert!(set_cookies(&res).is_empty(), "{:?}", set_cookies(&res));
}

#[tokio::test]
async fn session_sign_in_is_upgraded_but_bearer_never_is() {
    let key_dir = tempfile::tempdir().unwrap();
    let app = app_remembering(TOKEN, key_dir.path(), vec![]);
    let session = sign_in(app.clone(), "127.0.0.1:7778")
        .await
        .into_iter()
        .find(|c| c.starts_with("ferro_7778="))
        .unwrap();
    // A browser holding only this launch's session cookie gets remembered.
    let res = app
        .clone()
        .oneshot(api_get("/api/v1/meta", "127.0.0.1:7778", &session))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(set_cookies(&res)
        .iter()
        .any(|c| c.starts_with("ferro_device=v1.")));
    // Scripts using the token get data, never cookies.
    let res = app
        .oneshot(
            req("GET", "/api/v1/meta")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(set_cookies(&res).is_empty());
}

#[tokio::test]
async fn browsers_are_remembered_only_on_loopback_hosts() {
    let key_dir = tempfile::tempdir().unwrap();
    let app = app_remembering(TOKEN, key_dir.path(), vec!["box.ts.net".into()]);
    let remote = sign_in(app.clone(), "box.ts.net:7778").await;
    assert!(
        remote.iter().all(|c| !c.starts_with("ferro_device=")),
        "{remote:?}"
    );
    // A remembered cookie from this machine is not accepted through another host name.
    let local = sign_in(app.clone(), "127.0.0.1:7778").await;
    let device = local
        .iter()
        .find(|c| c.starts_with("ferro_device="))
        .unwrap();
    let res = app
        .oneshot(api_get("/api/v1/meta", "box.ts.net:7778", device))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sign_out_this_browser_and_every_browser() {
    let key_dir = tempfile::tempdir().unwrap();
    let app = app_remembering(TOKEN, key_dir.path(), vec![]);
    let cookies = sign_in(app.clone(), "127.0.0.1:7778").await.join("; ");
    let other_browser = sign_in(app.clone(), "127.0.0.1:7778").await.join("; ");
    let logout = |uri: &str, cookie: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7778")
            .header(header::ORIGIN, "http://127.0.0.1:7778")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap()
    };
    // This browser: the remembered cookie and every ferro session cookie it sent
    // expire (cookies are per host, so another port's session arrives too).
    let with_other_port = format!("{cookies}; ferro_7791=x; ferro_theme=dark");
    let res = app
        .clone()
        .oneshot(logout("/api/v1/auth/logout", &with_other_port))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let expired: Vec<String> = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    let names: Vec<&str> = expired
        .iter()
        .map(|c| c.split('=').next().unwrap())
        .collect();
    assert_eq!(
        names,
        ["ferro_device", "ferro_7778", "ferro_7791"],
        "{expired:?}"
    );
    assert!(
        expired.iter().all(|c| c.contains("Max-Age=0")),
        "{expired:?}"
    );
    // Every browser: the other browser's session and remembered cookies stop working.
    let res = app
        .clone()
        .oneshot(logout("/api/v1/auth/logout?all=1", &other_browser))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    for c in other_browser.split("; ") {
        let s = app
            .clone()
            .oneshot(api_get("/api/v1/meta", "127.0.0.1:7778", c))
            .await
            .unwrap()
            .status();
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{c}");
    }
    // The printed link still signs in again.
    assert_eq!(sign_in(app, "127.0.0.1:7778").await.len(), 2);
}

#[tokio::test]
async fn cross_origin_post_is_403() {
    let r = req("POST", "/api/review/drafts")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::ORIGIN, "https://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(status(app(TOKEN), r).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn same_origin_post_with_bearer_passes_guard() {
    let r = req("POST", "/api/review/drafts")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::ORIGIN, "http://127.0.0.1:7778")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"path":"a","line":1,"body":"x"}"#))
        .unwrap();
    // Passes guards (handler may 400/200 on its own merits, but not 401/403).
    let s = status(app(TOKEN), r).await;
    assert!(
        s != StatusCode::UNAUTHORIZED && s != StatusCode::FORBIDDEN,
        "{s}"
    );
}

#[tokio::test]
async fn foreign_host_is_403_on_static_and_api() {
    for uri in ["/", "/api/stats"] {
        let r = Request::builder()
            .method("GET")
            .uri(uri)
            .header(header::HOST, "attacker.example")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status(app(TOKEN), r).await, StatusCode::FORBIDDEN, "{uri}");
    }
}

#[tokio::test]
async fn security_headers_present() {
    let r = req("GET", "/")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = app(TOKEN).oneshot(r).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let h = res.headers();
    assert!(h.contains_key("x-content-type-options"));
    assert!(h.contains_key("referrer-policy"));
    assert!(h.contains_key("cross-origin-opener-policy"));
    assert!(h.contains_key("permissions-policy"));
    let csp = h.get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
}

#[tokio::test]
async fn markdown_endpoint_strips_xss() {
    let (app, _dir, _home) = app_with_files(
        TOKEN,
        &[(
            "evil.md",
            b"# T\n\n<img src=x onerror=alert(1)>\n\n[evil](javascript:alert(2))\n\n<script>alert(3)</script>\n",
        )],
    );
    let r = req("GET", "/api/markdown?path=evil.md")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(r).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("<h1>"), "{html}");
    assert!(!html.contains("alert("), "{html}");
    assert!(!html.contains("javascript:"), "{html}");
    assert!(!html.contains("<script"), "{html}");
}

#[tokio::test]
async fn raw_svg_has_sandbox_csp_and_inline_disposition() {
    let (app, _dir, _home) = app_with_files(
        TOKEN,
        &[(
            "x.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
        )],
    );
    let r = req("GET", "/api/raw?path=x.svg")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(r).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let h = res.headers();
    assert_eq!(h.get(header::CONTENT_TYPE).unwrap(), "image/svg+xml");
    let csp = h
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(csp.contains("sandbox"), "{csp}");
    let cd = h
        .get(header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cd.starts_with("inline;"), "{cd}");
}

#[tokio::test]
#[cfg(unix)]
async fn symlink_escape_is_403() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "s3cret").unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("inside.txt"), "in").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        dir.path().join("evil.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(dir.path().join("inside.txt"), dir.path().join("ok.txt")).unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let state = server::build_state(
        dir.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    let guard = Arc::new(GuardConfig::new(Some(TOKEN.into()), 7778, vec![], false));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let app = server::build_router(state, guard, last, true, None);
    let get = |path: &str| {
        req("GET", &format!("/api/file?path={path}"))
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        status(app.clone(), get("evil.txt")).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(status(app.clone(), get("ok.txt")).await, StatusCode::OK);
    // Traversal over HTTP is also 403, not a silent miss.
    assert_eq!(
        status(app, get("../secret.txt")).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn utf8_boundary_file_returns_valid_utf8() {
    // 'é' (2 bytes) straddles the old 512 KiB slicing point: must not panic or truncate mid-char.
    let mut bytes = vec![b'a'; 512 * 1024 - 1];
    bytes.extend_from_slice("é".as_bytes());
    bytes.extend_from_slice(b"tail");
    let (app, _dir, _home) = app_with_files(TOKEN, &[("uni.txt", &bytes)]);
    let r = req("GET", "/api/file?path=uni.txt")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(r).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let s = std::str::from_utf8(&body).expect("response must be valid UTF-8");
    // The é straddling the cap is backed off, never split: valid UTF-8, ≤ cap.
    assert!(s.len() <= 512 * 1024, "{}", s.len());
    assert!(s.ends_with('a'));
}
