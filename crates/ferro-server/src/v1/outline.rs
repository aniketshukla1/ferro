//! GET /api/v1/file/outline — tree-sitter symbols (B6) with regex fallback.
//! Response shape per API.md § 4.7 (unchanged from B1).

use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/file/outline", get(outline))
}

#[derive(Deserialize)]
struct OutlineQ {
    path: String,
}

async fn outline(
    State(s): State<Arc<AppState>>,
    Query(q): Query<OutlineQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let abs = super::files::resolve_pub(&s, &q.path)?;
    let ext = q.path.rsplit('.').next().unwrap_or("").to_lowercase();
    // Read and parse off the runtime: a tree-sitter pass over an 8 MiB file
    // would otherwise hold an async worker for the whole parse.
    let (source, symbols) = tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&abs)
            .map_err(|_| ApiError::not_found(format!("cannot read: {}", abs.display())))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(ApiError::new(
                crate::error::ErrorCode::TooLarge,
                "file over 8 MiB",
            ));
        }
        let text = String::from_utf8_lossy(&bytes);
        Ok::<_, ApiError>(outline_of(&ext, &text))
    })
    .await
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "outline task failed"))??;
    Ok(Json(
        serde_json::json!({ "path": q.path, "source": source, "symbols": symbols }),
    ))
}

/// Tree-sitter symbols when the language has a grammar, else the regex extractor.
fn outline_of(ext: &str, text: &str) -> (&'static str, Vec<serde_json::Value>) {
    match ferro_core::symbols::outline_ts(ext, text) {
        Some(syms) => (
            "treesitter",
            syms.into_iter()
                .map(|s| {
                    let mut o = serde_json::json!({
                        "name": s.name, "kind": s.kind,
                        "line": s.line, "depth": s.depth,
                    });
                    o["endLine"] = s.end_line.into();
                    if !s.detail.is_empty() {
                        o["detail"] = s.detail.into();
                    }
                    o
                })
                .collect::<Vec<_>>(),
        ),
        None => (
            "regex",
            ferro_core::outline::extract(ext, text)
                .into_iter()
                .map(|(name, kind, line, depth)| {
                    serde_json::json!({ "name": name, "kind": kind, "line": line, "depth": depth })
                })
                .collect::<Vec<_>>(),
        ),
    }
}
