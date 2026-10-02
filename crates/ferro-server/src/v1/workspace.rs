//! Workspace switching (API.md § 7.2). `{ path }` switches roots in place (an absolute path,
//! or `~/…`); `{ prUrl }` opens a pull request. `GET /workspace/recent` lists folders opened before.

use axum::{
    body::Bytes,
    extract::State,
    routing::{get, post},
    Extension, Json, Router,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::ApiError;
use crate::guard::GuardConfig;
use crate::state::{AppState, Mode, Workspace};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/workspace/open", post(open))
        .route("/api/v1/workspace/recent", get(recent))
}

/// `~` and `~/…` are the user's home; anything else must be absolute (never the server's cwd).
fn expand(path: &str, home: &Path) -> Option<PathBuf> {
    let p = match path.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with(['/', std::path::MAIN_SEPARATOR]) => home.join(&rest[1..]),
        _ => PathBuf::from(path),
    };
    p.is_absolute().then_some(p)
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
    let root = expand(path, &ferro_core::dirs::home_dir())
        .ok_or_else(|| ApiError::bad_request("path must be absolute, or start with ~/"))?;
    if !root.is_dir() {
        return Err(ApiError::not_found(format!("not a directory: {path}")));
    }
    let root = root.canonicalize().unwrap_or(root);
    let ws = Workspace::local(root, &s.dirs);
    crate::recent::record(&s.dirs, &ws.root);
    let key = ws.key.clone();
    let root_s = ws.root.to_string_lossy().to_string();
    let mode = match ws.mode {
        Mode::Local => "workspace",
        Mode::Pr => "pr",
    }
    .to_string();
    let files = ws.index.snapshot().len();
    s.ws.store(ws);
    // Live updates follow the workspace: the old root's watcher stops.
    // Setting up recursive watches walks the tree, so not on a worker.
    let s2 = s.clone();
    tokio::task::spawn_blocking(move || crate::watch::restart(&s2));
    s.jobs.register(
        crate::jobs::Job::new("workspace.open"),
        tokio_util::sync::CancellationToken::new(),
    );
    // Index in the background like boot does (search index included).
    s.index_in_background(s.ws());
    s.bus.publish(crate::bus::ServerEvent::Workspace {
        key: key.clone(),
        root: root_s,
        mode: mode.clone(),
    });
    Ok(Json(
        serde_json::json!({ "job": { "id": format!("j_ws_{key}"), "kind": "workspace.open" }, "files": files, "mode": mode }),
    ))
}

/// Folders opened before, newest first; gone ones are skipped.
async fn recent(
    State(s): State<Arc<AppState>>,
    Extension(g): Extension<Arc<GuardConfig>>,
) -> Json<serde_json::Value> {
    // A read-only ferro can't switch, and its viewers need not learn other paths on the host.
    if g.read_only {
        return Json(serde_json::json!({ "items": [] }));
    }
    let ws = s.ws();
    let current = ws.root.canonicalize().unwrap_or_else(|_| ws.root.clone());
    let items: Vec<serde_json::Value> = crate::recent::load(&s.dirs)
        .into_iter()
        .filter(|r| Path::new(&r.root).is_dir())
        .map(|r| {
            let p = Path::new(&r.root);
            serde_json::json!({
                "root": r.root,
                "name": p.file_name().map_or_else(|| r.root.clone(), |n| n.to_string_lossy().into_owned()),
                "openedAt": r.opened_at,
                "current": ws.mode == Mode::Local && p == current,
                "git": p.join(".git").exists(),
            })
        })
        .collect();
    Json(serde_json::json!({ "items": items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn paths_expand_home_and_must_be_absolute() {
        let home = Path::new("/home/ann");
        assert_eq!(expand("~", home), Some(PathBuf::from("/home/ann")));
        assert_eq!(
            expand("~/code/api", home),
            Some(PathBuf::from("/home/ann/code/api"))
        );
        assert_eq!(expand("/srv/repo", home), Some(PathBuf::from("/srv/repo")));
        assert_eq!(
            expand("code/api", home),
            None,
            "relative: not the server's cwd"
        );
        assert_eq!(
            expand("~bob/code", home),
            None,
            "another user's home is not expanded"
        );
    }
}
