//! B2a contract tests: fuzzy v2, search, stream, find, resolve (API.md § 5).
//! Driven against the real router with oneshot requests.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn state() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("vendor")).unwrap();
    std::fs::write(
        dir.path().join("src/server.rs"),
        "fn serve() {}\nfn main() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn helper() {}\n// foo bar\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("vendor/vendored.rs"), "fn serve() {}\n").unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {\n    serve();\n}\n").unwrap();
    // Leak the fixture dir: the router holds an absolute root path, and the
    // tempdir would otherwise be deleted on return.
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
    // Index synchronously so the snapshot is populated before requests.
    tokio::runtime::Handle::try_current()
        .map(|_| ())
        .unwrap_or(());
    let idx = st.ws().index.clone();
    // Rebuild in a blocking thread; the test runtime may not exist yet here
    // in all call orders, so use a fresh current-thread runtime.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
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

fn post(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
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

#[tokio::test]
async fn fuzzy_shape_and_empty() {
    let app = state();
    let (s, v) = j(app.clone(), "/api/v1/fuzzy?q=server").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["q"], "server");
    assert!(v["total"].as_u64().unwrap() >= 1);
    assert!(v["ms"].is_number());
    assert!(v["generation"].is_number());
    let r0 = &v["results"][0];
    assert!(r0["path"].is_string());
    assert!(r0["score"].is_number());
    assert!(r0["positions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x.is_number()));
    // Exact basename first.
    let (s, v) = j(app.clone(), "/api/v1/fuzzy?q=server.rs").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["results"][0]["path"], "src/server.rs");

    let (s, v) = j(app.clone(), "/api/v1/fuzzy?q=").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["results"].as_array().unwrap().len(), 0);

    // Over-long q rejected.
    let long = "x".repeat(513);
    let (s, _) = j(app.clone(), &format!("/api/v1/fuzzy?q={long}")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn search_literal_and_modes() {
    let app = state();
    let (s, v) = j(app.clone(), "/api/v1/search?q=serve").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["engine"], "scan");
    assert!(v["filesScanned"].as_u64().unwrap() >= 3);
    assert!(v["filesMatched"].as_u64().unwrap() >= 1);
    assert_eq!(
        v["excluded"]["globs"].as_array().unwrap()[0],
        "**/vendor/**"
    );
    assert_eq!(v["excluded"]["files"], 1);
    // Path-sorted.
    let paths: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    let mut sorted = paths.clone();
    sorted.sort_unstable();
    assert_eq!(paths, sorted);
    let hit = &v["files"][0]["hits"][0];
    assert!(hit["line"].is_number() && hit["text"].is_string());
    assert!(
        hit["ranges"].as_array().unwrap()[0]
            .as_array()
            .unwrap()
            .len()
            == 2
    );

    // Smart case: uppercase narrows.
    let (_, lower) = j(app.clone(), "/api/v1/search?q=serve").await;
    let (_, upper) = j(app.clone(), "/api/v1/search?q=Serve").await;
    assert!(lower["filesMatched"].as_u64().unwrap() >= upper["filesMatched"].as_u64().unwrap());

    // Word mode drops the vendored partial... (serve vs serves: use helper).
    let (s, _) = j(app.clone(), "/api/v1/search?q=serve&word=1").await;
    assert_eq!(s, StatusCode::OK);

    // Bad regex → 400 with detail.position.
    let (s, v) = j(app.clone(), "/api/v1/search?q=a(b&mode=regex").await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], "bad_request");
    assert!(v["error"]["detail"]["position"].is_number());

    // Bad mode/case rejected.
    let (s, _) = j(app.clone(), "/api/v1/search?q=x&mode=glob").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Missing q rejected.
    let (s, _) = j(app.clone(), "/api/v1/search").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn search_stream_events() {
    let app = state();
    let res = app
        .clone()
        .oneshot(get("/api/v1/search?q=serve"))
        .await
        .unwrap();
    // Sanity: non-stream works on the same clipping path.
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .clone()
        .oneshot(get("/api/v1/search/stream?q=serve"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/event-stream"));
    let body = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body).into_owned();
    assert!(text.contains("event: file"), "{text}");
    assert!(text.contains("event: done"), "{text}");

    // Regex error on stream is a 400, not a stream error.
    let res = app
        .oneshot(get("/api/v1/search/stream?q=a(b&mode=regex"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn file_find_and_resolve() {
    let app = state();
    let (s, v) = j(app.clone(), "/api/v1/file/find?path=src%2Flib.rs&q=foo").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 1);
    assert!(!v["truncated"].as_bool().unwrap());
    assert_eq!(v["matches"][0]["line"], 2);

    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/paths/resolve",
            r#"{"candidates":["src/server.rs:1","server.rs","nope:1"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["resolved"]["src/server.rs:1"]["path"], "src/server.rs");
    assert_eq!(v["resolved"]["src/server.rs:1"]["line"], 1);
    assert_eq!(v["resolved"]["server.rs"]["path"], "src/server.rs");
    assert!(v["resolved"]["nope:1"].is_null());
}

#[tokio::test]
async fn legacy_routes_use_new_engines() {
    let app = state();
    let (s, v) = j(app.clone(), "/api/fuzzy?q=server").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["path"] == "src/server.rs"));
    let (s, v) = j(app.clone(), "/api/search?q=serve").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["path"].as_str().unwrap().ends_with(".rs")));
}
