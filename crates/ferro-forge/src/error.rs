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
    Auth { provider: &'static str },
    /// The forge declined an action the token is valid for (e.g. GitLab
    /// answers an approve it won't take with 401).
    #[error("{provider} refused: {reason}")]
    Refused {
        provider: &'static str,
        reason: String,
    },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unexpected response: {0}")]
    Schema(String),
}

/// Where a token for `provider` ("github" / "gitlab") comes from.
pub fn token_hint(provider: &str) -> &'static str {
    if provider == "gitlab" {
        "set GITLAB_TOKEN (for gitlab.com or GITLAB_HOST) or `glab auth login`"
    } else {
        "set GITHUB_TOKEN / GH_TOKEN, `gh auth login`, `PUT /api/v1/credentials`, or the OS keychain"
    }
}

impl ForgeError {
    /// API error code for the HTTP boundary.
    pub fn code(&self) -> &'static str {
        match self {
            ForgeError::BadUrl(_) => "bad_request",
            ForgeError::Network(_) => "upstream",
            ForgeError::Upstream { .. } => "upstream",
            ForgeError::RateLimited { .. } => "rate_limited",
            ForgeError::Auth { .. } => "unauthorized",
            ForgeError::Refused { .. } => "upstream",
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
            ForgeError::Auth { .. } => 401,
            ForgeError::Refused { .. } => 403,
            ForgeError::NotFound(_) => 404,
            ForgeError::Schema(_) => 502,
        }
    }

    /// The forge that answered, when the error came from one.
    pub fn provider(&self) -> Option<&'static str> {
        match self {
            ForgeError::Upstream { provider, .. }
            | ForgeError::Auth { provider }
            | ForgeError::Refused { provider, .. } => Some(provider),
            _ => None,
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
            ForgeError::Refused { provider, .. } => {
                serde_json::json!({ "status": 403, "provider": provider })
            }
            ForgeError::Auth { provider } => {
                serde_json::json!({ "hint": token_hint(provider) })
            }
            _ => serde_json::Value::Null,
        }
    }
}
