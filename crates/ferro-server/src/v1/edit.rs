//! Inline edits (API.md § 4.10): `POST /api/v1/file/edit` replaces a line range typed by a
//! person or proposed by the AI (§ 10.9), only while those lines still read as the editor saw
//! them. `--read-only` refuses it and the audit log records it (both from API.md § 2).

use axum::{extract::State, routing::post, Json, Router};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/file/edit", post(edit))
}

#[derive(Deserialize)]
struct EditBody {
    path: String,
    #[serde(rename = "startLine")]
    start_line: usize,
    #[serde(rename = "endLine")]
    end_line: usize,
    expected: String,
    text: String,
}

fn path_err(e: ferro_core::paths::PathError) -> ApiError {
    use ferro_core::paths::PathError as P;
    match e {
        P::Empty => ApiError::bad_request("empty path"),
        P::Escapes => ApiError::new(ErrorCode::Forbidden, "path outside workspace"),
        P::Protected => ApiError::new(ErrorCode::Forbidden, "protected path"),
    }
}

async fn edit(
    State(s): State<Arc<AppState>>,
    Json(b): Json<EditBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use ferro_core::edit::{replace_lines, write_atomic, EditError, MAX_EDIT_BYTES};
    let rel = b.path.trim().trim_start_matches('/').to_string();
    if rel.is_empty() || rel.len() > 1024 {
        return Err(ApiError::bad_request("path required (at most 1024 bytes)"));
    }
    let abs = ferro_core::paths::resolve(&s.ws().root, &rel, ferro_core::paths::Access::Write)
        .map_err(path_err)?;
    let (start, end) = (b.start_line, b.end_line);
    tokio::task::spawn_blocking(move || {
        let md = std::fs::metadata(&abs)
            .map_err(|_| ApiError::not_found(format!("not found: {rel}")))?;
        if !md.is_file() {
            return Err(ApiError::bad_request("not a file"));
        }
        if md.len() > MAX_EDIT_BYTES {
            return Err(ApiError::new(
                ErrorCode::TooLarge,
                "files over 8 MiB are not edited in place",
            ));
        }
        let bytes = std::fs::read(&abs)
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("cannot read: {e}")))?;
        let out = replace_lines(&bytes, start, end, &b.expected, &b.text).map_err(|e| match e {
            EditError::NotText => ApiError::new(
                ErrorCode::Unsupported,
                "only UTF-8 text files can be edited in ferro",
            ),
            EditError::Range => ApiError::bad_request("startLine/endLine outside the file"),
            EditError::Stale(current) => ApiError::detail(
                ErrorCode::Conflict,
                "these lines changed on disk since you opened them",
                serde_json::json!({ "current": current }),
            ),
        })?;
        write_atomic(&abs, &out.bytes)
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("cannot write: {e}")))?;
        let mtime_ms = std::fs::metadata(&abs)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Ok(Json(serde_json::json!({
            "path": rel,
            "lines": out.lines,
            "startLine": start,
            "endLine": out.end,
            "mtimeMs": mtime_ms,
        })))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "edit task failed"))?
}
