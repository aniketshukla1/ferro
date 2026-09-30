//! Inline edits (API.md § 4.10, § 10.9): `POST /file/edit` against a temp workspace.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn workspace(files: &[(&str, &str)]) -> &'static std::path::Path {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    for (p, c) in files {
        let full = dir.path().join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, c).unwrap();
    }
    dir.path()
}

fn router(root: &std::path::Path, read_only: bool) -> axum::Router {
    let home = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        root.to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        read_only,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    server::build_router(st, guard, last, true, None)
}

async fn post(
    app: axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::HOST, "127.0.0.1:7778")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let s = res.status();
    let b = axum::body::to_bytes(res.into_body(), 8 << 20)
        .await
        .unwrap();
    (
        s,
        serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null),
    )
}

fn edit(path: &str, start: usize, end: usize, expected: &str, text: &str) -> serde_json::Value {
    serde_json::json!({ "path": path, "startLine": start, "endLine": end, "expected": expected, "text": text })
}

#[tokio::test]
async fn edits_a_line_range_in_place() {
    let d = workspace(&[("src/lib.rs", "fn a() {}\r\nfn b() {}\r\nfn c() {}\r\n")]);
    let (s, v) = post(
        router(d, false),
        "/api/v1/file/edit",
        edit("src/lib.rs", 2, 2, "fn b() {}", "fn b() {\n    todo!()\n}"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["lines"], 5);
    assert_eq!(v["endLine"], 4);
    assert_eq!(
        std::fs::read_to_string(d.join("src/lib.rs")).unwrap(),
        "fn a() {}\r\nfn b() {\r\n    todo!()\r\n}\r\nfn c() {}\r\n"
    );
}

#[tokio::test]
async fn refuses_stale_lines_and_says_what_they_read_now() {
    let d = workspace(&[("a.txt", "one\ntwo\n")]);
    let (s, v) = post(
        router(d, false),
        "/api/v1/file/edit",
        edit("a.txt", 2, 2, "TWO", "2"),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "conflict");
    assert_eq!(v["error"]["detail"]["current"], "two");
    assert_eq!(
        std::fs::read_to_string(d.join("a.txt")).unwrap(),
        "one\ntwo\n"
    );
}

#[tokio::test]
async fn refuses_binary_escapes_vcs_dirs_and_read_only_mode() {
    let d = workspace(&[
        ("bin.dat", "a\0b"),
        ("ok.txt", "x\n"),
        (".git/config", "[core]\n"),
    ]);
    let (s, _) = post(
        router(d, false),
        "/api/v1/file/edit",
        edit("bin.dat", 1, 1, "a\0b", "c"),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = post(
        router(d, false),
        "/api/v1/file/edit",
        edit("../outside.txt", 1, 1, "", "c"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = post(
        router(d, false),
        "/api/v1/file/edit",
        edit(".git/config", 1, 1, "[core]", "x"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = post(
        router(d, false),
        "/api/v1/file/edit",
        edit("ok.txt", 3, 3, "", "c"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = post(
        router(d, true),
        "/api/v1/file/edit",
        edit("ok.txt", 1, 1, "x", "y"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(std::fs::read_to_string(d.join("ok.txt")).unwrap(), "x\n");
}

#[tokio::test]
async fn ai_edit_needs_an_instruction_and_honours_never_send() {
    let d = workspace(&[(".env", "KEY=1\n"), ("a.rs", "fn a() {}\n")]);
    let body = |path: &str, instruction: &str| serde_json::json!({ "path": path, "startLine": 1, "endLine": 1, "instruction": instruction, "text": "KEY=1" });
    let (s, _) = post(router(d, false), "/api/v1/ai/edit", body("a.rs", "  ")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = post(router(d, false), "/api/v1/ai/edit", body(".env", "rename")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
}
