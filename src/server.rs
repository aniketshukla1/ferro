use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

use crate::index::Index;

#[derive(RustEmbed, Clone)]
#[folder = "web/"]
struct Web;

pub async fn serve(state: Arc<Index>, addr: &str) {
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/files", get(files))
        .route("/api/fuzzy", get(fuzzy))
        .route("/api/search", get(search))
        .route("/api/file", get(read_file))
        .route("/api/git-status", get(git_status))
        .route("/api/diff", get(diff))
        .fallback(static_file)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
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
    let ranked = crate::fuzzy::rank(&query, &paths, limit);
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
    let hits = tokio::task::spawn_blocking(move || crate::search::grep(&root, &query, limit))
        .await
        .unwrap_or_default();
    Json(hits)
}

#[derive(Deserialize)]
struct FileQ {
    path: String,
}

async fn read_file(State(s): State<Arc<Index>>, Query(q): Query<FileQ>) -> impl IntoResponse {
    let Some(p) = s.safe_join(&q.path) else {
        return (StatusCode::FORBIDDEN, "traversal blocked".to_string()).into_response();
    };
    match tokio::fs::read_to_string(&p).await {
        Ok(t) => {
            // Cap at ~512KB window parity with px0 hlWindowBytes.
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
    let out = tokio::task::spawn_blocking(move || crate::git::status(&root))
        .await
        .unwrap_or_default();
    (StatusCode::OK, out).into_response()
}

#[derive(Deserialize)]
struct DiffQ {
    path: Option<String>,
}

async fn diff(State(s): State<Arc<Index>>, Query(q): Query<DiffQ>) -> impl IntoResponse {
    let root = s.root().to_path_buf();
    let rel = q.path.clone();
    let out = tokio::task::spawn_blocking(move || crate::git::diff_head(&root, rel.as_deref()))
        .await
        .unwrap_or_default();
    (StatusCode::OK, out).into_response()
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

#[derive(Serialize)]
struct _Unused {
    _a: u8,
}
