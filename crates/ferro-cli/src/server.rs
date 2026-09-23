use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

use ferro_core::Index;

#[derive(RustEmbed, Clone)]
#[folder = "../../web/"]
struct Web;

pub async fn serve(
    state: Arc<Index>,
    host: &str,
    port: u16,
    no_git: bool,
    narrate: bool,
    no_open: bool,
) {
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
        .route("/api/ask", post(ask));
    if !no_git {
        app = app
            .route("/api/git-status", get(git_status))
            .route("/api/diff", get(diff));
    }
    app = app.route("/api/pr-info", get(pr_info));
    app = app
        .route("/api/review/drafts", get(review_list).post(review_add))
        .route("/api/review/drafts/{id}", delete(review_delete))
        .route("/api/review/submit", post(review_submit));
    let app = app
        .fallback(static_file)
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}"))
        .await
        .expect("bind");
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let host_out = if host == "0.0.0.0" { "127.0.0.1" } else { host };
    if narrate {
        tracing::info!(
            "ferro serving {} on http://{}:{}/",
            state.root().display(),
            host_out,
            bound
        );
        println!("ferro http://{}:{}/", host_out, bound);
    }

    if !no_open {
        let _ = open::that(format!("http://127.0.0.1:{bound}/"));
    }

    axum::serve(listener, app).await.expect("serve");
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status":"ok","service":"ferro"}))
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

async fn static_file(uri: axum::http::Uri) -> impl IntoResponse {
    let mut path = uri.path().trim_start_matches('/').to_string();
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
