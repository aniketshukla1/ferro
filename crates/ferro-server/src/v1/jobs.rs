//! Jobs registry HTTP surface (API.md § 7.1).

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/jobs", get(list))
        .route("/api/v1/jobs/{id}", get(one))
        .route("/api/v1/jobs/{id}/cancel", post(cancel))
        .route("/api/v1/index/rebuild", post(rebuild))
}

async fn list(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "jobs": s.jobs.list() }))
}

async fn one(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.jobs
        .get(&id)
        .map(|j| Json(serde_json::json!(j)))
        .ok_or_else(|| ApiError::not_found(format!("no such job: {id}")))
}

async fn cancel(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.jobs
        .cancel(&id)
        .map(|j| {
            s.bus.publish(crate::bus::ServerEvent::Job {
                job: serde_json::json!(j),
            });
            Json(serde_json::json!(j))
        })
        .ok_or_else(|| ApiError::not_found(format!("no such job: {id}")))
}

async fn rebuild(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    use crate::jobs::{Job, JobState};
    let token = tokio_util::sync::CancellationToken::new();
    let job = s.jobs.register(Job::new("index.rebuild"), token.clone());
    let id = job.id.clone();
    s.bus.publish(crate::bus::ServerEvent::Job {
        job: serde_json::json!(s.jobs.get(&id)),
    });
    let jobs = s.jobs.clone();
    let bus = s.bus.clone();
    let ws = s.ws();
    let live = id.clone();
    tokio::spawn(async move {
        jobs.update(&live, |j| j.state = JobState::Running);
        let idx = ws.index.clone();
        tokio::select! {
            _ = token.cancelled() => {
                jobs.update(&live, |j| {
                    j.state = JobState::Cancelled;
                    j.ended_at = Some(crate::jobs::now_iso());
                });
            }
            _ = async { idx.rebuild().await } => {
                let (files, ms) = idx.stats();
                ws.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                jobs.update(&live, |j| {
                    j.state = JobState::Done;
                    j.ended_at = Some(crate::jobs::now_iso());
                    j.result = Some(serde_json::json!({ "files": files, "ms": ms }));
                });
                bus.publish(crate::bus::ServerEvent::Index {
                    state: "ready".into(),
                    files,
                    ms,
                    generation: ws.generation.load(std::sync::atomic::Ordering::Relaxed),
                    search_index: "off".into(),
                });
            }
        }
        if let Some(j) = jobs.get(&live) {
            bus.publish(crate::bus::ServerEvent::Job {
                job: serde_json::json!(j),
            });
        }
    });
    Json(serde_json::json!({ "job": { "id": id, "kind": "index.rebuild" } }))
}
