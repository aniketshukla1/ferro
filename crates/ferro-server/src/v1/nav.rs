//! Symbol search (B6, API.md `GET /api/v1/symbols`): fuzzy over workspace
//! symbol names from the background symbol index. Definition, references,
//! and hover land here in slice 3.

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
    Router::new().route("/api/v1/symbols", get(symbols))
}

#[derive(Deserialize)]
struct SymbolsQ {
    q: Option<String>,
    limit: Option<usize>,
}

async fn symbols(
    State(s): State<Arc<AppState>>,
    Query(q): Query<SymbolsQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = q.q.unwrap_or_default();
    if query.len() > 256 {
        return Err(ApiError::bad_request("q over 256 chars"));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 100);
    let ws = s.ws();
    // Build (or reuse) the index synchronously when empty: tests and small
    // workspaces finish in milliseconds; big trees build in the background.
    if ws.symbols.state() != ferro_core::symindex::SymbolState::Ready {
        let snap = ws.index.file_index.load();
        if snap.len() <= 2000 {
            let ws2 = ws.clone();
            tokio::task::spawn_blocking(move || {
                ws2.symbols.ensure_built(&ws2.index.file_index.load());
            })
            .await
            .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "symbols task failed"))?;
            // Wait briefly for the background build on small trees.
            let t0 = std::time::Instant::now();
            while ws.symbols.state() != ferro_core::symindex::SymbolState::Ready
                && t0.elapsed() < std::time::Duration::from_secs(10)
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        } else {
            ws.symbols.ensure_built(&snap);
        }
    }
    let t0 = std::time::Instant::now();
    let q2 = query.clone();
    let hits = tokio::task::spawn_blocking(move || ws.symbols.query(&q2, limit))
        .await
        .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "symbols task failed"))?;
    Ok(Json(serde_json::json!({
        "q": query,
        "ms": t0.elapsed().as_millis(),
        "symbols": hits.into_iter().map(|(sym, score)| serde_json::json!({
            "path": sym.path, "name": sym.name, "kind": sym.kind.as_str(),
            "line": sym.line, "endLine": sym.end_line, "score": score,
        })).collect::<Vec<_>>(),
    })))
}
