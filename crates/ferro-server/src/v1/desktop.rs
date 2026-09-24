//! Desktop-host endpoints (API.md § 7.3). 422 on the CLI host.

use axum::{body::Bytes, extract::State, routing::post, Json, Router};
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::{AppState, Host};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/desktop/pick-folder", post(pick_folder))
        .route("/api/v1/desktop/open-external", post(open_external))
}

async fn desktop_host(s: &Arc<AppState>) -> Result<Arc<dyn crate::state::DesktopHost>, ApiError> {
    match &s.host {
        Host::Desktop(h) => Ok(h.clone()),
        Host::Cli => Err(ApiError::new(
            crate::error::ErrorCode::Unsupported,
            "desktop-only endpoint",
        )),
    }
}

async fn pick_folder(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let h = desktop_host(&s).await?;
    Ok(Json(serde_json::json!({ "path": h.pick_folder().await })))
}

async fn open_external(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let url = v
        .get("url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| ApiError::bad_request("body.url required"))?;
    let parsed: url::Url = url
        .parse()
        .map_err(|_| ApiError::bad_request("invalid url"))?;
    match parsed.scheme() {
        "http" | "https" | "mailto" => {}
        _ => {
            return Err(ApiError::bad_request(
                "url scheme must be http/https/mailto",
            ))
        }
    }
    desktop_host(&s)
        .await?
        .open_external(&parsed)
        .await
        .map_err(|e| ApiError::new(crate::error::ErrorCode::Internal, e.to_string()))?;
    Ok(Json(serde_json::json!({})))
}
