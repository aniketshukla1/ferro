//! Failure modes map onto API.md § 1.4: `rate_limited` carries
//! `retryAfterMs`; `upstream` carries `{status, provider}`.

use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum ForgeError {
    #[error("bad PR/MR url: {0}")]
    BadUrl(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("upstream {provider} status {status}")]
    Upstream { provider: &'static str, status: u16 },
    #[error("rate limited")]
    RateLimited { retry_after_ms: u64 },
    #[error("auth failed (check token scopes)")]
    Auth,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unexpected response: {0}")]
    Schema(String),
}

impl ForgeError {
    /// API error code for the HTTP boundary.
    pub fn code(&self) -> &'static str {
        match self {
            ForgeError::BadUrl(_) => "bad_request",
            ForgeError::Network(_) => "upstream",
            ForgeError::Upstream { .. } => "upstream",
            ForgeError::RateLimited { .. } => "rate_limited",
            ForgeError::Auth => "unauthorized",
            ForgeError::NotFound(_) => "not_found",
            ForgeError::Schema(_) => "upstream",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            ForgeError::BadUrl(_) => 400,
            ForgeError::Network(_) => 502,
            ForgeError::Upstream { status, .. } => *status,
            ForgeError::RateLimited { .. } => 429,
            ForgeError::Auth => 401,
            ForgeError::NotFound(_) => 404,
            ForgeError::Schema(_) => 502,
        }
    }

    pub fn detail(&self) -> serde_json::Value {
        match self {
            ForgeError::RateLimited { retry_after_ms } => {
                serde_json::json!({ "retryAfterMs": retry_after_ms })
            }
            ForgeError::Upstream { status, provider } => {
                serde_json::json!({ "status": status, "provider": provider })
            }
            ForgeError::Auth => {
                serde_json::json!({ "hint": "set GITHUB_TOKEN / GH_TOKEN, `gh auth login`, or the OS keychain (B8)" })
            }
            _ => serde_json::Value::Null,
        }
    }
}
