//! /api/v1 route handlers (BACKEND.md § 6 B1 surface).

pub mod auth;
pub mod desktop;
pub mod events;
pub mod files;
pub mod highlight;
pub mod jobs;
pub mod markdown;
pub mod meta;
pub mod metrics;
pub mod outline;
pub mod session;
pub mod settings;
pub mod workspace;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(meta::routes())
        .merge(auth::routes())
        .merge(events::routes())
        .merge(settings::routes())
        .merge(session::routes())
        .merge(files::routes())
        .merge(markdown::routes())
        .merge(highlight::routes())
        .merge(outline::routes())
        .merge(jobs::routes())
        .merge(workspace::routes())
        .merge(desktop::routes())
        .merge(metrics::routes())
}
