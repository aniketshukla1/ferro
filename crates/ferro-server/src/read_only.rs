//! `--read-only`: block every API mutation with 403 (API.md §3.1 `readOnly`).

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension,
};
use std::sync::Arc;

use crate::guard::GuardConfig;

pub async fn read_only_guard(
    Extension(g): Extension<Arc<GuardConfig>>,
    req: Request<Body>,
    next: Next,
) -> impl IntoResponse {
    if g.read_only && crate::contract::read_only_blocks(req.method(), req.uri().path()) {
        return deny_read_only().into_response();
    }
    next.run(req).await
}

fn deny_read_only() -> Response {
    let body = serde_json::json!({
        "error": {
            "code": "forbidden",
            "message": "read-only mode: mutations are disabled",
        }
    })
    .to_string();
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )
        .body(Body::from(body))
        .unwrap()
}
