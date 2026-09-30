//! Model Context Protocol server (B7b): stdio + HTTP.

mod http;
mod protocol;
mod stdio;
pub mod tools;

pub use http::post_mcp;
pub use stdio::serve_stdio;

use axum::{routing::post, Router};
use std::sync::Arc;

use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/mcp", post(post_mcp))
}
