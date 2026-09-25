//! One error type for the whole HTTP boundary (BACKEND.md § 4.4).
//! Every non-2xx API response uses the API.md § 1.4 envelope.

use axum::{
    body::Body,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    TooLarge,
    Unsupported,
    NotReady,
    RateLimited,
    Upstream,
    GitFailed,
    Cancelled,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Conflict => "conflict",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::NotReady => "not_ready",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::Upstream => "upstream",
            ErrorCode::GitFailed => "git_failed",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::Internal => "internal",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::Forbidden => StatusCode::FORBIDDEN,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::Conflict => StatusCode::CONFLICT,
            ErrorCode::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::Unsupported => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::NotReady => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::Upstream => StatusCode::BAD_GATEWAY,
            ErrorCode::GitFailed => StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Cancelled => StatusCode::from_u16(499).unwrap(),
            ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{1}")]
    Coded(ErrorCode, String, Option<serde_json::Value>),
    #[error("internal error")]
    Internal,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError::Coded(code, message.into(), None)
    }

    pub fn detail(code: ErrorCode, message: impl Into<String>, detail: serde_json::Value) -> Self {
        ApiError::Coded(code, message.into(), Some(detail))
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, what)
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            ApiError::Coded(c, _, _) => *c,
            ApiError::Internal => ErrorCode::Internal,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, message, detail) = match self {
            ApiError::Coded(c, m, d) => (c, m, d),
            ApiError::Internal => (ErrorCode::Internal, "internal error".to_string(), None),
        };
        let mut err = serde_json::json!({ "code": code.as_str(), "message": message });
        if let Some(d) = detail {
            err["detail"] = d;
        }
        let body = serde_json::json!({ "error": err }).to_string();
        Response::builder()
            .status(code.status())
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/json; charset=utf-8",
            )
            .body(Body::from(body))
            .unwrap()
    }
}

/// Axum JSON rejections map to `bad_request` (B1).
pub fn json_rejection(err: axum::extract::rejection::JsonRejection) -> ApiError {
    ApiError::bad_request(format!("invalid JSON body: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shape() {
        let res = ApiError::detail(
            ErrorCode::BadRequest,
            "bad regex",
            serde_json::json!({ "position": 3 }),
        )
        .into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn codes_cover_contract() {
        assert_eq!(ErrorCode::Conflict.status(), StatusCode::CONFLICT);
        assert_eq!(ErrorCode::TooLarge.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            ErrorCode::Unsupported.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(ErrorCode::Cancelled.status().as_u16(), 499);
    }
}
