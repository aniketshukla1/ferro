//! POST /api/v1/auth/logout — sign this browser out of every ferro on this host
//! (its remembered cookie and all `ferro_<port>` session cookies expire);
//! `?all=1` also voids every other browser's session and remembered cookie.
//! The token link ferro printed keeps working for signing in again.

use axum::{
    extract::Query,
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    routing::post,
    Extension, Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::guard::GuardConfig;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/auth/logout", post(logout))
}

#[derive(Deserialize, Default)]
struct LogoutQ {
    all: Option<String>,
}

async fn logout(
    Extension(g): Extension<Arc<GuardConfig>>,
    Query(q): Query<LogoutQ>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let all = matches!(q.all.as_deref(), Some("1" | "true"));
    if all {
        g.forget_all_browsers().map_err(|e| {
            ApiError::new(
                ErrorCode::Internal,
                format!("could not sign out other browsers: {e}"),
            )
        })?;
    }
    let mut res = Json(serde_json::json!({ "ok": true, "all": all })).into_response();
    let sent = headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
    for c in g.expired_cookies(sent) {
        if let Ok(v) = HeaderValue::from_str(&c) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    Ok(res)
}
