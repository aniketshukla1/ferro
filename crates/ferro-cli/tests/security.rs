//! B0 security regressions (BACKEND.md § 6 B0 acceptance + D1).
//! Drives the real router with oneshot requests. No network, no git needed.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro::{guard::GuardConfig, server};
use std::sync::Arc;
use tower::ServiceExt;

fn app(token: &str) -> axum::Router {
    app_with_files(token, &[]).0
}

fn app_with_files(token: &str, files: &[(&str, &[u8])]) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let state = Arc::new(ferro_core::Index::new(dir.path().to_path_buf()));
    let guard = Arc::new(GuardConfig::new(Some(token.into()), 7778, vec![], false));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    (server::router(state, guard, last, true), dir)
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
    let cookie = format!("ferro_7778={TOKEN}");
    let r = req("POST", "/api/review/drafts")
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(status(app(TOKEN), r).await, StatusCode::FORBIDDEN);
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
    let (app, _dir) = app_with_files(
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
    let (app, _dir) = app_with_files(
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
