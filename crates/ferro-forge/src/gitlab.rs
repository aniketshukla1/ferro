//! GitLab REST v4 client (B4b): MR metadata, discussions → threads,
//! submit via draft notes + bulk publish, approvals.
//! Endpoint shapes follow the official docs (verified 2026-09-25):
//! positions carry base/head/start shas + paths + lines; the server
//! computes line codes, so multi-line ranges need no client digests.

use std::collections::HashMap;
use std::sync::Mutex;

use super::error::ForgeError;
use super::github::{
    Checks, ForgeComment, PullMeta, ReviewComment, ReviewEvent, ReviewThread, SubmitResponse,
};
use super::parse::ForgeRef;

const PROVIDER: &str = "gitlab";

pub struct GitLab {
    http: reqwest::Client,
    api_base: String,
    token: Option<String>,
    etags: Mutex<HashMap<String, (String, Vec<u8>)>>,
}

impl std::fmt::Debug for GitLab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitLab")
            .field("api_base", &self.api_base)
            .field("has_token", &self.token.is_some())
            .finish()
    }
}

impl GitLab {
    pub fn new(api_base: String, token: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("client");
        Self {
            http,
            api_base,
            token,
            etags: Mutex::new(HashMap::new()),
        }
    }

    pub fn for_ref(r: &ForgeRef, token: Option<String>) -> Self {
        Self::new(r.api_base(), token)
    }

    pub fn has_token(&self) -> bool {
        self.token.as_ref().is_some_and(|t| !t.is_empty())
    }

    fn req(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut b = self.http.request(method, url).header("User-Agent", "ferro");
        if let Some(t) = self.token.as_deref() {
            b = b.header("PRIVATE-TOKEN", t);
        }
        b
    }

    fn path(&self, r: &ForgeRef, rest: &str) -> String {
        format!(
            "{}/projects/{}/merge_requests/{}{}",
            self.api_base,
            r.encoded_project(),
            r.number,
            rest
        )
    }

    async fn get(&self, url: &str) -> Result<serde_json::Value, ForgeError> {
        let etag = self.etags.lock().unwrap().get(url).map(|(e, _)| e.clone());
        let mut b = self.req(reqwest::Method::GET, url);
        if let Some(e) = etag {
            b = b.header("If-None-Match", e);
        }
        let resp = b
            .send()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        if status == 304 {
            let body = self
                .etags
                .lock()
                .unwrap()
                .get(url)
                .map(|(_, b)| b.clone())
                .unwrap_or_default();
            return serde_json::from_slice(&body).map_err(|e| ForgeError::Schema(e.to_string()));
        }
        Self::check_status(&resp)?;
        let new_etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?
            .to_vec();
        if let Some(e) = new_etag {
            self.etags
                .lock()
                .unwrap()
                .insert(url.into(), (e, bytes.clone()));
        }
        serde_json::from_slice(&bytes).map_err(|e| ForgeError::Schema(e.to_string()))
    }

    async fn post(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, ForgeError> {
        let resp = self
            .req(reqwest::Method::POST, url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;
        Self::check_status(&resp)?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ForgeError::Network(e.to_string()))?;
        if bytes.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| ForgeError::Schema(e.to_string()))
    }

    fn check_status(resp: &reqwest::Response) -> Result<(), ForgeError> {
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        if status == 401 {
            return Err(ForgeError::Auth);
        }
        if status == 404 {
            return Err(ForgeError::NotFound("gitlab resource".into()));
        }
        if status == 429 {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|s| s * 1000)
                .unwrap_or(60_000);
            return Err(ForgeError::RateLimited {
                retry_after_ms: retry,
            });
        }
        Err(ForgeError::Upstream {
            provider: PROVIDER,
            status,
        })
    }

    // -- MR surface (mirrors GitHub shapes for the server) ---------------------

    pub async fn pull(&self, r: &ForgeRef) -> Result<PullMeta, ForgeError> {
        let v = self.get(&self.path(r, "")).await?;
        Ok(PullMeta::from_gitlab(&v, r))
    }

    async fn approvals(&self, r: &ForgeRef) -> Result<ApprovalState, ForgeError> {
        let v = self.get(&format!("{}/approvals", self.path(r, ""))).await?;
        Ok(ApprovalState {
            approved: v.get("approved").and_then(|b| b.as_bool()).unwrap_or(false),
            count: v
                .get("approved_by")
                .and_then(|a| a.as_array())
                .map(|a| a.len())
                .unwrap_or(0),
        })
    }

    pub async fn can_push(&self, r: &ForgeRef) -> Result<bool, ForgeError> {
        // Developer (30)+ on the SOURCE project can push the head branch.
        let src = self.source_project(r).await?;
        let me: serde_json::Value = self.get(&format!("{}/user", self.api_base)).await?;
        let uid = me
            .get("id")
            .and_then(|i| i.as_u64())
            .ok_or_else(|| ForgeError::Schema("user id".into()))?;
        let m = self
            .get(&format!(
                "{}/projects/{}/members/all/{}",
                self.api_base, src, uid
            ))
            .await;
        match m {
            Ok(m) => Ok(m.get("access_level").and_then(|l| l.as_u64()).unwrap_or(0) >= 30),
            Err(ForgeError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn source_project(&self, r: &ForgeRef) -> Result<String, ForgeError> {
        let v = self.get(&self.path(r, "")).await?;
        let id = v
            .get("source_project_id")
            .and_then(|i| i.as_u64())
            .ok_or_else(|| ForgeError::Schema("source project".into()))?;
        Ok(id.to_string())
    }

    /// Clone URL of the source (head) project, for fork-aware pushes.
    pub async fn source_clone_url(&self, r: &ForgeRef) -> Result<String, ForgeError> {
        let id = self.source_project(r).await?;
        let v = self
            .get(&format!("{}/projects/{}", self.api_base, id))
            .await?;
        v.get("http_url_to_repo")
            .and_then(|u| u.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ForgeError::Schema("clone url".into()))
    }

    pub async fn checks(&self, r: &ForgeRef, _sha: &str) -> Result<Checks, ForgeError> {
        // Approvals stand in for CI here; pipelines stay out of scope.
        match self.approvals(r).await {
            Ok(a) => Ok(Checks {
                state: if a.approved {
                    "success".into()
                } else {
                    "pending".into()
                },
                url: Some(format!(
                    "{}/-/merge_requests/{}/diffs",
                    self.project_url(r).await.unwrap_or_default(),
                    r.number
                )),
            }),
            Err(_) => Ok(Checks {
                state: "none".into(),
                url: None,
            }),
        }
    }

    async fn project_url(&self, r: &ForgeRef) -> Result<String, ForgeError> {
        let v = self
            .get(&format!(
                "{}/projects/{}",
                self.api_base,
                r.encoded_project()
            ))
            .await?;
        Ok(v.get("web_url")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .into())
    }

    pub async fn threads(&self, r: &ForgeRef) -> Result<Vec<ReviewThread>, ForgeError> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let v = self
                .get(&format!(
                    "{}/discussions?per_page=100&page={page}",
                    self.path(r, "")
                ))
                .await?;
            let arr = v.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                break;
            }
            let full = arr.len() == 100;
            for d in &arr {
                if let Some(t) = thread_from_discussion(d) {
                    out.push(t);
                }
            }
            if !full {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    pub async fn conversation(&self, r: &ForgeRef) -> Result<Vec<ForgeComment>, ForgeError> {
        // Non-positioned notes (top-level + commit threads, no diff position).
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let v = self
                .get(&format!(
                    "{}/discussions?per_page=100&page={page}",
                    self.path(r, "")
                ))
                .await?;
            let arr = v.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                break;
            }
            let full = arr.len() == 100;
            for d in &arr {
                for n in d
                    .get("notes")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default()
                {
                    if n.get("system").and_then(|b| b.as_bool()).unwrap_or(false) {
                        continue;
                    }
                    if n.get("position").is_some() {
                        continue;
                    }
                    out.push(gl_comment(&n)?);
                }
            }
            if !full {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Submit via draft notes + bulk publish. `head_sha` pins the review to
    /// the current head (positions carry it; stale heads fail server-side
    /// and surface in `failed`).
    #[allow(clippy::too_many_arguments)]
    pub async fn submit_review(
        &self,
        r: &ForgeRef,
        head_sha: &str,
        event: ReviewEvent,
        body: &str,
        comments: &[ReviewComment],
    ) -> Result<SubmitResponse, ForgeError> {
        let refs = self.diff_refs(r).await?;
        for c in comments {
            let mut position = serde_json::json!({
                "base_sha": refs.base,
                "head_sha": head_sha,
                "start_sha": refs.start,
                "position_type": "text",
                "new_path": c.path,
                "old_path": c.path,
            });
            match (&c.line, &c.side) {
                (Some(l), Some(s)) if s == "LEFT" => {
                    position["old_line"] = (*l).into();
                }
                (Some(l), _) => {
                    position["new_line"] = (*l).into();
                }
                (None, _) => {}
            }
            if let (Some(sl), Some(el)) = (c.start_line, c.line) {
                if sl < el {
                    let side = c.side.as_deref().unwrap_or("RIGHT");
                    let (ot, nt) = if side == "LEFT" {
                        ("old", "old")
                    } else {
                        ("new", "new")
                    };
                    position["line_range"] = serde_json::json!({
                        "start": { "type": ot, "old_line": if ot == "old" { Some(sl) } else { None }, "new_line": if ot == "new" { Some(sl) } else { None } },
                        "end": { "type": nt, "old_line": if nt == "old" { Some(el) } else { None }, "new_line": if nt == "new" { Some(el) } else { None } },
                    });
                }
            }
            self.post(
                &format!("{}/draft_notes", self.path(r, "")),
                serde_json::json!({ "note": c.body, "position": position }),
            )
            .await?;
        }
        // Reply drafts arrive as comments with thread ids handled by the
        // server layer through `reply` before this call.
        let mut bulk = serde_json::json!({});
        if !body.is_empty() {
            bulk["note"] = body.into();
        }
        match event {
            ReviewEvent::Comment => {}
            ReviewEvent::Approve => {
                self.post(
                    &format!("{}/approve", self.path(r, "")),
                    serde_json::json!({}),
                )
                .await?;
            }
            ReviewEvent::RequestChanges => {
                bulk["reviewer_state"] = "requested_changes".into();
            }
        }
        self.post(
            &format!("{}/draft_notes/bulk_publish", self.path(r, "")),
            bulk,
        )
        .await?;
        Ok(SubmitResponse {
            id: 0,
            html_url: r.html_url(),
            state: event.as_str().into(),
        })
    }

    /// Reply inside a discussion (discussion id string).
    pub async fn reply(
        &self,
        r: &ForgeRef,
        discussion_id: &str,
        body: &str,
    ) -> Result<ForgeComment, ForgeError> {
        let v = self
            .post(
                &format!("{}/discussions/{}/notes", self.path(r, ""), discussion_id),
                serde_json::json!({ "body": body }),
            )
            .await?;
        gl_comment(&v)
    }

    /// Reply-targeting draft note (used for thread replies at submit time).
    pub async fn reply_draft(
        &self,
        r: &ForgeRef,
        discussion_id: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.post(
            &format!("{}/draft_notes", self.path(r, "")),
            serde_json::json!({ "note": body, "in_reply_to_discussion_id": discussion_id }),
        )
        .await?;
        Ok(())
    }

    pub async fn post_comment(&self, r: &ForgeRef, body: &str) -> Result<ForgeComment, ForgeError> {
        let v = self
            .post(
                &format!("{}/notes", self.path(r, "")),
                serde_json::json!({ "body": body }),
            )
            .await?;
        gl_comment(&v)
    }

    async fn diff_refs(&self, r: &ForgeRef) -> Result<DiffRefs, ForgeError> {
        let v = self.get(&self.path(r, "")).await?;
        Ok(DiffRefs {
            base: v
                .get("diff_refs")
                .and_then(|d| d.get("base_sha"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .into(),
            head: v
                .get("diff_refs")
                .and_then(|d| d.get("head_sha"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .into(),
            start: v
                .get("diff_refs")
                .and_then(|d| d.get("start_sha"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .into(),
        })
    }
}

struct DiffRefs {
    base: String,
    #[allow(dead_code)]
    head: String,
    start: String,
}

struct ApprovalState {
    approved: bool,
    #[allow(dead_code)]
    count: usize,
}

/// A positioned, non-system discussion becomes one review thread.
fn thread_from_discussion(d: &serde_json::Value) -> Option<ReviewThread> {
    let notes = d.get("notes")?.as_array()?;
    let first = notes
        .iter()
        .find(|n| !n.get("system").and_then(|b| b.as_bool()).unwrap_or(false))?;
    let pos = first.get("position")?;
    if pos.get("position_type").and_then(|t| t.as_str()) != Some("text") {
        return None;
    }
    let new_path = pos.get("new_path").and_then(|p| p.as_str()).unwrap_or("");
    let old_path = pos.get("old_path").and_then(|p| p.as_str()).unwrap_or("");
    let (new_line, old_line) = range_lines(pos);
    let side = if new_line.is_some() { "RIGHT" } else { "LEFT" };
    let line = new_line.or(old_line);
    let mut comments = Vec::new();
    for n in notes {
        if n.get("system").and_then(|b| b.as_bool()).unwrap_or(false) {
            continue;
        }
        comments.push(gl_comment(n).ok()?);
    }
    if comments.is_empty() {
        return None;
    }
    Some(ReviewThread {
        id: d.get("id").and_then(|i| i.as_str()).unwrap_or("").into(),
        path: if !new_path.is_empty() {
            new_path.into()
        } else {
            old_path.into()
        },
        line,
        start_line: range_start(pos),
        side: side.into(),
        original_line: old_line,
        commit_sha: None,
        outdated: pos
            .get("outdated")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        resolved: d
            .get("resolved")
            .and_then(|b| b.as_bool())
            .unwrap_or_else(|| {
                notes
                    .iter()
                    .any(|n| n.get("resolved").and_then(|b| b.as_bool()).unwrap_or(false))
            }),
        comments,
    })
}

/// (new_line, old_line) honoring line_range ends when present.
fn range_lines(pos: &serde_json::Value) -> (Option<u64>, Option<u64>) {
    if let Some(range) = pos.get("line_range") {
        let end = &range["end"];
        let t = end.get("type").and_then(|x| x.as_str()).unwrap_or("new");
        let l = end
            .get("new_line")
            .and_then(|n| n.as_u64())
            .or_else(|| end.get("old_line").and_then(|n| n.as_u64()));
        if t == "old" {
            return (None, l);
        }
        return (l, None);
    }
    (
        pos.get("new_line").and_then(|n| n.as_u64()),
        pos.get("old_line").and_then(|n| n.as_u64()),
    )
}

fn range_start(pos: &serde_json::Value) -> Option<u64> {
    let range = pos.get("line_range")?;
    let start = &range["start"];
    start
        .get("new_line")
        .and_then(|n| n.as_u64())
        .or_else(|| start.get("old_line").and_then(|n| n.as_u64()))
}

fn gl_comment(v: &serde_json::Value) -> Result<ForgeComment, ForgeError> {
    Ok(ForgeComment {
        id: v
            .get("id")
            .and_then(|i| i.as_u64())
            .map(|i| i.to_string())
            .or_else(|| v.get("id").and_then(|i| i.as_str()).map(|s| s.into()))
            .ok_or_else(|| ForgeError::Schema("note without id".into()))?,
        author_login: v
            .get("author")
            .and_then(|a| a.get("username"))
            .and_then(|l| l.as_str())
            .unwrap_or("")
            .into(),
        author_avatar: v
            .get("author")
            .and_then(|a| a.get("avatar_url"))
            .and_then(|l| l.as_str())
            .map(|s| s.into()),
        body: v.get("body").and_then(|b| b.as_str()).unwrap_or("").into(),
        created_at: v
            .get("created_at")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .into(),
        url: v.get("url").and_then(|u| u.as_str()).unwrap_or("").into(),
    })
}

impl PullMeta {
    pub(crate) fn from_gitlab(v: &serde_json::Value, r: &ForgeRef) -> Self {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let state_raw = s("state");
        let (state, merged) = match state_raw.as_str() {
            "merged" => ("merged", true),
            "closed" => ("closed", false),
            _ => ("open", false),
        };
        let (adds, dels, files) = count_changes(v);
        Self {
            title: s("title"),
            body: v
                .get("description")
                .and_then(|b| b.as_str())
                .map(|x| x.into()),
            state: state.into(),
            merged,
            draft: v.get("draft").and_then(|b| b.as_bool()).unwrap_or(false)
                || v.get("work_in_progress")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false),
            base_ref: s("target_branch"),
            head_ref: s("source_branch"),
            base_sha: v
                .get("diff_refs")
                .and_then(|d| d.get("base_sha"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .into(),
            head_sha: v
                .get("diff_refs")
                .and_then(|d| d.get("head_sha"))
                .and_then(|x| x.as_str())
                .unwrap_or_else(|| v.get("sha").and_then(|x| x.as_str()).unwrap_or(""))
                .into(),
            head_clone_url: None, // resolved by the server from the source project
            is_fork: v.get("source_project_id").and_then(|i| i.as_u64())
                != v.get("target_project_id").and_then(|i| i.as_u64()),
            created_at: s("created_at"),
            updated_at: s("updated_at"),
            additions: adds,
            deletions: dels,
            changed_files: files,
            commits: v
                .get("diverged_commits_count")
                .and_then(|n| n.as_u64())
                .unwrap_or(0),
            html_url: v
                .get("web_url")
                .and_then(|u| u.as_str())
                .unwrap_or(&r.html_url())
                .to_string(),
            author_login: v
                .get("author")
                .and_then(|a| a.get("username"))
                .and_then(|l| l.as_str())
                .unwrap_or("")
                .into(),
            author_avatar: v
                .get("author")
                .and_then(|a| a.get("avatar_url"))
                .and_then(|l| l.as_str())
                .map(|x| x.into()),
        }
    }
}

/// Count +/- lines from the embedded `changes[].diff` hunks.
fn count_changes(v: &serde_json::Value) -> (u64, u64, u64) {
    let mut adds = 0u64;
    let mut dels = 0u64;
    let mut files = 0u64;
    for c in v
        .get("changes")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default()
    {
        files += 1;
        let diff = c.get("diff").and_then(|d| d.as_str()).unwrap_or("");
        for line in diff.lines() {
            if let Some(b) = line.strip_prefix('+') {
                if !b.starts_with("++") {
                    adds += 1;
                }
            } else if let Some(b) = line.strip_prefix('-') {
                if !b.starts_with("--") {
                    dels += 1;
                }
            }
        }
    }
    (adds, dels, files)
}
