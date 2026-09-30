//! `PUT /api/v1/credentials` — write-only OS keychain storage (B8).

use axum::{
    body::Bytes,
    extract::State,
    response::{IntoResponse, Response},
    routing::put,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/credentials", put(put_credentials))
}

#[derive(Deserialize)]
struct PutCredentialsBody {
    credentials: Vec<CredentialItem>,
}

#[derive(Deserialize)]
struct CredentialItem {
    id: String,
    value: String,
}

async fn put_credentials(
    State(_s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    if body.len() > 1024 * 1024 {
        return Err(ApiError::new(ErrorCode::TooLarge, "body over 1 MiB"));
    }
    // serde's message quotes the offending value, which may be the secret itself.
    let parsed: PutCredentialsBody = serde_json::from_slice(&body).map_err(|e| {
        ApiError::bad_request(format!(
            "invalid body at line {} column {}: expected {{\"credentials\": [{{\"id\", \"value\"}}]}}",
            e.line(),
            e.column()
        ))
    })?;
    if parsed.credentials.is_empty() {
        return Err(ApiError::bad_request("credentials must not be empty"));
    }
    // Validate every item before storing any, so a bad id never leaves a partial write.
    let mut items = Vec::with_capacity(parsed.credentials.len());
    for (i, item) in parsed.credentials.into_iter().enumerate() {
        // The id is not echoed: a swapped id/value pair would put the secret in the error.
        let id = ferro_core::credentials::CredentialId::parse(&item.id).map_err(|_| {
            ApiError::bad_request(format!("credentials[{i}]: unknown credential id"))
        })?;
        if item.value.trim().is_empty() {
            return Err(ApiError::bad_request(format!(
                "credentials[{i}]: value must not be empty"
            )));
        }
        items.push((id, item.value));
    }
    let mut stored = Vec::with_capacity(items.len());
    for (id, value) in &items {
        ferro_core::credentials::store(id, value).map_err(map_cred_err)?;
        stored.push(id.wire_id());
    }
    let target = format!("/api/v1/credentials?ids={}", stored.join(","));
    let mut res = Json(serde_json::json!({ "stored": stored })).into_response();
    res.extensions_mut()
        .insert(crate::audit::AuditTarget(target));
    Ok(res)
}

fn map_cred_err(e: ferro_core::credentials::CredentialError) -> ApiError {
    match e {
        ferro_core::credentials::CredentialError::KeychainUnavailable => ApiError::detail(
            ErrorCode::Unsupported,
            "OS keychain unavailable (no secret service on this host); use environment variables instead",
            serde_json::json!({
                "hint": "On headless Linux install and start a Secret Service (for example gnome-keyring or keepassxc) or set provider tokens in the environment."
            }),
        ),
        ferro_core::credentials::CredentialError::EmptyValue => {
            ApiError::bad_request("credential value must not be empty")
        }
        ferro_core::credentials::CredentialError::BadId(id) => {
            ApiError::bad_request(format!("unknown credential id: {id}"))
        }
        ferro_core::credentials::CredentialError::StoreFailed => ApiError::new(
            ErrorCode::Internal,
            "failed to store credential in the OS keychain",
        ),
    }
}
