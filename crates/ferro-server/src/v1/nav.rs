//! Symbol search (B6, API.md `GET /api/v1/symbols`) plus code navigation:
//! `GET /api/v1/nav/definition`, `/nav/references`, `/nav/hover`.
//! Columns are 1-based UTF-16 units. Definition ranking: same file, then
//! same directory, then import-resolved, then global; top 10.

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
    Router::new()
        .route("/api/v1/symbols", get(symbols))
        .route("/api/v1/nav/definition", get(definition))
        .route("/api/v1/nav/references", get(references))
        .route("/api/v1/nav/hover", get(hover))
}

#[derive(Deserialize)]
struct SymbolsQ {
    q: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct NavQ {
    path: String,
    line: Option<usize>,
    col: Option<usize>,
    limit: Option<usize>,
}

fn nav_error(e: ferro_core::nav::NavError) -> ApiError {
    use ferro_core::nav::NavError as N;
    match e {
        N::BadPath(m) => ApiError::not_found(m),
        N::BadPosition(m) => ApiError::bad_request(m),
        N::NoIdentifier => ApiError::not_found("no definition here".to_string()),
        N::TooLarge => ApiError::new(crate::error::ErrorCode::TooLarge, "file too large"),
    }
}

fn nav_params(q: &NavQ) -> Result<(String, usize, usize), ApiError> {
    let path = q.path.trim().trim_start_matches('/').to_string();
    if path.is_empty() || path.len() > 512 {
        return Err(ApiError::bad_request("path required (≤512)"));
    }
    Ok((path, q.line.unwrap_or(1).max(1), q.col.unwrap_or(1).max(1)))
}

/// Build (or reuse) the symbol index: small trees (tests, tiny workspaces)
/// build synchronously in milliseconds; big trees build in the background
/// and queries serve whatever is ready.
async fn ensure_ready(ws: &Arc<crate::state::Workspace>) -> Result<(), ApiError> {
    use ferro_core::symindex::SymbolState;
    if ws.symbols.state() == SymbolState::Ready {
        return Ok(());
    }
    let snap = ws.index.file_index.load();
    if snap.len() <= 2000 {
        let ws2 = ws.clone();
        tokio::task::spawn_blocking(move || {
            ws2.symbols.ensure_built(&ws2.index.file_index.load());
        })
        .await
        .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "symbols task failed"))?;
        let t0 = std::time::Instant::now();
        while ws.symbols.state() != SymbolState::Ready
            && t0.elapsed() < std::time::Duration::from_secs(10)
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    } else {
        ws.symbols.ensure_built(&snap);
    }
    Ok(())
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
    ensure_ready(&ws).await?;
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

async fn definition(
    State(s): State<Arc<AppState>>,
    Query(q): Query<NavQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (path, line, col) = nav_params(&q)?;
    super::files::resolve_pub(&s, &path)?;
    let ws = s.ws();
    ensure_ready(&ws).await?;
    let t0 = std::time::Instant::now();
    let defs = tokio::task::spawn_blocking(move || {
        let snap = ws.index.file_index.load();
        let nav = ferro_core::nav::Nav {
            root: &ws.root,
            snap: &snap,
            syms: &ws.symbols,
        };
        ferro_core::nav::definitions(&nav, &path, line, col, 10)
    })
    .await
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "nav task failed"))?
    .map_err(nav_error)?;
    Ok(Json(serde_json::json!({
        "definitions": defs.into_iter().map(|d| serde_json::json!({
            "path": d.path, "line": d.line, "endLine": d.end_line,
            "kind": d.kind, "name": d.name, "source": d.source,
        })).collect::<Vec<_>>(),
        "ms": t0.elapsed().as_millis(),
    })))
}

async fn references(
    State(s): State<Arc<AppState>>,
    Query(q): Query<NavQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (path, line, col) = nav_params(&q)?;
    super::files::resolve_pub(&s, &path)?;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let ws = s.ws();
    ensure_ready(&ws).await?;
    let t0 = std::time::Instant::now();
    let (refs, truncated) = tokio::task::spawn_blocking(move || {
        let snap = ws.index.file_index.load();
        let nav = ferro_core::nav::Nav {
            root: &ws.root,
            snap: &snap,
            syms: &ws.symbols,
        };
        ferro_core::nav::references(&nav, &path, line, col, limit)
    })
    .await
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "nav task failed"))?
    .map_err(nav_error)?;
    Ok(Json(serde_json::json!({
        "references": refs.into_iter().map(|r| serde_json::json!({
            "path": r.path, "line": r.line, "col": r.col,
        })).collect::<Vec<_>>(),
        "truncated": truncated,
        "ms": t0.elapsed().as_millis(),
    })))
}

async fn hover(
    State(s): State<Arc<AppState>>,
    Query(q): Query<NavQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (path, line, col) = nav_params(&q)?;
    super::files::resolve_pub(&s, &path)?;
    let ws = s.ws();
    ensure_ready(&ws).await?;
    let h = tokio::task::spawn_blocking(move || {
        let snap = ws.index.file_index.load();
        let nav = ferro_core::nav::Nav {
            root: &ws.root,
            snap: &snap,
            syms: &ws.symbols,
        };
        ferro_core::nav::hover(&nav, &path, line, col)
    })
    .await
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "nav task failed"))?
    .map_err(nav_error)?;
    Ok(Json(serde_json::json!({
        "name": h.name, "kind": h.kind, "path": h.path, "line": h.line,
        "signature": h.signature, "doc": h.doc,
    })))
}
