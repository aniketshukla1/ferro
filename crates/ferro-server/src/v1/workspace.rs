//! Workspace switching (API.md § 7.2). `{ path }` switches roots in place;
//! `{ prUrl }` is B4 (422 until then).

use axum::{body::Bytes, extract::State, routing::post, Json, Router};
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::{AppState, Mode, Workspace};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/workspace/open", post(open))
}

async fn open(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > 1024 * 1024 {
        return Err(ApiError::new(
            crate::error::ErrorCode::TooLarge,
            "body over 1 MiB",
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    if let Some(url) = v.get("prUrl").and_then(|u| u.as_str()) {
        // PR workspaces open through the same pr.open job.
        return super::pr::start_open_job(&s, url).await;
    }
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| ApiError::bad_request("body.path required"))?;
    if path.is_empty() {
        return Err(ApiError::bad_request("body.path required"));
    }
    let root = std::path::PathBuf::from(path);
    if !root.is_dir() {
        return Err(ApiError::not_found(format!("not a directory: {path}")));
    }
    let ws = Workspace::local(root, &s.dirs);
    let key = ws.key.clone();
    let root_s = ws.root.to_string_lossy().to_string();
    let mode = match ws.mode {
        Mode::Local => "workspace",
        Mode::Pr => "pr",
    }
    .to_string();
    let files = ws.index.snapshot().len();
    s.ws.store(ws);
    s.jobs.register(
        crate::jobs::Job::new("workspace.open"),
        tokio_util::sync::CancellationToken::new(),
    );
    // Index in the background like boot does.
    let ws2 = s.ws();
    let bus = s.bus.clone();
    tokio::spawn(async move {
        ws2.index.rebuild().await;
        ws2.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (files, ms) = ws2.index.stats();
        bus.publish(crate::bus::ServerEvent::Index {
            state: "ready".into(),
            files,
            ms,
            generation: ws2.generation.load(std::sync::atomic::Ordering::Relaxed),
            search_index: "off".into(),
        });
    });
    s.bus.publish(crate::bus::ServerEvent::Workspace {
        key: key.clone(),
        root: root_s,
        mode: mode.clone(),
    });
    Ok(Json(
        serde_json::json!({ "job": { "id": format!("j_ws_{key}"), "kind": "workspace.open" }, "files": files, "mode": mode }),
    ))
}
