//! POST /mcp — JSON-RPC over HTTP (Bearer auth via global guard).

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;

use crate::error::ApiError;
use crate::state::AppState;

use super::protocol;

pub async fn post_mcp(State(s): State<Arc<AppState>>, body: Bytes) -> Result<Response, ApiError> {
    if body.len() > 1024 * 1024 {
        return Err(ApiError::new(
            crate::error::ErrorCode::TooLarge,
            "body over 1 MiB",
        ));
    }
    let msg: Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let version = s.version.clone();
    match protocol::handle_message(&s, msg, &version).await {
        Some(resp) => Ok((
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            Json(resp),
        )
            .into_response()),
        None => Ok(StatusCode::ACCEPTED.into_response()),
    }
}
