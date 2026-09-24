//! Session blob endpoints (API.md § 3.3).

use axum::{body::Bytes, extract::State, routing::get, Json, Router};
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/session", get(session_get).put(session_put))
}

async fn session_get(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let ws = s.ws();
    let (data, updated) = read_session(&ws.session_path);
    Json(serde_json::json!({ "data": data, "updatedAt": updated }))
}

async fn session_put(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > 256 * 1024 {
        return Err(ApiError::new(
            crate::error::ErrorCode::TooLarge,
            "session over 256 KiB",
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let data = v.get("data").cloned().unwrap_or(serde_json::Value::Null);
    if !data.is_object() && !data.is_null() {
        return Err(ApiError::bad_request("data must be an object"));
    }
    let ws = s.ws();
    if let Some(dir) = ws.session_path.parent() {
        std::fs::create_dir_all(dir).map_err(|_| {
            ApiError::new(crate::error::ErrorCode::Internal, "cannot store session")
        })?;
    }
    let at = crate::jobs::now_iso();
    let doc = serde_json::json!({ "data": data, "updatedAt": at });
    ferro_core::settings::atomic_write(
        &ws.session_path,
        serde_json::to_string_pretty(&doc)
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "cannot store session"))?;
    Ok(Json(serde_json::json!({ "updatedAt": at })))
}

fn read_session(path: &std::path::Path) -> (serde_json::Value, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (serde_json::Value::Null, None);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (serde_json::Value::Null, None);
    };
    (
        v.get("data").cloned().unwrap_or(serde_json::Value::Null),
        v.get("updatedAt")
            .and_then(|u| u.as_str())
            .map(|s| s.to_string()),
    )
}
