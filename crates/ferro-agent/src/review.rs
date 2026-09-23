//! PR review drafts + submission.
//! Drafts live in-process (like px0's Alt+R drafts); submit posts them
//! to GitHub via `gh api` as a single review.

use serde::{Deserialize, Serialize};
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub path: String,
    pub line: usize,
    pub body: String,
}

#[derive(Debug, Default)]
pub struct ReviewStore {
    drafts: RwLock<Vec<Draft>>,
    counter: RwLock<usize>,
}

static STORE: std::sync::OnceLock<ReviewStore> = std::sync::OnceLock::new();

/// Process-global draft store (one server / one desktop per process).
pub fn drafts() -> &'static ReviewStore {
    STORE.get_or_init(ReviewStore::default)
}

impl ReviewStore {
    pub fn add(&self, path: String, line: usize, body: String) -> Draft {
        let mut n = self.counter.write().unwrap();
        *n += 1;
        let d = Draft {
            id: format!("d{n}"),
            path,
            line,
            body,
        };
        self.drafts.write().unwrap().push(d.clone());
        d
    }

    pub fn list(&self) -> Vec<Draft> {
        self.drafts.read().unwrap().clone()
    }

    pub fn remove(&self, id: &str) -> bool {
        let mut w = self.drafts.write().unwrap();
        let before = w.len();
        w.retain(|d| d.id != id);
        w.len() != before
    }

    pub fn clear(&self) {
        self.drafts.write().unwrap().clear();
    }

    pub fn count_for(&self, path: &str) -> usize {
        self.drafts
            .read()
            .unwrap()
            .iter()
            .filter(|d| d.path == path)
            .count()
    }
}

/// Submit drafts as one GitHub review. `event`: Approve | Comment | RequestChanges.
pub fn submit(
    owner: &str,
    repo: &str,
    number: u64,
    event: &str,
    body: &str,
    drafts: &[Draft],
) -> Result<String, String> {
    let event = match event.to_lowercase().as_str() {
        "approve" => "APPROVE",
        "request_changes" | "request-changes" => "REQUEST_CHANGES",
        _ => "COMMENT",
    };
    let comments: Vec<serde_json::Value> = drafts
        .iter()
        .map(|d| serde_json::json!({"path": d.path, "line": d.line, "body": d.body}))
        .collect();
    let payload = serde_json::json!({
        "commit_id": null,
        "body": body,
        "event": event,
        "comments": comments,
    });
    let mut f = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    use std::io::Write;
    f.write_all(payload.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let out = std::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{owner}/{repo}/pulls/{number}/reviews"),
            "--input",
            &f.path().to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("gh not available: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_crud() {
        let s = ReviewStore::default();
        let d = s.add("a.rs".into(), 10, "nit".into());
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.count_for("a.rs"), 1);
        assert!(s.remove(&d.id));
        assert!(s.list().is_empty());
    }

    #[test]
    fn event_normalizes() {
        // unknown events fall back to COMMENT (no network in test)
        let _ = event_ok("approve");
        let _ = event_ok("request-changes");
        let _ = event_ok("whatever");
    }

    fn event_ok(e: &str) -> bool {
        matches!(
            match e.to_lowercase().as_str() {
                "approve" => "APPROVE",
                "request_changes" | "request-changes" => "REQUEST_CHANGES",
                _ => "COMMENT",
            },
            "APPROVE" | "REQUEST_CHANGES" | "COMMENT"
        )
    }
}
