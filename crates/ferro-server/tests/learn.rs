//! Learning conventions from merged pull requests (`POST /memory/learn`): a mock GitHub and a
//! mock Anthropic on one local server, no network. One test per binary: it sets process env.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn git(args: &[&str], dir: &std::path::Path) {
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success(),
        "{args:?}"
    );
}

fn app(root: &std::path::Path) -> axum::Router {
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
        false,
        false,
    ));
    server::build_router(
        st,
        guard,
        Arc::new(std::sync::Mutex::new(std::time::Instant::now())),
        true,
        None,
    )
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let req = b
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 << 20)
        .await
        .unwrap();
    (
        s,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// GitHub's merged-PR and review-comment lists, and an Anthropic reply naming one convention.
async fn mock() -> String {
    use axum::routing::{get, post};
    let comment = |id: u64, pr: u64, user: &str, body: &str| {
        serde_json::json!({
            "id": id, "pull_request_url": format!("https://api.github.com/repos/o/r/pulls/{pr}"),
            "html_url": format!("https://github.com/o/r/pull/{pr}#discussion_r{id}"),
            "user": {"login": user, "type": "User"}, "path": "src/lib.rs", "body": body,
        })
    };
    let comments = serde_json::json!([
        comment(1, 20, "rev", "Please add a regression test for this fix"),
        comment(2, 21, "rev", "This needs a test that reproduces the bug"),
        comment(3, 21, "bob", "Thanks, I added the test you asked for now"),
        comment(
            4,
            22,
            "sam",
            "Could you add a test covering the empty input?"
        ),
        comment(
            5,
            22,
            "rev",
            "Log the error instead of silently ignoring it"
        ),
    ]);
    let answer = r#"{"conventions":[{"text":"Add a regression test with every bug fix","category":"tests","evidence":["1","2","4"]},{"text":"Log errors","category":"errors","evidence":["5"]}]}"#;
    let sse: String = [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#.to_string(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#.into(),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":answer}}).to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#.into(),
        r#"{"type":"message_stop"}"#.into(),
    ]
    .iter()
    .map(|e| format!("event: x\ndata: {e}\n\n"))
    .collect();
    let app = axum::Router::new()
        .route(
            "/api/repos/o/r/pulls",
            get(|| async {
                axum::Json(serde_json::json!([
                    {"number": 22, "merged_at": "2026-09-03T00:00:00Z", "user": {"login": "bob"}},
                    {"number": 21, "merged_at": "2026-09-02T00:00:00Z", "user": {"login": "bob"}},
                    {"number": 20, "merged_at": "2026-09-01T00:00:00Z", "user": {"login": "ann"}}
                ]))
            }),
        )
        .route(
            "/api/repos/o/r/pulls/comments",
            get(move || async move { axum::Json(comments) }),
        )
        .route(
            "/v1/messages",
            post(move || async move { ([(header::CONTENT_TYPE, "text/event-stream")], sse) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn learns_a_convention_from_merged_prs_and_proposes_it() {
    for k in [
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "GEMINI_API_KEY",
        "OLLAMA_MODEL",
        "ANTHROPIC_AUTH_TOKEN",
    ] {
        std::env::remove_var(k);
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    git(&["init", "-q", "-b", "main"], d);
    let app = app(d);

    // No forge remote yet: nothing to learn from.
    let (s, v) = call(
        &app,
        "POST",
        "/api/v1/memory/learn",
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");

    let base = mock().await;
    std::env::set_var("FERRO_FORGE_API_BASE", format!("{base}/api"));
    std::env::set_var("GITHUB_TOKEN", "test-token");
    std::env::set_var("ANTHROPIC_API_KEY", "test-key");
    std::env::set_var("ANTHROPIC_BASE_URL", &base);
    git(&["remote", "add", "origin", "git@github.com:o/r.git"], d);

    let (_, mem) = call(&app, "GET", "/api/v1/memory", None).await;
    assert_eq!(mem["forge"]["repo"], "o/r");
    let (s, v) = call(
        &app,
        "POST",
        "/api/v1/memory/learn",
        Some(serde_json::json!({ "prs": 10 })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["job"]["id"].as_str().unwrap().to_string();
    let mut job = serde_json::Value::Null;
    for _ in 0..100 {
        job = call(&app, "GET", &format!("/api/v1/jobs/{id}"), None)
            .await
            .1;
        if job["state"] == "done" || job["state"] == "failed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(job["state"], "done", "{job}");
    // bob's own reply on #21 is not review feedback; "Log errors" has one PR only.
    assert_eq!(job["result"]["prs"], 3);
    assert_eq!(job["result"]["comments"], 4);
    assert_eq!(job["result"]["conventions"], 1);

    let (_, mem) = call(&app, "GET", "/api/v1/memory", None).await;
    let s = &mem["suggestions"][0];
    assert_eq!(s["source"], "merged", "{mem}");
    assert_eq!(
        s["rule"]["text"],
        "Add a regression test with every bug fix"
    );
    assert_eq!(s["evidence"].as_array().unwrap().len(), 3);
    assert_eq!(
        s["evidence"][0]["url"],
        "https://github.com/o/r/pull/20#discussion_r1"
    );
    assert_eq!(mem["learned"]["count"], 1);

    // "Not now" hides it.
    let key = s["key"].as_str().unwrap();
    let (st, _) = call(
        &app,
        "POST",
        "/api/v1/memory/suggestions/dismiss",
        Some(serde_json::json!({ "key": key })),
    )
    .await;
    assert!(st.is_success());
    let (_, mem) = call(&app, "GET", "/api/v1/memory", None).await;
    assert!(
        mem["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["source"] != "merged"),
        "{mem}"
    );
}
