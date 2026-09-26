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
        let guard = ENV_LOCK.lock().unwrap();
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
