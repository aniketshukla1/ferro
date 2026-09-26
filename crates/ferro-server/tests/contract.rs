//! B1 contract tests: every v1 endpoint's shape and status codes (API.md).
//! Driven against the real router with oneshot requests.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

fn state() -> (axum::Router, tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(
        dir.path().join("main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("sub/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "# Title\n\nHello.\n").unwrap();
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
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    (server::build_router(st, guard, last, true, None), dir, home)
}

const TOKEN: &str = "01234567890123456789012345678901";

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

fn put(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
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
async fn meta_shape() {
    let (app, _d, _h) = state();
    let (s, v) = j(app, "/api/v1/meta").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["api"], 1);
    assert_eq!(v["specVersion"], "1.0");
    assert_eq!(v["host"], "cli");
    for f in [
        "v1",
        "events",
        "settings",
        "session",
        "tree",
        "file",
        "markdown.v2",
        "outline",
        "jobs",
        "workspace.open",
        "desktop",
        "metrics",
        "fuzzy.v2",
        "search.v2",
        "search.regex",
        "search.stream",
        "file.find",
        "paths.resolve",
        "git.status.v2",
        "git.changes",
        "git.diff.v2",
        "git.blob",
        "git.gutter",
        "git.write",
        "git.log",
    ] {
        assert!(
            v["features"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(f)),
            "{f}"
        );
    }
    for l in [
        "maxWindowLines",
        "maxCols",
        "maxRawBytes",
        "maxMarkdownBytes",
        "maxSearchFiles",
        "maxDiffRows",
    ] {
        assert!(v["limits"][l].is_number(), "{l}");
    }
    assert!(v["workspace"]["key"].as_str().unwrap().len() == 16);
}

#[tokio::test]
async fn tree_and_file() {
    let (app, _d, _h) = state();
    let (s, v) = j(app.clone(), "/api/v1/tree").await;
    assert_eq!(s, StatusCode::OK);
    let names: Vec<&str> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"main.rs") && names.contains(&"sub"));
    // Directories first.
    assert_eq!(v["entries"][0]["dir"], true);
    let (s, v) = j(app.clone(), "/api/v1/tree?dir=sub").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["entries"][0]["path"], "sub/lib.rs");
    let (s, missing) = j(app.clone(), "/api/v1/tree?dir=nope").await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{missing}");

    let (s, v) = j(app.clone(), "/api/v1/file?path=main.rs").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["lines"], 3);
    assert_eq!(v["language"], "Rust");
    assert_eq!(v["kind"], "text");
    assert_eq!(v["eol"], "lf");
    assert_eq!(v["encoding"], "utf-8");
    let (s, _) = j(app.clone(), "/api/v1/file?path=../x").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn file_lines_window() {
    let (app, _d, _h) = state();
    let (s, v) = j(
        app.clone(),
        "/api/v1/file/lines?path=main.rs&from=2&count=1",
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 3);
    assert_eq!(v["lines"][0]["n"], 2);
    assert!(v["lines"][0]["html"].as_str().unwrap().contains("println"));
    assert_eq!(v["language"], "Rust");
    let (s, v) = j(
        app.clone(),
        "/api/v1/file/lines?path=main.rs&from=99&count=5",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["lines"].as_array().unwrap().is_empty());
    // maxCols cut is flagged.
    let (s, v) = j(
        app.clone(),
        "/api/v1/file/lines?path=main.rs&from=1&count=3&hl=0&maxCols=4",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["lines"][0]["cut"], 11);
    // Review fix: with highlighting on, a cut line is still truncated plain text
    // (highlighted HTML would carry the whole line and ignore maxCols).
    let (s, v) = j(
        app.clone(),
        "/api/v1/file/lines?path=main.rs&from=1&count=3&hl=1&maxCols=4",
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["lines"][0]["cut"], 11);
    assert_eq!(v["lines"][0]["text"], "fn m");
    assert!(v["lines"][0].get("html").is_none(), "{v}");
    // Line 2 is also over 4 units (cut); line 3 (`}`) fits and keeps its HTML.
    assert!(v["lines"][1]["cut"].is_number(), "{v}");
    assert_eq!(v["lines"][2]["html"], "}", "{v}");
    assert!(v["lines"][2].get("cut").is_none(), "{v}");
}

/// Review fix: CRLF files never leak `\r` or `\n` into highlighted lines.
#[tokio::test]
async fn crlf_lines_have_no_terminators() {
    let (app, d, _h) = state();
    std::fs::write(d.path().join("w.rs"), "//! doc\r\nfn a() {} // c\r\n").unwrap();
    let (s, v) = j(app, "/api/v1/file/lines?path=w.rs&from=1&count=2&hl=1").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for l in v["lines"].as_array().unwrap() {
        let html = l["html"].as_str().or(l["text"].as_str()).unwrap();
        assert!(!html.contains('\r') && !html.contains('\n'), "{l}");
    }
}

#[tokio::test]
async fn file_raw_and_markdown() {
    let (app, _d, _h) = state();
    let res = app
        .clone()
        .oneshot(get("/api/v1/file/raw?path=main.rs"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("sandbox"));
    let (s, v) = j(app.clone(), "/api/v1/file/markdown?path=README.md").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["html"].as_str().unwrap().contains("<h1"), "{}", v["html"]);
    assert_eq!(v["headings"][0]["id"], "title");

    // Render endpoint with suggestion context.
    let res = app
        .clone()
        .oneshot(
            post("/api/v1/markdown/render", r#"{"text":"```suggestion\nnew\n```\n","context":{"path":"main.rs","startLine":1,"endLine":1}}"#),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn highlight_and_outline() {
    let (app, _d, _h) = state();
    let res = app
        .clone()
        .oneshot(post(
            "/api/v1/highlight",
            r#"{"code":"fn main() {}","language":"rs"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["language"], "Rust");
    assert!(v["lines"][0].as_str().unwrap().contains("t-k"));

    let (s, v) = j(app, "/api/v1/file/outline?path=main.rs").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["source"], "regex");
    assert_eq!(v["symbols"][0]["name"], "main");
}

#[tokio::test]
async fn settings_session_jobs_workspace() {
    let (app, _d, _h) = state();
    let (s, v) = j(app.clone(), "/api/v1/settings/schema").await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k["key"] == "theme"));
    let (s, v) = j(app.clone(), "/api/v1/settings").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["values"]["theme"], "forge");

    // Invalid values rejected with field detail.
    let res = app
        .clone()
        .oneshot(put(
            "/api/v1/settings?scope=workspace",
            r#"{"values":{"theme":"nope"}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Session round-trip.
    let res = app
        .clone()
        .oneshot(put("/api/v1/session", r#"{"data":{"tabs":["a"]}}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let (s, v) = j(app.clone(), "/api/v1/session").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["tabs"][0], "a");

    // Jobs + rebuild.
    let res = app
        .clone()
        .oneshot(post("/api/v1/index/rebuild", ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let (s, v) = j(app.clone(), "/api/v1/jobs").await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["kind"] == "index.rebuild"));

    // Workspace open rejects PR urls in B1; bad paths 404.
    // B4: prUrl starts a pr.open job (bad URLs are 400).
    let res = app
        .clone()
        .oneshot(post("/api/v1/workspace/open", r#"{"prUrl":"nope"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    for (uri, body, want) in [(
        "/api/v1/workspace/open",
        r#"{"path":"/nonexistent-xyz"}"#,
        StatusCode::NOT_FOUND,
    )] {
        let res = app.clone().oneshot(post(uri, body)).await.unwrap();
        assert_eq!(res.status(), want, "{uri}");
    }

    // Desktop endpoints are 422 on the CLI host.
    let res = app
        .oneshot(post("/api/v1/desktop/pick-folder", ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn events_and_metrics() {
    let (app, _d, _h) = state();
    let res = app.clone().oneshot(get("/api/v1/events")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("text/event-stream"));
    let (s, v) = j(app, "/api/v1/metrics").await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["uptimeMs"].is_number());
    assert!(v["rssBytes"].is_number());
}
