//! B8 credentials: write-only PUT, keychain storage, no leaks in responses/logs/audit.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::io::Write;
use std::sync::{Arc, Mutex, Once};
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

const TOKEN: &str = "01234567890123456789012345678901";
const SECRET: &str = "sk-ant-test-secret-never-leak-abcdef0123456789";

static GLOBAL_TRACE: Once = Once::new();

/// The in-memory keychain and the provider env vars are process-wide: tests that store,
/// delete or read them take turns, or one test's cleanup races another's assertions.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A process-wide dispatch so HTTP trace callsites never register against `NoSubscriber`
/// when other tests run in parallel (interest is cached globally).
fn ensure_global_trace_dispatch() {
    GLOBAL_TRACE.call_once(|| {
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("trace"))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(std::io::sink),
            );
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

fn state() -> (axum::Router, tempfile::TempDir, tempfile::TempDir) {
    ensure_global_trace_dispatch();
    ferro_core::credentials::enable_mem_store_for_tests();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
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
    let last = Arc::new(Mutex::new(std::time::Instant::now()));
    (server::build_router(st, guard, last, true, None), dir, home)
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

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

async fn body_json(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let res = router.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn walk_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for ent in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = ent.path();
        if p.is_dir() {
            out.extend(walk_files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

struct TraceCapture {
    lines: Arc<Mutex<String>>,
    _guard: tracing::subscriber::DefaultGuard,
}

fn trace_logs() -> TraceCapture {
    let lines = Arc::new(Mutex::new(String::new()));
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("trace"))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer({
                    let lines = lines.clone();
                    move || {
                        struct W(Arc<Mutex<String>>);
                        impl Write for W {
                            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                                let n = b.len();
                                if let Ok(s) = std::str::from_utf8(b) {
                                    self.0.lock().unwrap().push_str(s);
                                }
                                Ok(n)
                            }
                            fn flush(&mut self) -> std::io::Result<()> {
                                Ok(())
                            }
                        }
                        W(lines.clone())
                    }
                }),
        );
    ensure_global_trace_dispatch();
    let guard = tracing::subscriber::set_default(subscriber);
    tracing::callsite::rebuild_interest_cache();
    TraceCapture {
        lines,
        _guard: guard,
    }
}

#[tokio::test]
async fn put_credentials_is_write_only_and_configures_ai() {
    let _serial = SERIAL.lock().await;
    for v in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
    ] {
        std::env::remove_var(v);
    }
    let (router, _dir, home) = state();
    let body =
        format!(r#"{{"credentials":[{{"id":"provider.anthropic.api_key","value":"{SECRET}"}}]}}"#,);
    let (status, resp) = body_json(router.clone(), put("/api/v1/credentials", &body)).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert!(!resp.contains(SECRET), "response leaked secret: {resp}");
    assert!(resp.contains("provider.anthropic.api_key"));

    for path in [
        "/api/v1/meta",
        "/api/v1/settings",
        "/api/v1/settings/schema",
        "/api/v1/ai/status",
    ] {
        let (st, text) = body_json(router.clone(), get(path)).await;
        assert_eq!(st, StatusCode::OK, "{path} {text}");
        assert!(!text.contains(SECRET), "{path} leaked: {text}");
    }
    let (_, ai_text) = body_json(router.clone(), get("/api/v1/ai/status")).await;
    let ai: serde_json::Value = serde_json::from_str(&ai_text).unwrap();
    let configured = ai["configured"].as_bool().unwrap_or(false);
    assert!(configured, "{ai}");

    let state_root = home.path().join("s");
    for file in walk_files(&state_root) {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        assert!(
            !text.contains(SECRET),
            "plaintext credential in {}",
            file.display()
        );
    }
    let (get_status, get_body) = body_json(router.clone(), get("/api/v1/credentials")).await;
    assert_ne!(get_status, StatusCode::OK, "{get_body}");
    assert!(!get_body.contains(SECRET), "GET leaked: {get_body}");

    let audit = state_root.join("audit.jsonl");
    assert!(audit.is_file(), "audit log was not written");
    let audit_text = std::fs::read_to_string(&audit).unwrap();
    assert!(!audit_text.contains(SECRET), "audit leaked: {audit_text}");
    // One line per PUT, in the B8a audit schema, naming the ids (never values).
    let lines: Vec<&str> = audit_text
        .lines()
        .filter(|l| l.contains("credentials"))
        .collect();
    assert_eq!(lines.len(), 1, "one audit line per PUT: {audit_text}");
    assert!(lines[0].contains("PUT /api/v1/credentials"), "{audit_text}");
    assert!(
        lines[0].contains("provider.anthropic.api_key"),
        "{audit_text}"
    );
}

#[tokio::test]
async fn malformed_bodies_never_echo_the_secret() {
    let _serial = SERIAL.lock().await;
    let (router, _dir, _home) = state();
    for body in [
        // A bare string where an item belongs: serde quotes the bad value in its message.
        format!(r#"{{"credentials":["{SECRET}"]}}"#),
        // id and value swapped by mistake: the unknown id must not be echoed either.
        format!(r#"{{"credentials":[{{"id":"{SECRET}","value":"provider.openai.api_key"}}]}}"#),
    ] {
        let (status, resp) = body_json(router.clone(), put("/api/v1/credentials", &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
        assert!(!resp.contains(SECRET), "error echoed the secret: {resp}");
    }
}

#[tokio::test]
async fn a_bad_id_later_in_the_list_stores_nothing() {
    let _serial = SERIAL.lock().await;
    let (router, _dir, _home) = state();
    let host = "partial-write.invalid";
    let body = format!(
        r#"{{"credentials":[{{"id":"forge.github:{host}","value":"{SECRET}"}},{{"id":"not.a.credential","value":"x"}}]}}"#
    );
    let (status, resp) = body_json(router, put("/api/v1/credentials", &body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    let id = ferro_core::credentials::CredentialId::GitHubToken { host: host.into() };
    assert_eq!(
        ferro_core::credentials::load(&id).unwrap(),
        None,
        "the first credential was stored before the bad id was rejected"
    );
}

#[tokio::test]
async fn stored_secret_never_appears_in_trace_logs() {
    let _serial = SERIAL.lock().await;
    std::env::remove_var("ANTHROPIC_API_KEY");
    let capture = trace_logs();
    let (router, _dir, _home) = state();
    let body =
        format!(r#"{{"credentials":[{{"id":"provider.openai.api_key","value":"{SECRET}"}}]}}"#,);
    let (status, resp) = body_json(router, put("/api/v1/credentials", &body)).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    let log = capture.lines.lock().unwrap().clone();
    assert!(
        log.contains("/api/v1/credentials"),
        "trace capture missed the request:\n{log}"
    );
    assert!(!log.contains(SECRET), "trace log leaked secret:\n{log}");
}

struct UnavailableGuard;

impl UnavailableGuard {
    fn arm() -> Self {
        ferro_core::credentials::force_keychain_unavailable_for_tests(true);
        Self
    }
}

impl Drop for UnavailableGuard {
    fn drop(&mut self) {
        ferro_core::credentials::force_keychain_unavailable_for_tests(false);
    }
}

#[tokio::test]
async fn empty_value_and_unknown_id_are_400() {
    let _serial = SERIAL.lock().await;
    let (router, _dir, _home) = state();
    let (status, resp) = body_json(
        router.clone(),
        put(
            "/api/v1/credentials",
            r#"{"credentials":[{"id":"provider.openai.api_key","value":"  "}]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    let (status, resp) = body_json(
        router,
        put(
            "/api/v1/credentials",
            r#"{"credentials":[{"id":"not.a.credential","value":"x"}]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
}

#[tokio::test]
async fn headless_keychain_returns_422_without_the_secret() {
    let _serial = SERIAL.lock().await;
    let _guard = UnavailableGuard::arm();
    let (router, _dir, home) = state();
    let body =
        format!(r#"{{"credentials":[{{"id":"provider.gemini.api_key","value":"{SECRET}"}}]}}"#,);
    let (status, resp) = body_json(router, put("/api/v1/credentials", &body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    let err: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(err["error"]["code"], "unsupported");
    let hint = err["error"]["detail"]["hint"].as_str().unwrap_or("");
    assert!(!hint.is_empty(), "{resp}");
    assert!(!resp.contains(SECRET), "error leaked secret: {resp}");
    let audit = home.path().join("s").join("audit.jsonl");
    if audit.exists() {
        let text = std::fs::read_to_string(&audit).unwrap();
        assert!(!text.contains(SECRET), "audit leaked: {text}");
    }
}

#[tokio::test]
async fn missing_provider_key_is_clear_not_empty_wire() {
    let _serial = SERIAL.lock().await;
    ferro_core::credentials::enable_mem_store_for_tests();
    for v in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"] {
        std::env::remove_var(v);
    }
    let id = ferro_core::credentials::CredentialId::AnthropicApiKey;
    let _ = ferro_core::credentials::store(&id, "x");
    ferro_core::credentials::delete(&id).unwrap();
    let err = ferro_agent::provider_v2::resolve_provider(None, None).unwrap_err();
    assert!(
        matches!(err, ferro_agent::provider::ProviderError::NoKey),
        "{err}"
    );
    let client = ferro_agent::provider_v2::make_client(&ferro_agent::provider_v2::ProviderSpec {
        kind: ferro_agent::provider_v2::ProviderKind::OpenAI,
        model: "m".into(),
        base_url: "http://127.0.0.1:9".into(),
    });
    assert!(client.is_err());
}
