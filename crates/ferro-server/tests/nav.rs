//! B6: `/symbols` fuzzy search over the workspace symbol index.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn state_with_files(files: &[(&str, &str)]) -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    for (p, content) in files {
        let full = dir.path().join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
    }
    let dir = Box::leak(Box::new(dir));
    let home = Box::leak(Box::new(tempfile::tempdir().unwrap()));
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
    // Synchronous file index so the snapshot exists for symbol extraction.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let idx = st.ws().index.clone();
                rt.block_on(idx.rebuild());
            })
            .join()
            .unwrap();
    });
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    server::build_router(st, guard, last, true, None)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

async fn j(router: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = router.oneshot(get(uri)).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, v)
}

fn app() -> axum::Router {
    state_with_files(&[
        (
            "src/main.rs",
            "fn main() {\n    serve();\n}\nfn serve() {}\n",
        ),
        (
            "src/lib.rs",
            "pub struct Scheduler;\nimpl Scheduler {\n pub fn run(&self) {}\n}\n",
        ),
    ])
}

#[tokio::test]
async fn symbols_find_by_name() {
    let (s, v) = j(app(), "/api/v1/symbols?q=main").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let names: Vec<&str> = v["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"main"), "{v}");
    let main = &v["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "main")
        .unwrap();
    assert_eq!(main["kind"], "function");
    assert_eq!(main["path"], "src/main.rs");
    assert_eq!(main["line"], 1);
    assert!(main["score"].is_number());
}

#[tokio::test]
async fn symbols_fuzzy_matches() {
    let (s, v) = j(app(), "/api/v1/symbols?q=schdlr").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let names: Vec<&str> = v["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Scheduler"), "{v}");
}

#[tokio::test]
async fn symbols_empty_q_and_limits() {
    let (s, v) = j(app(), "/api/v1/symbols?q=").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["symbols"].as_array().unwrap().len(), 0);
    let (s, v) = j(app(), "/api/v1/symbols?q=main&limit=1").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["symbols"].as_array().unwrap().len() <= 1);
    let long = "x".repeat(300);
    let (s, v) = j(app(), &format!("/api/v1/symbols?q={long}")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], "bad_request");
}
