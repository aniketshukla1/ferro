//! B5 AI endpoint tests: status shape, ask validation, commit-message states,
//! findings stubs (API.md § 10). No provider keys: no network in CI.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";
const AI_KEYS: [&str; 7] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "GEMINI_API_KEY",
    "OLLAMA_MODEL",
];

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Removes AI provider env vars for hermetic tests; restores on drop.
struct NoProvider {
    saved: Vec<(String, Option<String>)>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl NoProvider {
    fn lock() -> Self {
        // Leak the guard lifetime: tests in this binary are the only env
        // mutators, serialized by ENV_LOCK.
        // A failed test must not poison the others' env handling.
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let guard: std::sync::MutexGuard<'static, ()> = unsafe { std::mem::transmute(guard) };
        let mut saved = Vec::new();
        for k in AI_KEYS {
            saved.push((k.to_string(), std::env::var(k).ok()));
            std::env::remove_var(k);
        }
        Self {
            saved,
            _guard: guard,
        }
    }
}

impl Drop for NoProvider {
    fn drop(&mut self) {
        for (k, v) in std::mem::take(&mut self.saved) {
            match v {
                Some(val) => std::env::set_var(&k, val),
                None => std::env::remove_var(&k),
            }
        }
    }
}

fn git(args: &[&str], dir: &std::path::Path) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "{args:?}");
}

fn router_over(root: std::path::PathBuf) -> axum::Router {
    app_over(root).0
}

fn app_over(root: std::path::PathBuf) -> (axum::Router, Arc<ferro_server::state::AppState>) {
    let home = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(root, dirs, ferro_server::Host::Cli, "test".into());
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        false,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    (
        server::build_router(st.clone(), guard, last, false, None),
        st,
    )
}

fn plain_state() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let dir = Box::leak(Box::new(dir));
    router_over(dir.path().to_path_buf())
}

fn git_state() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    git(&["init", "-b", "main"], dir.path());
    git(&["config", "user.email", "t@t"], dir.path());
    git(&["config", "user.name", "t"], dir.path());
    git(&["config", "commit.gpgsign", "false"], dir.path());
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    git(&["add", "."], dir.path());
    git(&["commit", "-m", "init"], dir.path());
    let dir = Box::leak(Box::new(dir));
    router_over(dir.path().to_path_buf())
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

async fn body_json(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test]
async fn status_shape_when_unconfigured() {
    let _np = NoProvider::lock();
    let (s, v) = body_json(
        plain_state()
            .oneshot(get("/api/v1/ai/status"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["configured"], false);
    assert!(v["provider"].is_null() && v["model"].is_null());
    let providers = v["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 5);
    for p in providers {
        assert!(p["id"].is_string() && p["label"].is_string());
        assert_eq!(p["configured"], false, "{}", p["id"]);
    }
    assert_eq!(providers[0]["defaultModel"], "claude-opus-5");
}

#[tokio::test]
async fn meta_lists_ai_features() {
    let (_s, v) = body_json(plain_state().oneshot(get("/api/v1/meta")).await.unwrap()).await;
    for f in ["ai", "ai.ask", "ai.review", "ai.commit", "symbols", "nav"] {
        assert!(
            v["features"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(f)),
            "{f}"
        );
    }
}

#[tokio::test]
async fn ask_rejects_empty_question() {
    let _np = NoProvider::lock();
    let (s, v) = body_json(
        plain_state()
            .oneshot(post("/api/v1/ai/ask", r#"{"question":"  "}"#))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn ask_without_provider_is_unsupported() {
    let _np = NoProvider::lock();
    let (s, v) = body_json(
        plain_state()
            .oneshot(post("/api/v1/ai/ask", r#"{"question":"what is this?"}"#))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "unsupported");
}

#[tokio::test]
async fn commit_message_needs_a_repo() {
    let _np = NoProvider::lock();
    let (s, v) = body_json(
        plain_state()
            .oneshot(post("/api/v1/git/commit-message", "{}"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "unsupported");
}

#[tokio::test]
async fn commit_message_conflicts_when_nothing_staged() {
    let _np = NoProvider::lock();
    let (s, v) = body_json(
        git_state()
            .oneshot(post("/api/v1/git/commit-message", "{}"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "conflict");
}

#[tokio::test]
async fn review_validates_scope_and_focus() {
    let _np = NoProvider::lock();
    let (s, v) = body_json(
        plain_state()
            .oneshot(post("/api/v1/ai/review", r#"{"scope":"bogus"}"#))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "bad_request");
    let (s, v) = body_json(
        plain_state()
            .oneshot(post(
                "/api/v1/ai/review",
                r#"{"scope":"changes","focus":["vibes"]}"#,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn review_without_provider_is_unsupported() {
    let _np = NoProvider::lock();
    let (s, v) = body_json(
        git_state()
            .oneshot(post("/api/v1/ai/review", r#"{"scope":"changes"}"#))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "unsupported");
}

fn seed_finding() -> ferro_forge::Finding {
    ferro_forge::Finding {
        id: "f_1".into(),
        head_sha: "abc".into(),
        path: "a.txt".into(),
        line: 1,
        start_line: None,
        side: "RIGHT".into(),
        severity: "high".into(),
        category: "bug".into(),
        title: "off by one".into(),
        body: "detail".into(),
        suggestion: None,
        confidence: 0.9,
        created_at: "2026-09-26T00:00:00Z".into(),
        dismissed: false,
        dismiss_reason: None,
    }
}

fn git_app() -> (
    axum::Router,
    Arc<ferro_server::state::AppState>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    git(&["init", "-b", "main"], dir.path());
    git(&["config", "user.email", "t@t"], dir.path());
    git(&["config", "user.name", "t"], dir.path());
    git(&["config", "commit.gpgsign", "false"], dir.path());
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    git(&["add", "."], dir.path());
    git(&["commit", "-m", "init"], dir.path());
    let (router, st) = app_over(dir.path().to_path_buf());
    // Keep the tempdir alive: the router holds the root path; the dir
    // itself must outlive the test — return it.
    (router, st, dir)
}

#[tokio::test]
async fn findings_accept_and_dismiss_roundtrip() {
    let (router, st, _dir) = git_app();
    let ws = st.ws();
    let store = ferro_forge::ReviewStore::new(st.dirs.workspace_state_dir(&ws.key).join("review"));
    store.save_findings("abc", &[seed_finding()]).unwrap();

    let (s, v) = body_json(
        router
            .clone()
            .oneshot(post(
                "/api/v1/ai/findings/f_1/accept",
                r#"{"body":"please fix"}"#,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["source"], "ai");
    assert_eq!(v["findingId"], "f_1");
    assert_eq!(v["body"], "please fix");

    let (s, _) = body_json(
        router
            .clone()
            .oneshot(post(
                "/api/v1/ai/findings/f_1/dismiss",
                r#"{"reason":"wontfix"}"#,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Dismissed findings no longer accept.
    let (s, v) = body_json(
        router
            .clone()
            .oneshot(post("/api/v1/ai/findings/f_1/accept", "{}"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");

    let (s, _) = body_json(
        router
            .oneshot(post("/api/v1/ai/findings/f_nope/dismiss", "{}"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

// -- against a mock Anthropic endpoint ------------------------------------------

/// Local `/v1/messages` speaking SSE: scripted responses in order (the last
/// one repeats), every request body recorded. No network.
struct MockAnthropic {
    url: String,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockAnthropic {
    async fn start(scripts: Vec<(u16, String)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let scripts = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
            scripts,
        )));
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let b2 = bodies.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let (scripts, bodies) = (scripts.clone(), b2.clone());
                tokio::spawn(async move {
                    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
                    let (rh, mut wh) = sock.into_split();
                    let mut rd = tokio::io::BufReader::new(rh);
                    loop {
                        let mut len = 0usize;
                        let mut first = true;
                        loop {
                            let mut line = String::new();
                            if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == "\r\n" && !first {
                                break;
                            }
                            first = false;
                            if line.to_lowercase().starts_with("content-length:") {
                                len = line[15..].trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0u8; len];
                        if len > 0 && rd.read_exact(&mut body).await.is_err() {
                            return;
                        }
                        bodies
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&body).into_owned());
                        let (status, text) = {
                            let mut s = scripts.lock().unwrap();
                            if s.len() > 1 {
                                s.pop_front().unwrap()
                            } else {
                                s.front().cloned().unwrap_or((500, String::new()))
                            }
                        };
                        let head = format!(
                            "HTTP/1.1 {status} x\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                            text.len()
                        );
                        if wh.write_all(head.as_bytes()).await.is_err()
                            || wh.write_all(text.as_bytes()).await.is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        Self {
            url: format!("http://{addr}"),
            bodies,
        }
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }
}

/// One assistant text turn ending the conversation.
fn text_turn(text: &str) -> String {
    [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#.to_string(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#.into(),
        serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}).to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#.into(),
        r#"{"type":"message_stop"}"#.into(),
    ]
    .iter()
    .map(|e| format!("event: x\ndata: {e}\n\n"))
    .collect()
}

/// Provider env pointing at the mock, restored on drop (serialized with the
/// other env-touching tests by NoProvider's lock).
struct WithAnthropic(#[allow(dead_code)] NoProvider);

impl WithAnthropic {
    fn at(url: &str) -> Self {
        let np = NoProvider::lock();
        std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        std::env::set_var("ANTHROPIC_BASE_URL", url);
        Self(np)
    }
}

/// A repo with one committed file, for ask/review/commit-message runs.
fn repo_with(
    files: &[(&str, &str)],
) -> (
    axum::Router,
    Arc<ferro_server::state::AppState>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    git(&["init", "-b", "main"], dir.path());
    git(&["config", "user.email", "t@t"], dir.path());
    git(&["config", "user.name", "t"], dir.path());
    git(&["config", "commit.gpgsign", "false"], dir.path());
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    git(&["add", "."], dir.path());
    git(&["commit", "-m", "init"], dir.path());
    for (p, c) in files {
        let full = dir.path().join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, c).unwrap();
    }
    let (router, st) = app_over(dir.path().to_path_buf());
    (router, st, dir)
}

async fn sse_text(res: axum::response::Response) -> String {
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024),
    )
    .await
    .expect("the ask stream never ended")
    .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn ask_stream_ends_and_keeps_the_conversation() {
    let mock = MockAnthropic::start(vec![(200, text_turn("hello there"))]).await;
    let _env = WithAnthropic::at(&mock.url);
    let (router, _st, _dir) = repo_with(&[]);
    let res = router
        .clone()
        .oneshot(post("/api/v1/ai/ask", r#"{"question":"first question"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let text = sse_text(res).await;
    assert!(text.contains("event: final"), "{text}");
    // After the answer: usage, and the stream closes (no hang).
    assert!(text.contains("event: usage"), "{text}");
    let conv = text
        .split("\"conversationId\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .to_string();
    // The follow-up carries the first turn to the provider.
    let res = router
        .oneshot(post(
            "/api/v1/ai/ask",
            &serde_json::json!({"question": "follow up", "conversationId": conv}).to_string(),
        ))
        .await
        .unwrap();
    let _ = sse_text(res).await;
    let second = mock.bodies().last().cloned().unwrap();
    assert!(
        second.contains("first question") && second.contains("hello there"),
        "{second}"
    );
}

#[tokio::test]
async fn ask_context_cut_is_char_safe() {
    // A context window whose 12,000th byte falls inside a multi-byte char.
    let line = "é".repeat(40);
    let mut content = String::new();
    for _ in 0..200 {
        content.push_str(&line);
        content.push('\n');
    }
    let mock = MockAnthropic::start(vec![(200, text_turn("ok"))]).await;
    let _env = WithAnthropic::at(&mock.url);
    let mut found = false;
    for pad in 0..3 {
        let body = format!("{}{content}", "x".repeat(pad));
        let (router, _st, _dir) = repo_with(&[("ctx.txt", &body)]);
        let res = router
            .oneshot(post(
                "/api/v1/ai/ask",
                r#"{"question":"q","context":{"path":"ctx.txt","startLine":1,"endLine":200}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "pad {pad}");
        let text = sse_text(res).await;
        assert!(text.contains("event: final"), "pad {pad}: {text}");
        found = true;
    }
    assert!(found);
}

#[tokio::test]
async fn commit_message_never_sends_never_send_files() {
    let mock = MockAnthropic::start(vec![(200, text_turn("feat: add config"))]).await;
    let _env = WithAnthropic::at(&mock.url);
    let (router, _st, dir) = repo_with(&[
        (".env", "DB_PASSWORD=hunter2-secret\n"),
        ("src/app.rs", "fn app() {}\n"),
    ]);
    git(&["add", "-f", ".env", "src/app.rs"], dir.path());
    let (s, v) = body_json(
        router
            .oneshot(post("/api/v1/git/commit-message", "{}"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["message"], "feat: add config");
    let sent = mock.bodies().join("\n");
    assert!(sent.contains("fn app()"), "{sent}");
    assert!(!sent.contains("hunter2-secret"), "{sent}");
}

async fn run_review(router: axum::Router) -> serde_json::Value {
    let (s, v) = body_json(
        router
            .clone()
            .oneshot(post("/api/v1/ai/review", r#"{"scope":"changes"}"#))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["job"]["id"].as_str().unwrap().to_string();
    for _ in 0..100 {
        let (_, j) = body_json(
            router
                .clone()
                .oneshot(get(&format!("/api/v1/jobs/{id}")))
                .await
                .unwrap(),
        )
        .await;
        if ["done", "failed", "cancelled"].contains(&j["state"].as_str().unwrap_or("")) {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("review job never finished");
}

#[tokio::test]
async fn review_prompt_is_redacted() {
    let mock = MockAnthropic::start(vec![(200, text_turn("looks fine"))]).await;
    let _env = WithAnthropic::at(&mock.url);
    let (router, _st, _dir) = repo_with(&[("a.txt", "one\nkey = \"AKIAIOSFODNN7EXAMPLE\"\n")]);
    let j = run_review(router).await;
    assert_eq!(j["state"], "done", "{j}");
    let sent = mock.bodies().join("\n");
    assert!(sent.contains("[REDACTED]"), "{sent}");
    assert!(!sent.contains("AKIAIOSFODNN7EXAMPLE"), "{sent}");
}

#[tokio::test]
async fn review_that_never_reached_the_model_fails() {
    // Every provider call is refused: that is not a clean review.
    let mock = MockAnthropic::start(vec![(
        400,
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"model: unknown"}}"#
            .into(),
    )])
    .await;
    let _env = WithAnthropic::at(&mock.url);
    let (router, _st, _dir) = repo_with(&[("a.txt", "one\ntwo\n")]);
    let j = run_review(router).await;
    assert_eq!(j["state"], "failed", "{j}");
    assert!(
        j["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("model: unknown"),
        "{j}"
    );
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct PrApp {
    router: axum::Router,
    st: Arc<ferro_server::state::AppState>,
    session: Arc<ferro_server::state::PrSession>,
    dir: tempfile::TempDir,
    _home: tempfile::TempDir,
}

/// PR mode over a local checkout: `merge_base` (base.txt + a.txt) → the PR
/// commit (edits a.txt) is checked out; the base branch moved on since
/// (edits base_only.txt), and the poll already saw a newer, unfetched push.
fn pr_app() -> PrApp {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    git(&["init", "-b", "main"], d);
    git(&["config", "user.email", "t@t"], d);
    git(&["config", "user.name", "t"], d);
    git(&["config", "commit.gpgsign", "false"], d);
    std::fs::write(d.join("a.txt"), "one\n").unwrap();
    std::fs::write(d.join("base_only.txt"), "base\n").unwrap();
    git(&["add", "."], d);
    git(&["commit", "-m", "init"], d);
    let merge_base = git_out(d, &["rev-parse", "HEAD"]);
    git(&["checkout", "-qb", "base-tip"], d);
    std::fs::write(d.join("base_only.txt"), "base moved on\n").unwrap();
    git(&["commit", "-qam", "base moves"], d);
    let base_tip = git_out(d, &["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "main"], d);
    std::fs::write(d.join("a.txt"), "one\npr line\n").unwrap();
    git(&["commit", "-qam", "pr"], d);
    let head = git_out(d, &["rev-parse", "HEAD"]);
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        d.to_path_buf(),
        dirs.clone(),
        ferro_server::Host::Cli,
        "test".into(),
    );
    let meta = ferro_forge::github::PullMeta {
        title: "T".into(),
        body: None,
        state: "open".into(),
        merged: false,
        draft: false,
        base_ref: "main".into(),
        head_ref: "feature".into(),
        base_sha: base_tip,
        head_sha: "f".repeat(40),
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
    let session = Arc::new(ferro_server::state::PrSession {
        pr_ref: ferro_forge::parse_pr_url("https://github.com/o/r/pull/7").unwrap(),
        meta: parking_lot::RwLock::new(meta),
        client: Arc::new(ferro_forge::ForgeClient::GitHub(ferro_forge::GitHub::new(
            "http://127.0.0.1:9/api".into(),
            "http://127.0.0.1:9/graphql".into(),
            None,
        ))),
        token: None,
        token_source: None,
        worktree: parking_lot::RwLock::new(ferro_forge::OpenedPr {
            dir: d.to_path_buf(),
            base_ref: "main".into(),
            base_sha: merge_base.clone(),
            head_sha: head,
            merge_base,
            reused: false,
            remote: "origin".into(),
        }),
        store: ferro_forge::ReviewStore::new(home.path().join("reviews")),
        checks: parking_lot::RwLock::new(None),
        can_push: parking_lot::RwLock::new(None),
        can_push_known: std::sync::atomic::AtomicBool::new(false),
    });
    st.ws.store(ferro_server::state::Workspace::pr(
        d.to_path_buf(),
        session.clone(),
        &dirs,
    ));
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        false,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    PrApp {
        router: server::build_router(st.clone(), guard, last, true, None),
        st,
        session,
        dir,
        _home: home,
    }
}

#[tokio::test]
async fn pr_review_covers_the_checked_out_changes_only() {
    let mock = MockAnthropic::start(vec![(200, text_turn("reviewed"))]).await;
    let _env = WithAnthropic::at(&mock.url);
    let app = pr_app();
    let (s, v) = body_json(
        app.router
            .clone()
            .oneshot(post("/api/v1/ai/review", r#"{"scope":"pr"}"#))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["job"]["id"].as_str().unwrap().to_string();
    let mut job = serde_json::Value::Null;
    for _ in 0..100 {
        let (_, j) = body_json(
            app.router
                .clone()
                .oneshot(get(&format!("/api/v1/jobs/{id}")))
                .await
                .unwrap(),
        )
        .await;
        if ["done", "failed"].contains(&j["state"].as_str().unwrap_or("")) {
            job = j;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(job["state"], "done", "{job}");
    // What the PR changed, as the reviewer sees it — not the base branch's
    // own later commits, and at the head that is checked out.
    let sent = mock.bodies().join("\n");
    assert!(sent.contains("pr line"), "{sent}");
    assert!(!sent.contains("base_only.txt"), "{sent}");
    let head = app.session.worktree.read().head_sha.clone();
    assert!(
        !app.session.store.findings(Some(&head)).is_empty()
            || job["result"]["findings"].as_array().is_some()
    );
    let _ = &app.dir;
}

#[tokio::test]
async fn accepting_a_finding_publishes_every_draft() {
    let app = pr_app();
    let mut rx = app.st.bus.subscribe();
    let (s, _) = body_json(
        app.router
            .clone()
            .oneshot(post(
                "/api/v1/review/drafts",
                r#"{"path":"a.txt","line":1,"body":"mine"}"#,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let mut f = seed_finding();
    f.line = 2;
    app.session.store.save_findings("abc", &[f]).unwrap();
    let (s, v) = body_json(
        app.router
            .clone()
            .oneshot(post("/api/v1/ai/findings/f_1/accept", "{}"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // API.md § 12: `drafts` carries the whole list (every tab syncs from it).
    let mut last = None;
    while let Ok(Ok((_, ev))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
    {
        if let ferro_server::bus::ServerEvent::Drafts { drafts } = ev {
            last = Some(drafts);
        }
    }
    let drafts = last.expect("drafts event");
    assert_eq!(drafts.as_array().unwrap().len(), 2, "{drafts}");
}

#[tokio::test]
async fn review_guidance_cut_is_char_safe() {
    // Three guidance files of 8,000 chars with multi-byte text: the 20 KB
    // cut lands inside a character.
    let doc = "é".repeat(8000);
    let mock = MockAnthropic::start(vec![(200, text_turn("fine"))]).await;
    let _env = WithAnthropic::at(&mock.url);
    let (router, _st, _dir) = repo_with(&[
        ("AGENTS.md", &doc),
        ("CLAUDE.md", &doc),
        ("CONTRIBUTING.md", &doc),
        ("a.txt", "one\ntwo\n"),
    ]);
    let j = run_review(router).await;
    assert_eq!(j["state"], "done", "{j}");
}

#[tokio::test]
async fn explain_tags_hunks_caches_and_withholds_never_send() {
    let (router, _st, dir) = repo_with(&[]);
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(dir.path().join(".env"), "TOKEN=hunter2-secret\n").unwrap();
    let (_, d) = body_json(
        router
            .clone()
            .oneshot(get("/api/v1/git/diff?path=a.txt&base=HEAD&target=worktree"))
            .await
            .unwrap(),
    )
    .await;
    let id = d["hunks"][0]["id"].as_str().unwrap().to_string();
    let answer = serde_json::json!({
        "summary": "Adds a second line.",
        "hunks": [{ "id": id, "note": "Appends the word two." }, { "id": "bogus", "note": "ignored" }],
    });
    let mock = MockAnthropic::start(vec![(
        200,
        text_turn(&format!("Here you go:\n```json\n{answer}\n```")),
    )])
    .await;
    let _env = WithAnthropic::at(&mock.url);
    let body = r#"{"path":"a.txt","base":"HEAD","target":"worktree"}"#;
    let (s, v) = body_json(
        router
            .clone()
            .oneshot(post("/api/v1/ai/explain", body))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["summary"], "Adds a second line.");
    assert_eq!(
        v["hunks"].as_array().unwrap().len(),
        1,
        "unknown ids are dropped: {v}"
    );
    assert_eq!(
        v["hunks"][0]["kind"], "added",
        "kind comes from the rows, not the model"
    );
    assert_eq!(v["hunks"][0]["note"], "Appends the word two.");
    assert_eq!(v["cached"], false);
    assert!(
        mock.bodies()[0].contains("+two"),
        "the diff reaches the prompt"
    );
    // Same diff again: from the cache, no second provider call.
    let (_, v) = body_json(
        router
            .clone()
            .oneshot(post("/api/v1/ai/explain", body))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["cached"], true);
    assert_eq!(mock.bodies().len(), 1);
    // ai.neverSend files are refused before anything is sent.
    let (s, _) = body_json(
        router
            .oneshot(post(
                "/api/v1/ai/explain",
                r#"{"path":".env","base":"HEAD","target":"worktree"}"#,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!mock.bodies().join("\n").contains("hunter2-secret"));
}

#[tokio::test]
async fn explain_gives_verdicts_and_suggestions_that_apply() {
    let (router, _st, dir) = repo_with(&[]);
    std::fs::write(dir.path().join("a.txt"), "one\nlet ttl = 86400;\n").unwrap();
    std::fs::write(dir.path().join("k.txt"), "key\n").unwrap();
    git(&["add", "k.txt"], dir.path());
    git(&["commit", "-m", "k"], dir.path());
    std::fs::write(
        dir.path().join("k.txt"),
        "key\ntoken = ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    )
    .unwrap();
    let hunk_id = |path: &str| {
        let router = router.clone();
        let uri = format!("/api/v1/git/diff?path={path}&base=HEAD&target=worktree");
        async move {
            let (_, d) = body_json(router.oneshot(get(&uri)).await.unwrap()).await;
            d["hunks"][0]["id"].as_str().unwrap().to_string()
        }
    };
    let (a, k) = (hunk_id("a.txt").await, hunk_id("k.txt").await);
    let better = "one\nlet ttl = SECS_PER_DAY; // one day";
    let answers = [
        serde_json::json!({ "summary": "Adds a TTL.", "hunks": [
            { "id": a, "note": "Sets a one-day TTL.", "verdict": "improve", "why": "Name the constant.", "code": format!("```\n{better}\n```") },
        ] }),
        serde_json::json!({ "summary": "Adds a token.", "hunks": [
            { "id": k, "note": "Adds a token.", "verdict": "problem", "why": "Do not commit tokens.", "code": "key\ntoken = env(\"TOKEN\")" },
        ] }),
    ];
    let mock = MockAnthropic::start(
        answers
            .iter()
            .map(|x| (200, text_turn(&x.to_string())))
            .collect(),
    )
    .await;
    let _env = WithAnthropic::at(&mock.url);
    let explain = |path: &str| {
        let router = router.clone();
        let body = format!(r#"{{"path":"{path}","base":"HEAD","target":"worktree"}}"#);
        async move {
            body_json(
                router
                    .oneshot(post("/api/v1/ai/explain", &body))
                    .await
                    .unwrap(),
            )
            .await
        }
    };
    let (s, v) = explain("a.txt").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let h = &v["hunks"][0];
    assert_eq!(
        (h["verdict"].clone(), h["why"].clone()),
        (
            serde_json::json!("improve"),
            serde_json::json!("Name the constant.")
        )
    );
    let sg = &h["suggestion"];
    assert_eq!(
        (sg["start"].clone(), sg["end"].clone()),
        (serde_json::json!(1), serde_json::json!(2)),
        "{h}"
    );
    assert_eq!(sg["original"], "one\nlet ttl = 86400;");
    assert_eq!(sg["code"], better, "the code fence is stripped");
    assert!(
        mock.bodies()[0].contains("verdict"),
        "the prompt asks for verdicts"
    );

    // The suggestion applies through the inline-edit endpoint, exactly over its lines.
    let edit = serde_json::json!({ "path": "a.txt", "startLine": 1, "endLine": 2, "expected": sg["original"], "text": sg["code"] });
    let (s, e) = body_json(
        router
            .clone()
            .oneshot(post("/api/v1/file/edit", &edit.to_string()))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{e}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        format!("{better}\n")
    );

    // Over a line that holds a secret: the verdict stays, the code does not (it would be redacted).
    let (_, v) = explain("k.txt").await;
    let h = &v["hunks"][0];
    assert_eq!(h["verdict"], "problem");
    assert!(h.get("suggestion").is_none(), "{h}");
    assert!(
        !mock.bodies().join("\n").contains("ghp_aaaa"),
        "secrets are redacted before sending"
    );
}

/// One assistant turn that reports findings through the review tool, then stops for tool results.
fn findings_turn(findings: &[serde_json::Value]) -> String {
    let mut events =
        vec![r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#.to_string()];
    for (i, f) in findings.iter().enumerate() {
        events.push(serde_json::json!({"type":"content_block_start","index":i,"content_block":{"type":"tool_use","id":format!("t{i}"),"name":"report_finding"}}).to_string());
        events.push(serde_json::json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":f.to_string()}}).to_string());
        events.push(serde_json::json!({"type":"content_block_stop","index":i}).to_string());
    }
    events.push(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#.into());
    events.push(r#"{"type":"message_stop"}"#.into());
    events
        .iter()
        .map(|e| format!("event: x\ndata: {e}\n\n"))
        .collect()
}

#[tokio::test]
async fn team_memory_steers_and_filters_the_ai_review() {
    let f = |category: &str, title: &str| {
        serde_json::json!({
            "path": "a.txt", "line": 2, "side": "RIGHT", "severity": "medium", "category": category,
            "title": title, "body": "detail", "confidence": 0.9,
        })
    };
    let mock = MockAnthropic::start(vec![
        (
            200,
            findings_turn(&[
                f("style", "Magic number 42"),
                f("bug", "Off by one in the loop bound"),
            ]),
        ),
        (200, text_turn("done")),
    ])
    .await;
    let _env = WithAnthropic::at(&mock.url);
    let (router, _st, _dir) = repo_with(&[("a.txt", "one\nlimit = 42\n")]);
    let (s, v) = body_json(
        router
            .clone()
            .oneshot(post(
                "/api/v1/memory/rules",
                r#"{"kind":"ignore","appliesTo":"ai","category":"style","title":"Magic number","reason":"constants are fine in config"}"#,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, _) = body_json(
        router
            .clone()
            .oneshot(post(
                "/api/v1/memory/rules",
                r#"{"kind":"convention","appliesTo":"ai","text":"Every loop bound is checked for off-by-one"}"#,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let j = run_review(router).await;
    assert_eq!(j["state"], "done", "{j}");
    let r = &j["result"];
    let titles: Vec<&str> = r["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["title"].as_str())
        .collect();
    assert_eq!(titles, vec!["Off by one in the loop bound"], "{r}");
    assert_eq!(r["suppressed"][0]["title"], "Magic number 42");
    assert_eq!(r["suppressed"][0]["suppressedBy"]["scope"], "personal");
    // The reviewer was told the team's conventions and what not to report.
    let sent = mock.bodies()[0].clone();
    assert!(
        sent.contains("Every loop bound is checked for off-by-one"),
        "{sent}"
    );
    assert!(
        sent.contains("Do not report these") && sent.contains("Magic number"),
        "{sent}"
    );
}
