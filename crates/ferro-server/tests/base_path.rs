//! `--base-path`: ferro under a reverse-proxy subpath.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn app() -> (axum::Router, tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        dir.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    let guard = Arc::new(
        ferro_server::guard::GuardConfig::new(
            Some(TOKEN.into()),
            7778,
            vec![],
            false,
            false,
            false,
        )
        .with_base_path(Some("/ferro".into())),
    );
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let router = server::build_router(st, guard, last, true, None);
    (server::under_base_path("/ferro".into(), router), dir, home)
}

fn get(uri: &str, bearer: bool) -> Request<Body> {
    let mut b = Request::builder()
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778");
    if bearer {
        b = b.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn api_and_ui_live_under_the_prefix() {
    let (app, _d, _h) = app();
    let res = app
        .clone()
        .oneshot(get("/ferro/api/v1/meta", true))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .clone()
        .oneshot(get("/api/v1/meta", true))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "outside the prefix");
    let res = app
        .clone()
        .oneshot(get("/ferrox/api/v1/meta", true))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "a prefix of the prefix is not the prefix"
    );
    // Without the trailing slash, relative asset URLs would resolve above the prefix.
    let res = app
        .clone()
        .oneshot(get("/ferro?file=a.rs", false))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(res.headers()[header::LOCATION], "/ferro/?file=a.rs");
}

#[tokio::test]
async fn token_bootstrap_redirect_keeps_the_prefix() {
    let (app, _d, _h) = app();
    let res = app
        .oneshot(get(&format!("/ferro/?token={TOKEN}&file=a.rs"), false))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(res.headers()[header::LOCATION], "/ferro/?file=a.rs");
}

#[test]
fn base_path_normalization() {
    use server::normalize_base_path as n;
    assert_eq!(n("ferro").as_deref(), Some("/ferro"));
    assert_eq!(n("/tools/ferro/").as_deref(), Some("/tools/ferro"));
    assert_eq!(n("/"), None);
    assert_eq!(n(""), None);
    for bad in [
        "/a/../b",
        "/a?x=1",
        "/a#b",
        "//evil.example",
        "/a\\b",
        "/./a",
    ] {
        assert_eq!(n(bad), None, "{bad}");
    }
}
