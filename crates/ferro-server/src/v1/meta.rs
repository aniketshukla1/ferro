//! GET /api/v1/meta — capabilities and workspace summary (API.md § 3.1).

use axum::{extract::State, routing::get, Json, Router};
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::{AppState, Mode};

/// Exactly the flags this binary implements. The frontend gates on these.
pub const FEATURES: &[&str] = &[
    "v1",
    "events",
    "settings",
    "session",
    "tree",
    "file",
    "hl.classes",
    "hl.exact",
    "markdown.v2",
    "outline",
    "jobs",
    "workspace.open",
    "desktop",
    "metrics",
    "fuzzy.v2",
    "search.v2",
    "search.regex",
    "search.stream",
    "file.find",
    "paths.resolve",
];

fn git_info(root: &std::path::Path) -> (bool, Option<String>, Option<String>) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output();
    let Ok(out) = out else {
        return (false, None, None);
    };
    if !out.status.success() {
        return (false, None, None);
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let sha = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    (true, Some(branch), sha)
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/meta", get(meta))
}

async fn meta(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let (files, ms) = ws.index.stats();
    let (is_repo, branch, head_sha) = match &ws.git {
        Some(g) => {
            let (_, b, h) = git_info(&g.root);
            (true, b, h)
        }
        None => (false, None, None),
    };
    let name = match ws.mode {
        Mode::Local => ws
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        Mode::Pr => "pr".to_string(),
    };
    Ok(Json(serde_json::json!({
        "api": 1,
        "specVersion": s.spec_version,
        "version": s.version,
        "host": match s.host { crate::state::Host::Cli => "cli", crate::state::Host::Desktop(_) => "desktop" },
        "mode": match ws.mode { Mode::Local => "workspace", Mode::Pr => "pr" },
        "readOnly": false,
        "workspace": {
            "root": ws.root.to_string_lossy(),
            "name": name,
            "key": ws.key,
            "git": is_repo,
            "branch": branch,
            "headSha": head_sha,
        },
        "index": {
            "state": if files > 0 { "ready" } else { "indexing" },
            "files": files,
            "ms": ms,
            "generation": ws.generation.load(std::sync::atomic::Ordering::Relaxed),
            "searchIndex": "off",
        },
        "features": FEATURES,
        "limits": {
            "maxWindowLines": s.limits.max_window_lines,
            "maxCols": s.limits.max_cols,
            "maxRawBytes": s.limits.max_raw_bytes,
            "maxMarkdownBytes": s.limits.max_markdown_bytes,
            "maxSearchFiles": s.limits.max_search_files,
            "maxDiffRows": s.limits.max_diff_rows,
        },
        "pr": null,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_are_sorted_unique() {
        let mut v = FEATURES.to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), FEATURES.len());
    }
}
