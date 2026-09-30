//! Diagnostics from language servers (API.md § 4.9): `GET /lsp/open`, `GET /diagnostics`.
//! Opening is a GET: it only reads the file (like `/symbols` building its index on demand), so
//! read-only mode allows it and the mutation audit log does not record every file view.

use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/lsp/open", get(open))
        .route("/api/v1/diagnostics", get(diagnostics))
}

#[derive(Deserialize)]
struct OpenBody {
    path: String,
}

/// The viewer opened a file: hand it to its language's server (started on first use).
async fn open(
    State(s): State<Arc<AppState>>,
    Query(b): Query<OpenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let full = super::files::resolve_pub(&s, &b.path)?;
    let root = s.ws().root.clone();
    let rel = full
        .strip_prefix(root.canonicalize().unwrap_or(root.clone()))
        .or_else(|_| full.strip_prefix(&root))
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .map_err(|_| ApiError::new(ErrorCode::Forbidden, "path outside workspace"))?;
    let lsp = s.lsp();
    if !lsp.enabled() {
        return Ok(Json(
            serde_json::json!({ "opened": false, "enabled": false }),
        ));
    }
    let opened = tokio::task::spawn_blocking(move || lsp.open(&rel))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "lsp task failed"))?;
    Ok(Json(
        serde_json::json!({ "opened": opened, "enabled": true }),
    ))
}

async fn diagnostics(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let lsp = s.lsp();
    let files = lsp.diagnostics();
    let mut counts = serde_json::json!({ "error": 0, "warning": 0, "info": 0, "hint": 0 });
    for (_, list) in &files {
        for d in list {
            counts[d.severity] = serde_json::json!(counts[d.severity].as_u64().unwrap_or(0) + 1);
        }
    }
    Json(serde_json::json!({
        "enabled": lsp.enabled(),
        "servers": lsp.servers(),
        "files": files.into_iter().map(|(path, diagnostics)| serde_json::json!({ "path": path, "diagnostics": diagnostics })).collect::<Vec<_>>(),
        "counts": counts,
    }))
}
