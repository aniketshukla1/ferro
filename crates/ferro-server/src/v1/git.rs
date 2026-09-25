//! Git surface (API.md § 6): status, changes, log, mutations.
//! Every call runs in `spawn_blocking` with the hardened runner; paths are
//! validated by § 5.1 inside the core mutations.

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/git/status", get(status))
        .route("/api/v1/git/changes", get(changes))
        .route("/api/v1/git/log", get(log))
        .route("/api/v1/git/stage", post(stage))
        .route("/api/v1/git/unstage", post(unstage))
        .route("/api/v1/git/discard", post(discard))
        .route("/api/v1/git/commit", post(commit))
        .route("/api/v1/git/push", post(push))
        .route("/api/v1/git/pull", post(pull))
}

fn no_git() -> ApiError {
    ApiError::new(ErrorCode::Unsupported, "not a git repository")
}

fn map_err(e: ferro_core::git::GitError) -> ApiError {
    use ferro_core::git::GitError as G;
    match e {
        G::NotRepo => no_git(),
        G::Forbidden(what) => ApiError::new(ErrorCode::Forbidden, what),
        G::Cancelled => ApiError::new(ErrorCode::Cancelled, "cancelled"),
        G::Timeout { args, secs } => ApiError::detail(
            ErrorCode::GitFailed,
            format!("git {args} timed out after {secs}s"),
            serde_json::json!({ "stderr": "" }),
        ),
        G::Failed { args, stderr } => {
            // Nothing staged and diverged pulls are client states, not crashes.
            if stderr.contains("nothing staged") || stderr.contains("empty message") {
                return ApiError::new(ErrorCode::Conflict, stderr);
            }
            if args.starts_with("pull") && stderr.contains("Not possible to fast-forward") {
                return ApiError::new(ErrorCode::Conflict, stderr);
            }
            ApiError::detail(
                ErrorCode::GitFailed,
                format!("git {args} failed"),
                serde_json::json!({ "stderr": stderr }),
            )
        }
        G::Io(e) => ApiError::new(ErrorCode::Internal, e),
    }
}

async fn status(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    let st = tokio::task::spawn_blocking(move || g.status_v2())
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
        .map_err(map_err)?;
    Ok(Json(serde_json::to_value(&st).unwrap()))
}

#[derive(Deserialize)]
struct ChangesQ {
    base: Option<String>,
    target: Option<String>,
}

async fn changes(
    State(s): State<Arc<AppState>>,
    Query(q): Query<ChangesQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    let base = q.base.unwrap_or_else(|| "HEAD".into());
    let target = q.target.unwrap_or_else(|| "worktree".into());
    if base.len() > 256 || target.len() > 256 {
        return Err(ApiError::bad_request("base/target too long"));
    }
    let cs = tokio::task::spawn_blocking(move || g.changes(&base, &target))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
        .map_err(map_err)?;
    Ok(Json(serde_json::to_value(&cs).unwrap()))
}

#[derive(Deserialize)]
struct LogQ {
    limit: Option<usize>,
    path: Option<String>,
}

async fn log(
    State(s): State<Arc<AppState>>,
    Query(q): Query<LogQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    if let Some(p) = &q.path {
        if p.len() > 512 {
            return Err(ApiError::bad_request("path too long"));
        }
    }
    let out = tokio::task::spawn_blocking(move || g.log(limit, q.path.as_deref()))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
        .map_err(map_err)?;
    Ok(Json(serde_json::json!({ "commits": out })))
}

fn fresh_status(g: &ferro_core::git::GitRepo) -> Result<serde_json::Value, ApiError> {
    let st = g.status_v2().map_err(map_err)?;
    Ok(serde_json::to_value(&st).unwrap())
}

#[derive(Deserialize)]
struct PathsBody {
    paths: Vec<String>,
}

fn check_paths(paths: &[String]) -> Result<(), ApiError> {
    if paths.is_empty() || paths.len() > 1000 {
        return Err(ApiError::bad_request("paths: 1..1000 required"));
    }
    if paths.iter().any(|p| p.len() > 512) {
        return Err(ApiError::bad_request("path too long"));
    }
    Ok(())
}

async fn stage(
    State(s): State<Arc<AppState>>,
    Json(b): Json<PathsBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_paths(&b.paths)?;
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.stage_paths(&b.paths)
            .map_err(map_err)
            .and_then(|_| fresh_status(&g))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

async fn unstage(
    State(s): State<Arc<AppState>>,
    Json(b): Json<PathsBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_paths(&b.paths)?;
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.unstage_paths(&b.paths)
            .map_err(map_err)
            .and_then(|_| fresh_status(&g))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

#[derive(Deserialize)]
struct DiscardBody {
    paths: Vec<String>,
    confirm: Option<bool>,
}

async fn discard(
    State(s): State<Arc<AppState>>,
    Json(b): Json<DiscardBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !b.confirm.unwrap_or(false) {
        return Err(ApiError::bad_request("discard requires confirm: true"));
    }
    check_paths(&b.paths)?;
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.discard_paths(&b.paths)
            .map_err(map_err)
            .and_then(|_| fresh_status(&g))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

#[derive(Deserialize)]
struct CommitBody {
    message: String,
    amend: Option<bool>,
}

async fn commit(
    State(s): State<Arc<AppState>>,
    Json(b): Json<CommitBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.message.len() > 100_000 {
        return Err(ApiError::bad_request("message too long"));
    }
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.commit_msg(&b.message, b.amend.unwrap_or(false))
            .map_err(map_err)
            .and_then(|c| {
                fresh_status(&g).map(
                    |st| serde_json::json!({ "sha": c.sha, "summary": c.subject, "status": st }),
                )
            })
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

async fn push(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.push().map_err(map_err).and_then(|output| {
            fresh_status(&g).map(|st| serde_json::json!({ "output": output, "status": st }))
        })
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

async fn pull(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.pull_ff().map_err(map_err).and_then(|(output, updated)| {
            fresh_status(&g)
                .map(|st| serde_json::json!({ "output": output, "updated": updated, "status": st }))
        })
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}
