//! B8: legacy `/api/*` routes (API.md former § 13) must all 404.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

fn app() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("x.txt"), "x\n").unwrap();
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
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        false,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    std::mem::forget(dir);
    std::mem::forget(home);
    server::build_router(st, guard, last, false, None)
}

const TOKEN: &str = "01234567890123456789012345678901";

async fn status(method: &str, uri: &str) -> StatusCode {
    let r = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    app().oneshot(r).await.unwrap().status()
}

#[tokio::test]
async fn legacy_api_paths_return_not_found() {
    let gets = [
        "/api/health",
        "/api/stats",
        "/api/files",
        "/api/fuzzy?q=x",
        "/api/search?q=x",
        "/api/file?path=x.txt",
        "/api/file-meta?path=x.txt",
        "/api/file-window?path=x.txt",
        "/api/highlight?path=x.txt",
        "/api/git-status",
        "/api/diff",
        "/api/pr-info",
        "/api/markdown?path=x.md",
        "/api/raw?path=x.png",
        "/api/review/drafts",
        "/api/settings",
    ];
    for uri in gets {
        assert_eq!(status("GET", uri).await, StatusCode::NOT_FOUND, "{uri}");
    }
    assert_eq!(
        status("DELETE", "/api/review/drafts/draft-id").await,
        StatusCode::NOT_FOUND
    );
    let posts = [
        ("/api/ask", r#"{"prompt":"hi"}"#),
        ("/api/ask/stream", r#"{"prompt":"hi"}"#),
        (
            "/api/review/drafts",
            r#"{"path":"x.txt","line":1,"body":"c"}"#,
        ),
        ("/api/review/submit", r#"{}"#),
        ("/api/review/apply", r#"{}"#),
        ("/api/git/stage", r#"{"paths":["x.txt"]}"#),
        ("/api/git/unstage", r#"{"paths":["x.txt"]}"#),
        ("/api/git/commit", r#"{"message":"m"}"#),
        ("/api/git/commit-message", r#"{}"#),
        ("/api/git/push", r#"{}"#),
        ("/api/git/pull", r#"{}"#),
    ];
    for (uri, body) in posts {
        let r = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7778")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        assert_eq!(
            app().oneshot(r).await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let r = Request::builder()
        .method("PUT")
        .uri("/api/settings")
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"ui.theme":"x"}"#))
        .unwrap();
    assert_eq!(
        app().oneshot(r).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}
