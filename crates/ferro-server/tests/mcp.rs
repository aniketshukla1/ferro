//! MCP HTTP integration (B7b).

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

fn git(args: &[&str], dir: &std::path::Path) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "{args:?}");
}

async fn fixture_app() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    git(&["init", "-b", "main"], dir.path());
    git(&["config", "user.email", "t@t"], dir.path());
    git(&["config", "user.name", "t"], dir.path());
    git(&["config", "commit.gpgsign", "false"], dir.path());
    std::fs::write(
        dir.path().join("main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    git(&["add", "main.rs"], dir.path());
    git(&["commit", "-m", "init"], dir.path());
    std::fs::write(
        dir.path().join("main.rs"),
        "fn main() {\n    println!(\"hi\");\n    println!(\"edited\");\n}\n",
    )
    .unwrap();
    // The router keeps these paths. Leaking matches the other server tests.
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
    st.ws().index.rebuild().await;
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        false,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    server::build_router(st, guard, last, true, None)
}

const TOKEN: &str = "01234567890123456789012345678901";

fn mcp_post(body: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = bearer {
        b = b.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn mcp_call(app: &axum::Router, body: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(mcp_post(body, Some(TOKEN)))
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1_000_000)
        .await
        .unwrap();
    let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

fn tool_call(name: &str, args: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    })
    .to_string()
}

fn assert_tool_envelope(name: &str, status: StatusCode, v: &serde_json::Value) {
    assert_eq!(status, StatusCode::OK, "{name} {v}");
    assert!(v.get("error").is_none(), "{name} {v}");
    assert!(v["result"]["content"][0]["text"].is_string(), "{name} {v}");
    assert!(v["result"]["isError"].is_boolean(), "{name} {v}");
}

#[tokio::test]
async fn mcp_http_requires_bearer() {
    let app = fixture_app().await;
    let res = app
        .oneshot(mcp_post(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_http_rejects_wrong_bearer() {
    let app = fixture_app().await;
    let res = app
        .oneshot(mcp_post(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#,
            Some("00000000000000000000000000000000"),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_http_list_and_call_tools() {
    let app = fixture_app().await;
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    let (status, _) = mcp_call(&app, init).await;
    assert_eq!(status, StatusCode::OK);
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    let (status, v) = mcp_call(&app, list).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 9);
    for n in [
        "ferro_search",
        "ferro_fuzzy",
        "ferro_read",
        "ferro_outline",
        "ferro_definition",
        "ferro_references",
        "ferro_diff",
        "ferro_pr_threads",
        "ferro_add_draft",
    ] {
        assert!(names.contains(&n), "{n}");
        let tool = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == n)
            .unwrap();
        assert!(tool["inputSchema"].is_object(), "{n}");
    }

    let cases = [
        (
            "ferro_search",
            serde_json::json!({"q": "println"}),
            true,
            "println",
        ),
        (
            "ferro_fuzzy",
            serde_json::json!({"q": "main"}),
            true,
            "main.rs",
        ),
        (
            "ferro_read",
            serde_json::json!({"path": "main.rs", "start": 1, "count": 5}),
            true,
            "println",
        ),
        (
            "ferro_outline",
            serde_json::json!({"path": "main.rs"}),
            true,
            "main",
        ),
        (
            "ferro_definition",
            serde_json::json!({"path": "main.rs", "line": 1, "col": 4}),
            false,
            "",
        ),
        (
            "ferro_references",
            serde_json::json!({"path": "main.rs", "line": 1, "col": 4}),
            false,
            "",
        ),
        ("ferro_diff", serde_json::json!({}), true, "main.rs"),
        (
            "ferro_pr_threads",
            serde_json::json!({}),
            true,
            "not in PR mode",
        ),
    ];
    for (name, args, expect_ok, needle) in cases {
        let (status, v) = mcp_call(&app, &tool_call(name, args)).await;
        assert_tool_envelope(name, status, &v);
        if expect_ok {
            assert_eq!(v["result"]["isError"], false, "{name} {v}");
            let text = v["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains(needle), "{name} text={text}");
        }
    }

    let (status, v) = mcp_call(
        &app,
        &tool_call(
            "ferro_add_draft",
            serde_json::json!({"path": "main.rs", "line": 1, "body": "note"}),
        ),
    )
    .await;
    assert_tool_envelope("ferro_add_draft", status, &v);
    assert_eq!(v["result"]["isError"], true);
    assert!(v["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("not in PR mode"));

    let (status, v) = mcp_call(
        &app,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"ferro_read","arguments":{"path":""}}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["result"]["isError"], true);

    let (status, v) = mcp_call(
        &app,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ferro_read","arguments":[]}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["error"]["code"], -32602);

    let (status, v) = mcp_call(&app, "[]").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["error"]["code"], -32600);

    let (status, v) = mcp_call(
        &app,
        &tool_call(
            "ferro_add_draft",
            serde_json::json!({"path": "../etc/passwd", "line": 1, "body": "no"}),
        ),
    )
    .await;
    assert_tool_envelope("ferro_add_draft escape", status, &v);
    assert_eq!(v["result"]["isError"], true);
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("escapes") || text.contains("path"), "{text}");
    assert!(!text.contains("not in PR mode"));

    let (status, v) = mcp_call(
        &app,
        &tool_call(
            "ferro_add_draft",
            serde_json::json!({"path": ".env", "line": 1, "body": "no"}),
        ),
    )
    .await;
    assert_tool_envelope("ferro_add_draft env", status, &v);
    assert_eq!(v["result"]["isError"], true);
    assert!(v["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("never-send"));
}
