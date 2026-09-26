//! B4 review flow: threads, drafts, submit, viewed, rounds over a mock.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn git(dir: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "{args:?}");
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Mock forge: GraphQL threads, comments, review submit, replies.
async fn mock_forge() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            let seen3 = seen2.clone();
            tokio::spawn(async move {
                let (rh, mut wh) = sock.into_split();
                let mut rd = tokio::io::BufReader::new(rh);
                loop {
                    let mut first = String::new();
                    if rd.read_line(&mut first).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let mut len = 0usize;
                    loop {
                        let mut line = String::new();
                        if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        if line.to_lowercase().starts_with("content-length:") {
                            len = line
                                .split(':')
                                .nth(1)
                                .unwrap_or("0")
                                .trim()
                                .parse()
                                .unwrap_or(0);
                        }
                    }
                    let mut raw = first.clone();
                    let mut body = vec![0u8; len];
                    if len > 0 {
                        if rd.read_exact(&mut body).await.is_err() {
                            return;
                        }
                        raw.push_str(&String::from_utf8_lossy(&body));
                    }
                    seen3.lock().unwrap().push(raw);
                    let resp_body = if first.contains("/graphql") {
                        serde_json::json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
                            "nodes": [{"id": "T1", "isResolved": false, "isOutdated": false,
                                "path": "a.txt", "line": 2, "startLine": null, "diffSide": "RIGHT",
                                "originalLine": 2, "commit": {"oid": "abc"},
                                "comments": {"nodes": [
                                    {"databaseId": 5, "author": {"login": "ann"}, "body": "first", "createdAt": "2026-01-01T00:00:00Z", "url": "https://x/1"},
                                    {"databaseId": 6, "author": {"login": "bob"}, "body": "second", "createdAt": "2026-01-02T00:00:00Z", "url": "https://x/2"},
                                ]}}],
                            "pageInfo": {"hasNextPage": false, "endCursor": null}}}}}})
                    } else if first.contains("/pulls/7/reviews") {
                        serde_json::json!({"id": 42, "html_url": "https://github.com/o/r/pull/7#review", "state": "COMMENTED"})
                    } else if first.contains("/replies") {
                        serde_json::json!({"databaseId": 7, "author": {"login": "me"}, "body": "reply!", "createdAt": "2026-01-03T00:00:00Z", "url": "https://x/3"})
                    } else if first.contains("POST") && first.contains("/comments") {
                        serde_json::json!({"databaseId": 8, "author": {"login": "me"}, "body": "top", "createdAt": "2026-01-03T00:00:00Z", "url": "https://x/4"})
                    } else if first.contains("/comments") {
                        serde_json::json!([{"databaseId": 9, "author": {"login": "ann"}, "body": "conv", "createdAt": "2026-01-01T00:00:00Z", "url": "https://x/5"}])
                    } else {
                        serde_json::json!({})
                    };
                    let bytes = serde_json::to_vec(&resp_body).unwrap();
                    let head = format!(
                        "HTTP/1.1 200 x\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                        bytes.len()
                    );
                    if wh.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    if wh.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (format!("http://{addr}"), seen)
}

fn authed(uri: &str, method: &str, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(Body::from(body.unwrap_or("").to_string())).unwrap()
}

async fn req(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let res = router.oneshot(authed(uri, method, body)).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, v)
}

/// Build an AppState already in PR mode (mock forge + local worktree).
fn pr_state(
    mock: &str,
    token: Option<&str>,
) -> (axum::Router, tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main", "."]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "init"]);
    let head = git_out(dir.path(), &["rev-parse", "HEAD"]);
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        dir.path().to_path_buf(),
        dirs.clone(),
        ferro_server::Host::Cli,
        "test".into(),
    );
    let pref = ferro_forge::ForgeRef {
        host: "github.com".into(),
        owner: "o".into(),
        repo: "r".into(),
        number: 7,
    };
    let store = ferro_forge::store::ReviewStore::new(home.path().join("reviews"));
    let meta = ferro_forge::github::PullMeta {
        title: "T".into(),
        body: Some("b".into()),
        state: "open".into(),
        merged: false,
        draft: false,
        base_ref: "main".into(),
        head_ref: "feature".into(),
        base_sha: head.clone(),
        head_sha: head.clone(),
        head_clone_url: None,
        is_fork: false,
        created_at: "".into(),
        updated_at: "".into(),
        additions: 1,
        deletions: 0,
        changed_files: 1,
        commits: 1,
        html_url: "https://github.com/o/r/pull/7".into(),
        author_login: "ann".into(),
        author_avatar: None,
    };
    let opened = ferro_forge::OpenedPr {
        dir: dir.path().to_path_buf(),
        base_ref: "main".into(),
        base_sha: head.clone(),
        head_sha: head.clone(),
        merge_base: head.clone(),
        reused: false,
    };
    let session = Arc::new(ferro_server::state::PrSession {
        pr_ref: pref,
        meta: parking_lot::RwLock::new(meta),
        github: Arc::new(ferro_forge::GitHub::new(
            format!("{mock}/api"),
            format!("{mock}/graphql"),
            token.map(|s| s.into()),
        )),
        token: token.map(|s| s.into()),
        token_source: None,
        worktree: parking_lot::RwLock::new(opened),
        store,
        checks: parking_lot::RwLock::new(None),
        can_push: parking_lot::RwLock::new(None),
        can_push_known: std::sync::atomic::AtomicBool::new(false),
    });
    let ws = ferro_server::state::Workspace::pr(dir.path().to_path_buf(), session, &dirs);
    st.ws.store(ws);
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    (server::build_router(st, guard, last, true, None), dir, home)
}

#[tokio::test]
async fn threads_and_conversation_render() {
    let (mock, _seen) = mock_forge().await;
    let (app, _d, _h) = pr_state(&mock, Some("tok"));
    let (s, v) = req(app, "GET", "/api/v1/pr/threads", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["threads"][0]["id"], "T1");
    assert_eq!(v["threads"][0]["line"], 2);
    assert_eq!(v["threads"][0]["side"], "RIGHT");
    assert_eq!(v["threads"][0]["comments"].as_array().unwrap().len(), 2);
    assert!(v["threads"][0]["comments"][0]["bodyHtml"]
        .as_str()
        .unwrap()
        .contains("first"));
    assert_eq!(v["conversation"][0]["body"], "conv");
}

#[tokio::test]
async fn drafts_crud_validate() {
    let (mock, _) = mock_forge().await;
    let (app, _d, _h) = pr_state(&mock, Some("tok"));
    let (s, _) = req(
        app.clone(),
        "POST",
        "/api/v1/review/drafts",
        Some(r#"{"path":"","line":1,"body":"x"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = req(
        app.clone(),
        "POST",
        "/api/v1/review/drafts",
        Some(r#"{"path":"a.txt","line":2,"body":"fix this"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["side"], "RIGHT");
    let (s, v) = req(app.clone(), "GET", "/api/v1/review/drafts", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["drafts"].as_array().unwrap().len(), 1);
    let (s, v) = req(
        app.clone(),
        "PATCH",
        &format!("/api/v1/review/drafts/{id}"),
        Some(r#"{"body":"fixed"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["body"], "fixed");
    let (s, _) = req(
        app.clone(),
        "DELETE",
        &format!("/api/v1/review/drafts/{id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = req(app, "DELETE", &format!("/api/v1/review/drafts/{id}"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn submit_pins_and_clears() {
    let (mock, seen) = mock_forge().await;
    let (app, _d, _h) = pr_state(&mock, Some("tok"));
    // Inline draft + reply draft against an unknown thread (fails, stays).
    let (s, _) = req(
        app.clone(),
        "POST",
        "/api/v1/review/drafts",
        Some(r#"{"path":"a.txt","line":1,"body":"inline"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = req(
        app.clone(),
        "POST",
        "/api/v1/review/drafts",
        Some(r#"{"path":"a.txt","line":1,"body":"r","threadId":"NOPE"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = req(
        app.clone(),
        "POST",
        "/api/v1/review/submit",
        Some(r#"{"event":"COMMENT","body":"ship"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["posted"], 1);
    assert_eq!(v["failed"].as_array().unwrap().len(), 1);
    assert_eq!(v["failed"][0]["error"], "thread gone");
    // Commit pinned to head sha in the recorded review call.
    let raw = seen.lock().unwrap().join("\n");
    assert!(raw.contains("\"commit_id\""), "{raw}");
    assert!(raw.contains("\"event\":\"COMMENT\""), "{raw}");
    let (s, v) = req(app.clone(), "GET", "/api/v1/review/rounds", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["rounds"][0]["kind"], "submitted");
}

#[tokio::test]
async fn submit_needs_token_and_validates() {
    let (mock, _) = mock_forge().await;
    let (app, _d, _h) = pr_state(&mock, None);
    let (s, v) = req(
        app,
        "POST",
        "/api/v1/review/submit",
        Some(r#"{"event":"COMMENT","body":"x"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
}

#[tokio::test]
async fn viewed_roundtrip() {
    let (mock, _) = mock_forge().await;
    let (app, _d, _h) = pr_state(&mock, Some("tok"));
    let (s, v) = req(
        app.clone(),
        "PUT",
        "/api/v1/review/viewed",
        Some(r#"{"path":"a.txt","viewed":true}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["viewed"].as_bool().unwrap());
    let (s, v) = req(app, "GET", "/api/v1/review/viewed", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["files"]["a.txt"]["viewed"].as_bool().unwrap());
}
