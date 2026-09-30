//! Mutation audit log (B8): one JSONL line per successful mutation in the state dir.

use axum::{body::Body, http::Request, middleware::Next, response::Response, Extension};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::guard::GuardConfig;

/// Set as a response extension by a handler that knows a better audit target than the URI,
/// e.g. the credential ids a `PUT /credentials` stored. Ids only, never values.
#[derive(Clone)]
pub struct AuditTarget(pub String);

#[derive(Clone)]
pub struct AuditLog {
    path: PathBuf,
    /// One writer at a time so concurrent mutations cannot tear a JSONL line.
    write_lock: std::sync::Arc<std::sync::Mutex<()>>,
}

impl AuditLog {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("audit.jsonl"),
            write_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(
        &self,
        method: &str,
        route: &str,
        actor: &str,
        target: &str,
    ) -> std::io::Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let timestamp = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into());
        let line = serde_json::json!({
            "timestamp": timestamp,
            "method": method,
            "route": route,
            "actor": actor,
            "target": target,
        });
        let mut s = line.to_string();
        s.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(s.as_bytes())
    }
}

fn actor_label(g: &GuardConfig, req: &Request<Body>) -> &'static str {
    use axum::http::header;
    if g.no_auth {
        return "open";
    }
    if req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer "))
    {
        return "bearer";
    }
    "session"
}

fn target_from_uri(path: &str, query: Option<&str>) -> String {
    let mut out = path.to_string();
    if let Some(q) = query {
        for pair in q.split('&') {
            let Some((k, v)) = pair.split_once('=') else {
                continue;
            };
            if k == "path" || k == "dir" || k == "file" {
                out.push('?');
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                break;
            }
        }
    }
    out
}

/// After the handler runs, append an audit line for successful API mutations.
pub async fn audit_mutations(
    Extension(g): Extension<Arc<GuardConfig>>,
    Extension(audit): Extension<Arc<AuditLog>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let actor = actor_label(&g, &req).to_string();
    let target = target_from_uri(uri.path(), uri.query());
    let res = next.run(req).await;
    if g.read_only {
        return res;
    }
    if !path.starts_with("/api/") {
        return res;
    }
    if !crate::contract::read_only_blocks(&method, &path) {
        return res;
    }
    if !res.status().is_success() {
        return res;
    }
    let route = format!("{method} {path}");
    let target = res
        .extensions()
        .get::<AuditTarget>()
        .map_or(target, |t| t.0.clone());
    if let Err(e) = audit.append(method.as_str(), &route, &actor, &target) {
        tracing::warn!("audit log write failed: {e}");
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_line_has_no_raw_body_fields() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path());
        log.append(
            "POST",
            "POST /api/v1/settings",
            "bearer",
            "/api/v1/settings",
        )
        .unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        let line = text.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v.get("timestamp").is_some());
        assert!(v.get("route").is_some());
        assert!(v.get("actor").is_some());
        assert!(v.get("target").is_some());
        assert!(v.get("body").is_none());
        assert!(!text.contains("password"));
    }

    #[test]
    fn concurrent_appends_are_one_object_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path());
        std::thread::scope(|scope| {
            for i in 0..8 {
                let log = log.clone();
                scope.spawn(move || {
                    for n in 0..20 {
                        log.append(
                            "POST",
                            "POST /api/v1/settings",
                            "bearer",
                            &format!("/api/v1/settings?n={i}-{n}"),
                        )
                        .unwrap();
                    }
                });
            }
        });
        let text = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 160);
        for line in lines {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }
}
