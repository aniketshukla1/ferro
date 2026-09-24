use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{delete, get, post},
    Extension, Json, Router,
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use std::sync::Arc;
use tokio_stream::StreamExt as _;
use tower_http::trace::TraceLayer;

use ferro_core::Index;

#[derive(Debug, Clone)]
pub struct ServeOpts {
    pub host: String,
    pub port: u16,
    pub no_git: bool,
    pub narrate: bool,
    pub no_open: bool,
    /// File to open on boot with 1-based line.
    pub initial: Option<(String, usize)>,
    pub token: Option<String>,
    pub allow_hosts: Vec<String>,
    pub no_auth: bool,
}

#[derive(RustEmbed, Clone)]
#[folder = "../../web/"]
struct Web;

pub fn router(
    state: Arc<Index>,
    guard: Arc<crate::guard::GuardConfig>,
    last_active: Arc<std::sync::Mutex<std::time::Instant>>,
    no_git: bool,
) -> Router {
    let mut app = Router::new()
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/files", get(files))
        .route("/api/fuzzy", get(fuzzy))
        .route("/api/search", get(search))
        .route("/api/file", get(read_file))
        .route("/api/file-meta", get(file_meta))
        .route("/api/file-window", get(file_window))
        .route("/api/highlight", get(highlight))
        .route("/api/ask", post(ask))
        .route("/api/ask/stream", post(ask_stream));
    if !no_git {
        app = app
            .route("/api/git-status", get(git_status))
            .route("/api/diff", get(diff));
    }
    app = app.route("/api/pr-info", get(pr_info));
    app = app
        .route("/api/markdown", get(markdown))
        .route("/api/raw", get(raw));
    app = app
        .route("/api/review/drafts", get(review_list).post(review_add))
        .route("/api/review/drafts/{id}", delete(review_delete))
        .route("/api/review/submit", post(review_submit))
        .route("/api/review/apply", post(review_apply))
        .route("/api/settings", get(settings_get).put(settings_put));
    if !no_git {
        app = app
            .route("/api/git/stage", post(git_stage))
            .route("/api/git/unstage", post(git_unstage))
            .route("/api/git/commit", post(git_commit))
            .route("/api/git/commit-message", post(git_commit_message))
            .route("/api/git/push", post(git_push))
            .route("/api/git/pull", post(git_pull));
    }
    app.fallback(static_file)
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(track_activity))
        .layer(Extension(last_active))
        .layer(middleware::from_fn(crate::guard::guards))
        .layer(Extension(guard))
        .with_state(state)
}

pub async fn serve(state: Arc<Index>, o: ServeOpts) {
    let last_active = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", o.host, o.port))
        .await
        .expect("bind");
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(o.port);
    let guard = Arc::new(crate::guard::GuardConfig::new(
        o.token.clone(),
        bound,
        o.allow_hosts.clone(),
        o.no_auth,
    ));
    let app = router(state.clone(), guard.clone(), last_active.clone(), o.no_git);

    // Idle scavenger (px0 parity): after 15s without requests, drop the
    // highlight LRU so an idle tab settles back down.
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tick.tick().await;
            let idle = last_active.lock().map(|t| t.elapsed()).unwrap_or_default();
            if idle >= std::time::Duration::from_secs(15) {
                ferro_core::highlight::clear_cache();
                tracing::debug!("ferro idle {}s: highlight cache cleared", idle.as_secs());
            }
        }
    });

    let host_out = if o.host == "0.0.0.0" {
        "127.0.0.1"
    } else {
        o.host.as_str()
    };
    let mut url = format!("http://{}:{}/?token={}", host_out, bound, guard.token);
    if let Some((f, l)) = &o.initial {
        url = format!(
            "http://{}:{}/?token={}&file={}&line={}",
            host_out,
            bound,
            guard.token,
            encode_qs(f),
            l
        );
    }
    if o.no_auth {
        tracing::warn!("--no-auth: API auth disabled (loopback only)");
    }
    if o.narrate {
        tracing::info!("ferro serving {} on {}", state.root().display(), url);
        println!("ferro {}", url);
    }

    if !o.no_open {
        let _ = open::that(url);
    }

    axum::serve(listener, app).await.expect("serve");
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status":"ok","service":"ferro"}))
}

async fn track_activity(
    Extension(last): Extension<Arc<std::sync::Mutex<std::time::Instant>>>,
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> impl IntoResponse {
    if let Ok(mut t) = last.lock() {
        *t = std::time::Instant::now();
    }
    next.run(req).await
}

async fn stats(State(s): State<Arc<Index>>) -> impl IntoResponse {
    let (n, ms) = s.stats();
    Json(serde_json::json!({"files": n, "indexed_ms": ms, "root": s.root().to_string_lossy()}))
}

async fn files(State(s): State<Arc<Index>>) -> impl IntoResponse {
    Json(s.snapshot())
}

#[derive(Deserialize)]
struct Q {
    q: Option<String>,
    limit: Option<usize>,
}

async fn fuzzy(State(s): State<Arc<Index>>, Query(q): Query<Q>) -> impl IntoResponse {
    let query = q.q.unwrap_or_default();
    let limit = q.limit.unwrap_or(50).min(200);
    let snap = s.snapshot();
    let paths: Vec<String> = snap.into_iter().map(|f| f.path).collect();
    let ranked = ferro_core::fuzzy::rank(&query, &paths, limit);
    Json(
        ranked
            .into_iter()
            .map(|(p, sc)| serde_json::json!({"path": p, "score": sc}))
            .collect::<Vec<_>>(),
    )
}

async fn search(State(s): State<Arc<Index>>, Query(q): Query<Q>) -> impl IntoResponse {
    let query = q.q.unwrap_or_default();
    let limit = q.limit.unwrap_or(50).min(200);
    let root = s.root().to_path_buf();
    let hits = tokio::task::spawn_blocking(move || ferro_core::search::grep(&root, &query, limit))
        .await
        .unwrap_or_default();
    Json(hits)
}

#[derive(Deserialize)]
struct FileQ {
    path: String,
}

#[derive(Deserialize)]
struct WindowQ {
    path: String,
    start: Option<usize>,
    count: Option<usize>,
}

async fn file_meta(State(s): State<Arc<Index>>, Query(q): Query<FileQ>) -> impl IntoResponse {
    match s.file_meta(&q.path) {
        Some(m) => {
            Json(serde_json::json!({"size": m.size, "total_lines": m.total_lines})).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found".to_string()).into_response(),
    }
}

async fn file_window(State(s): State<Arc<Index>>, Query(q): Query<WindowQ>) -> impl IntoResponse {
    let start = q.start.unwrap_or(0);
    let count = q.count.unwrap_or(200).clamp(1, 2000);
    let s2 = s.clone();
    let path = q.path.clone();
    let out = tokio::task::spawn_blocking(move || s2.read_window(&path, start, count))
        .await
        .unwrap_or(None);
    match out {
        Some(w) => Json(w).into_response(),
        None => (StatusCode::NOT_FOUND, "not found".to_string()).into_response(),
    }
}

async fn highlight(State(s): State<Arc<Index>>, Query(q): Query<WindowQ>) -> impl IntoResponse {
    let start = q.start.unwrap_or(0);
    let count = q.count.unwrap_or(200).clamp(1, 1000);
    let s2 = s.clone();
    let path = q.path.clone();
    let out = tokio::task::spawn_blocking(move || s2.highlight_window(&path, start, count))
        .await
        .unwrap_or(None);
    match out {
        Some(w) => Json(w).into_response(),
        None => (StatusCode::NOT_FOUND, "not found".to_string()).into_response(),
    }
}

async fn read_file(State(s): State<Arc<Index>>, Query(q): Query<FileQ>) -> impl IntoResponse {
    let Some(p) = s.safe_join(&q.path) else {
        return (StatusCode::FORBIDDEN, "traversal blocked".to_string()).into_response();
    };
    match tokio::fs::read_to_string(&p).await {
        Ok(t) => {
            let out = if t.len() > 512 * 1024 {
                t[..512 * 1024].to_string()
            } else {
                t
            };
            (StatusCode::OK, out).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "not found".to_string()).into_response(),
    }
}

async fn git_status(State(s): State<Arc<Index>>) -> impl IntoResponse {
    let root = s.root().to_path_buf();
    let out = tokio::task::spawn_blocking(move || ferro_core::git::status(&root))
        .await
        .unwrap_or_default();
    (StatusCode::OK, out).into_response()
}

#[derive(Deserialize)]
struct DiffQ {
    path: Option<String>,
}

#[derive(Deserialize)]
struct AskBody {
    question: String,
    max_steps: Option<usize>,
}

/// POST /api/ask — read-only agent over the live index.
/// Provider resolves from server env (GEMINI_API_KEY / OPENAI_API_KEY / OLLAMA_MODEL).
/// Never accepts keys in the request body.
async fn ask(State(s): State<Arc<Index>>, Json(b): Json<AskBody>) -> impl IntoResponse {
    if b.question.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "empty question".to_string()).into_response();
    }
    let provider = match ferro_agent::OpenAiCompat::from_env(None, None, None) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, format!("no provider: {e}")).into_response()
        }
    };
    let sandbox = ferro_agent::Sandbox::readonly(s.root().to_path_buf());
    let agent = ferro_agent::Agent {
        index: s.clone(),
        sandbox,
        client: Arc::new(provider),
        max_steps: b.max_steps.unwrap_or(8).clamp(1, 16),
    };
    let t = agent.run(&b.question).await;
    let id = ferro_agent::new_id();
    let session = ferro_agent::log_ask(s.root(), &id, &b.question, &t, &[]).ok();
    let _ = session;
    Json(serde_json::json!({"id": id, "transcript": t})).into_response()
}

/// POST /api/ask/stream — same agent, step-level SSE while it works.
/// Events: {"kind":"thought"|"tool_start"|"tool_result"|"final", ...}.
/// Falls back to a single JSON error when no provider is configured.
async fn ask_stream(State(s): State<Arc<Index>>, Json(b): Json<AskBody>) -> impl IntoResponse {
    use axum::response::sse::{Event, Sse};
    if b.question.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "empty question".to_string()).into_response();
    }
    let provider = match ferro_agent::OpenAiCompat::from_env(None, None, None) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, format!("no provider: {e}")).into_response()
        }
    };
    let sandbox = ferro_agent::Sandbox::readonly(s.root().to_path_buf());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let agent = ferro_agent::Agent {
        index: s.clone(),
        sandbox,
        client: Arc::new(provider),
        max_steps: b.max_steps.unwrap_or(8).clamp(1, 16),
    };
    let question = b.question.clone();
    let root = s.root().to_path_buf();
    tokio::spawn(async move {
        let t = agent.run_stream(&question, tx).await;
        let id = ferro_agent::new_id();
        let _ = ferro_agent::log_ask(&root, &id, &question, &t, &[]);
    });
    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(|ev| {
        let data = serde_json::to_string(&ev).unwrap_or_default();
        Ok::<_, std::convert::Infallible>(Event::default().data(data))
    });
    Sse::new(stream).into_response()
}

async fn diff(State(s): State<Arc<Index>>, Query(q): Query<DiffQ>) -> impl IntoResponse {
    let root = s.root().to_path_buf();
    let rel = q.path.clone();
    // PR mode: scoped merge-base diff instead of HEAD diff.
    if let Some(pr) = s.pr_ctx() {
        let base = pr.base_ref.clone();
        let out = tokio::task::spawn_blocking(move || {
            ferro_core::pr::diff_merge_base(&root, &base, rel.as_deref())
        })
        .await
        .unwrap_or_default();
        return (StatusCode::OK, out).into_response();
    }
    let out =
        tokio::task::spawn_blocking(move || ferro_core::git::diff_head(&root, rel.as_deref()))
            .await
            .unwrap_or_default();
    (StatusCode::OK, out).into_response()
}

async fn pr_info(State(s): State<Arc<Index>>) -> impl IntoResponse {
    match s.pr_ctx() {
        Some(pr) => Json(serde_json::json!({"pr": pr})).into_response(),
        None => Json(serde_json::json!({"pr": null})).into_response(),
    }
}

async fn markdown(State(s): State<Arc<Index>>, Query(q): Query<FileQ>) -> impl IntoResponse {
    let s2 = s.clone();
    let path = q.path.clone();
    let out = tokio::task::spawn_blocking(move || ferro_core::media::render_markdown(&s2, &path))
        .await
        .unwrap_or(None);
    match out {
        Some(html) => ([(header::CONTENT_TYPE, "text/html")], html).into_response(),
        None => (StatusCode::NOT_FOUND, "not markdown".to_string()).into_response(),
    }
}

async fn raw(State(s): State<Arc<Index>>, Query(q): Query<FileQ>) -> impl IntoResponse {
    let s2 = s.clone();
    let path = q.path.clone();
    let out = tokio::task::spawn_blocking(move || ferro_core::media::read_image_bytes(&s2, &path))
        .await
        .unwrap_or(None);
    match out {
        Some((bytes, mime)) => {
            // D3: SVG opened directly cannot run script; sniffing off; inline disposition.
            let name = q
                .path
                .rsplit('/')
                .next()
                .unwrap_or("file")
                .replace(['"', '\\', '\r', '\n'], "");
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, mime.parse().unwrap());
            headers.insert(
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; sandbox"
                    .parse()
                    .unwrap(),
            );
            headers.insert(
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{name}\"").parse().unwrap(),
            );
            (headers, bytes).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found".to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct DraftBody {
    path: String,
    line: usize,
    body: String,
}

async fn review_list() -> impl IntoResponse {
    Json(ferro_agent::drafts().list())
}

async fn review_add(Json(b): Json<DraftBody>) -> impl IntoResponse {
    if b.path.is_empty() || b.line == 0 || b.body.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "path, line>0 and body required".to_string(),
        )
            .into_response();
    }
    Json(ferro_agent::drafts().add(b.path, b.line, b.body)).into_response()
}

async fn review_delete(Path(id): Path<String>) -> impl IntoResponse {
    if ferro_agent::drafts().remove(&id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such draft".to_string()).into_response()
    }
}

#[derive(Deserialize)]
struct SubmitBody {
    event: Option<String>,
    body: Option<String>,
}

async fn review_apply(State(s): State<Arc<Index>>) -> impl IntoResponse {
    let Some(pr) = s.pr_ctx() else {
        return (StatusCode::BAD_REQUEST, "not in PR mode".to_string()).into_response();
    };
    let provider = match ferro_agent::OpenAiCompat::from_env(None, None, None) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, format!("no provider: {e}")).into_response()
        }
    };
    let drafts = ferro_agent::drafts().list();
    if drafts.is_empty() {
        return (StatusCode::BAD_REQUEST, "no drafts to apply".to_string()).into_response();
    }
    let mut prompt = format!(
        "You are addressing {} code review comment(s) on PR #{} ({}/{}). For each comment, make the minimal edit with apply_patch (one call per fix, unified diff with `+++ b/<path>` lines). Do not commit. Reply with a short summary of what changed.\n\nComments:\n",
        drafts.len(), pr.number, pr.owner, pr.repo
    );
    for d in &drafts {
        prompt.push_str(&format!("- {}:{} — {}\n", d.path, d.line, d.body));
    }
    let mut sandbox = ferro_agent::Sandbox::readonly(s.root().to_path_buf());
    sandbox.allow_write = true;
    let agent = ferro_agent::Agent {
        index: s.clone(),
        sandbox,
        client: Arc::new(provider),
        max_steps: 12,
    };
    let t = agent.run(&prompt).await;
    let applied = t
        .steps
        .iter()
        .flat_map(|st| st.calls.iter())
        .filter(|(c, r)| c.name == "apply_patch" && r.ok)
        .count();
    let id = ferro_agent::new_id();
    let _ = ferro_agent::log_ask(
        s.root(),
        &id,
        &format!("batch apply {} drafts", drafts.len()),
        &t,
        &[],
    );
    // Worktree changed under the index; refresh synchronously so UI is current.
    s.rebuild().await;
    Json(serde_json::json!({"id": id, "applied": applied, "transcript": t})).into_response()
}

async fn settings_get(State(s): State<Arc<Index>>) -> impl IntoResponse {
    Json(ferro_core::settings::Settings::new(s.root().to_path_buf()).get())
}

async fn settings_put(
    State(s): State<Arc<Index>>,
    Json(patch): Json<std::collections::BTreeMap<String, serde_json::Value>>,
) -> impl IntoResponse {
    match ferro_core::settings::Settings::new(s.root().to_path_buf()).save(&patch) {
        Ok(eff) => Json(eff).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(Deserialize)]
struct PathsBody {
    paths: Vec<String>,
}

async fn git_op(
    s: Arc<Index>,
    f: impl FnOnce(&std::path::Path) -> Result<String, String> + Send + 'static,
) -> impl IntoResponse {
    let root = s.root().to_path_buf();
    let out = tokio::task::spawn_blocking(move || f(&root))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match out {
        Ok(o) => (StatusCode::OK, o).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn git_stage(State(s): State<Arc<Index>>, Json(b): Json<PathsBody>) -> impl IntoResponse {
    git_op(s, move |r| ferro_core::git::stage(r, &b.paths)).await
}

async fn git_unstage(State(s): State<Arc<Index>>, Json(b): Json<PathsBody>) -> impl IntoResponse {
    git_op(s, move |r| ferro_core::git::unstage(r, &b.paths)).await
}

#[derive(Deserialize)]
struct CommitBody {
    message: String,
}

async fn git_commit(State(s): State<Arc<Index>>, Json(b): Json<CommitBody>) -> impl IntoResponse {
    git_op(s, move |r| ferro_core::git::commit(r, &b.message)).await
}

async fn git_push(State(s): State<Arc<Index>>) -> impl IntoResponse {
    git_op(s, ferro_core::git::push).await
}

async fn git_pull(State(s): State<Arc<Index>>) -> impl IntoResponse {
    git_op(s, ferro_core::git::pull_ff).await
}

async fn git_commit_message(State(s): State<Arc<Index>>) -> impl IntoResponse {
    let provider = match ferro_agent::OpenAiCompat::from_env(None, None, None) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, format!("no provider: {e}")).into_response()
        }
    };
    let root = s.root().to_path_buf();
    let staged = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["diff", "--cached"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    if staged.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "nothing staged — stage files first".to_string(),
        )
            .into_response();
    }
    let basis: String = staged.chars().take(6000).collect();
    match provider
        .complete_simple(
            "Write a single conventional-commit message (type: subject, <=72 chars, imperative). Output only the message.",
            &format!("Diff:\n{basis}"),
        )
        .await
    {
        Ok(m) => Json(serde_json::json!({"message": m.lines().next().unwrap_or("").trim()})).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

async fn review_submit(
    State(s): State<Arc<Index>>,
    Json(b): Json<SubmitBody>,
) -> impl IntoResponse {
    let Some(pr) = s.pr_ctx() else {
        return (StatusCode::BAD_REQUEST, "not in PR mode".to_string()).into_response();
    };
    let drafts = ferro_agent::drafts().list();
    let out = tokio::task::spawn_blocking(move || {
        ferro_agent::submit_review(
            &pr.owner,
            &pr.repo,
            pr.number,
            &b.event.unwrap_or_else(|| "comment".into()),
            &b.body.unwrap_or_default(),
            &drafts,
        )
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    match out {
        Ok(resp) => {
            ferro_agent::drafts().clear();
            (StatusCode::OK, resp).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

async fn static_file(
    Extension(g): Extension<Arc<crate::guard::GuardConfig>>,
    req: axum::http::Request<axum::body::Body>,
) -> impl IntoResponse {
    if let Some(res) = crate::guard::bootstrap(&g, &req) {
        return res;
    }
    let mut path = req.uri().path().trim_start_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".to_string();
    }
    match Web::get(&path) {
        Some(f) => {
            let mime = mime_guess(&path);
            ([(header::CONTENT_TYPE, mime)], f.data.to_vec()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn encode_qs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn mime_guess(p: &str) -> &'static str {
    if p.ends_with(".html") {
        "text/html"
    } else if p.ends_with(".js") {
        "text/javascript"
    } else if p.ends_with(".css") {
        "text/css"
    } else if p.ends_with(".json") {
        "application/json"
    } else if p.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}
