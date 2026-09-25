//! /api/v1 route handlers (BACKEND.md § 6 B1 surface).

pub mod desktop;
pub mod events;
pub mod files;
pub mod git;
pub mod highlight;
pub mod jobs;
pub mod markdown;
pub mod meta;
pub mod metrics;
pub mod outline;
pub mod search;
pub mod session;
pub mod settings;
pub mod workspace;

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// Accept `?flag=1`, `?flag=0`, `?flag=true`, `?flag=false` (API.md writes 0/1).
pub(crate) fn de_flag<'de, D>(d: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = serde::Deserialize::deserialize(d)?;
    match opt.as_deref() {
        None => Ok(None),
        Some("1" | "true" | "yes") => Ok(Some(true)),
        Some("0" | "false" | "no") => Ok(Some(false)),
        Some(other) => Err(serde::de::Error::custom(format!("bad flag: {other}"))),
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(meta::routes())
        .merge(events::routes())
        .merge(settings::routes())
        .merge(session::routes())
        .merge(files::routes())
        .merge(markdown::routes())
        .merge(highlight::routes())
        .merge(outline::routes())
        .merge(git::routes())
        .merge(search::routes())
        .merge(jobs::routes())
        .merge(workspace::routes())
        .merge(desktop::routes())
        .merge(metrics::routes())
}
