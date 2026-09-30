//! Updates (API.md § 13): status, check now, background install, restart into the new build.

use axum::{extract::State, http::StatusCode, routing::get, routing::post, Json, Router};
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::AppState;
use crate::update_check;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/update", get(status))
        .route("/api/v1/update/check", post(check))
        .route("/api/v1/update/install", post(install))
        .route("/api/v1/update/restart", post(restart))
}

fn body(s: &AppState) -> serde_json::Value {
    let mut v = serde_json::to_value(s.update.status()).unwrap_or_default();
    v["auto"] = serde_json::json!(update_check::user_update_auto_enabled(&s.settings));
    v
}

async fn status(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(body(&s))
}

async fn check(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    update_check::check_now(&s).await?;
    Ok(Json(body(&s)))
}

/// Starts the download; progress arrives as `update` events.
async fn install(
    State(s): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let st = s.update.status();
    if !st.can_install {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "this ferro cannot update itself (desktop app)",
        ));
    }
    if st.state != "available" && st.state != "error" {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("nothing to install (update state: {})", st.state),
        ));
    }
    let s2 = s.clone();
    tokio::spawn(async move {
        if let Err(e) = update_check::install(&s2).await {
            tracing::warn!("update install: {e}");
        }
    });
    Ok((StatusCode::ACCEPTED, Json(body(&s))))
}

async fn restart(
    State(s): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    update_check::restart(&s)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "restarting": true })),
    ))
}
