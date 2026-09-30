//! GitLab REST v4 client (B4b): MR metadata, discussions → threads,
//! submit via draft notes + bulk publish, approvals.
//! Endpoint shapes follow the official docs (verified 2026-09-26): a
//! position carries the base/start/head shas of one MR diff version; an
//! unchanged line needs both `old_line` and `new_line`, an added line only
//! `new_line`, a removed one only `old_line`; multi-line ranges carry a
//! `line_code` and `type` per end.

use std::collections::HashMap;
use std::sync::Mutex;

use super::error::ForgeError;
use super::github::{
    Checks, ForgeComment, PullMeta, ReviewComment, ReviewEvent, ReviewThread, SubmitResponse,
};
use super::parse::ForgeRef;

const PROVIDER: &str = "gitlab";
/// Page cap for list reads (100 items a page).
const MAX_PAGES: u32 = 30;

pub struct GitLab {
    http: reqwest::Client,
    api_base: String,
    token: Option<String>,
    etags: Mutex<HashMap<String, (String, Vec<u8>)>>,
    /// Source project id → `http_url_to_repo` (fixed for an MR's life).
    clone_urls: Mutex<HashMap<u64, String>>,
    /// MR size at one head sha: the MR JSON carries no line counts.
    stats: Mutex<Option<(String, Stats)>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Stats {
    files: u64,
    additions: u64,
    deletions: u64,
    commits: u64,
}

impl std::fmt::Debug for GitLab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitLab")
            .field("api_base", &self.api_base)
            .field("has_token", &self.token.is_some())
            .finish()
    }
}

fn net(e: reqwest::Error) -> ForgeError {
    ForgeError::Network(e.to_string())
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
            clone_urls: Mutex::new(HashMap::new()),
            stats: Mutex::new(None),
        }
    }

    pub fn for_ref(r: &ForgeRef, token: Option<String>) -> Self {
        // Integration tests point this at a local mock server.
        if let Ok(base) = std::env::var("FERRO_FORGE_API_BASE") {
            return Self::new(base.trim_end_matches('/').to_string(), token);
        }
        Self::new(r.api_base(), token)
    }

    pub fn has_token(&self) -> bool {
        self.token.as_ref().is_some_and(|t| !t.is_empty())
    }

    /// Bearer works for every GitLab token kind; `PRIVATE-TOKEN` rejects
    /// OAuth tokens (what a glab web login stores).
    fn req(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut b = self.http.request(method, url).header("User-Agent", "ferro");
        if let Some(t) = self.token.as_deref() {
            b = b.bearer_auth(t);
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

    /// GET with ETag caching: 304 reuses the stored body.
    async fn get(&self, url: &str) -> Result<serde_json::Value, ForgeError> {
        let etag = self.etags.lock().unwrap().get(url).map(|(e, _)| e.clone());
        let mut b = self.req(reqwest::Method::GET, url);
        if let Some(e) = etag {
            b = b.header("If-None-Match", e);
        }
        let resp = b.send().await.map_err(net)?;
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
        let bytes = resp.bytes().await.map_err(net)?.to_vec();
        if let Some(e) = new_etag {
            self.etags
                .lock()
                .unwrap()
                .insert(url.into(), (e, bytes.clone()));
        }
        serde_json::from_slice(&bytes).map_err(|e| ForgeError::Schema(e.to_string()))
    }

    /// GET outside the ETag cache: state that must be current (approvals,
    /// versions) and lists read once per head (diffs, commits).
    async fn get_fresh(&self, url: &str) -> Result<serde_json::Value, ForgeError> {
        let resp = self
            .req(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(net)?;
        Self::check_status(&resp)?;
        let bytes = resp.bytes().await.map_err(net)?;
        serde_json::from_slice(&bytes).map_err(|e| ForgeError::Schema(e.to_string()))
    }

    /// Every page of a list endpoint (100 a page, capped), uncached.
    async fn list(&self, url: &str) -> Result<Vec<serde_json::Value>, ForgeError> {
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let v = self
                .get_fresh(&format!("{url}?per_page=100&page={page}"))
                .await?;
            let arr = v
                .as_array()
                .ok_or_else(|| ForgeError::Schema("expected a list".into()))?;
            out.extend(arr.iter().cloned());
            if arr.len() < 100 {
                break;
            }
        }
        Ok(out)
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
            .map_err(net)?;
        Self::check_status(&resp)?;
        let bytes = resp.bytes().await.map_err(net)?;
        if bytes.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| ForgeError::Schema(e.to_string()))
    }

    async fn delete(&self, url: &str) -> Result<(), ForgeError> {
        let resp = self
            .req(reqwest::Method::DELETE, url)
            .send()
            .await
            .map_err(net)?;
        Self::check_status(&resp)
    }

    fn check_status(resp: &reqwest::Response) -> Result<(), ForgeError> {
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        if status == 401 {
            return Err(ForgeError::Auth { provider: PROVIDER });
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
        let mut meta = PullMeta::from_gitlab(&v, r);
        // Neither the source project's clone URL (fork-aware pushes) nor
        // line counts are in the MR JSON. Both are cached (per project, per
        // head), so a poll stays one request.
        meta.head_clone_url = self.source_clone_url(&v).await;
        if let Some(s) = self.stats(r, &meta.head_sha).await {
            meta.changed_files = s.files;
            meta.additions = s.additions;
            meta.deletions = s.deletions;
            meta.commits = s.commits;
        }
        Ok(meta)
    }

    /// Clone URL of the MR's source (head) project; None when unreachable.
    async fn source_clone_url(&self, mr: &serde_json::Value) -> Option<String> {
        let id = mr.get("source_project_id")?.as_u64()?;
        if let Some(u) = self.clone_urls.lock().unwrap().get(&id) {
            return Some(u.clone());
        }
        let v = self
            .get_fresh(&format!("{}/projects/{id}", self.api_base))
            .await
            .ok()?;
        let url = v.get("http_url_to_repo")?.as_str()?.to_string();
        self.clone_urls.lock().unwrap().insert(id, url.clone());
        Some(url)
    }

    /// Size at `head`: files and +/- lines from `/diffs` (GitLab 15.7+,
    /// else the older `/changes`), commits from `/commits`.
    async fn stats(&self, r: &ForgeRef, head: &str) -> Option<Stats> {
        if let Some((h, s)) = self.stats.lock().unwrap().as_ref() {
            if h == head {
                return Some(*s);
            }
        }
        let diffs = match self.list(&self.path(r, "/diffs")).await {
            Ok(d) => d,
            Err(ForgeError::NotFound(_)) => self
                .get_fresh(&self.path(r, "/changes"))
                .await
                .ok()?
                .get("changes")?
                .as_array()?
                .clone(),
            Err(_) => return None,
        };
        let mut s = Stats {
            files: diffs.len() as u64,
            ..Stats::default()
        };
        for d in &diffs {
            let (adds, dels) = diff_lines(d.get("diff").and_then(|x| x.as_str()).unwrap_or(""));
            s.additions += adds;
            s.deletions += dels;
        }
        s.commits = self.list(&self.path(r, "/commits")).await.ok()?.len() as u64;
        *self.stats.lock().unwrap() = Some((head.to_string(), s));
        Some(s)
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

    /// CI state of the MR's head pipeline (`head_pipeline` in the MR JSON);
    /// "none" until a pipeline runs for `sha`.
    pub async fn checks(&self, r: &ForgeRef, sha: &str) -> Result<Checks, ForgeError> {
        let v = self.get(&self.path(r, "")).await?;
        Ok(pipeline_checks(v.get("head_pipeline"), sha))
    }

    /// Threads and conversation from one pass over the discussions.
    pub async fn review_state(
        &self,
        r: &ForgeRef,
    ) -> Result<(Vec<ReviewThread>, Vec<ForgeComment>), ForgeError> {
        let head = self.current_head(r).await?;
        let discussions = self.list(&self.path(r, "/discussions")).await?;
        Ok((
            threads_of(&discussions, &head),
            conversation_of(&discussions)?,
        ))
    }

    pub async fn threads(&self, r: &ForgeRef) -> Result<Vec<ReviewThread>, ForgeError> {
        let head = self.current_head(r).await?;
        let discussions = self.list(&self.path(r, "/discussions")).await?;
        Ok(threads_of(&discussions, &head))
    }

    pub async fn conversation(&self, r: &ForgeRef) -> Result<Vec<ForgeComment>, ForgeError> {
        conversation_of(&self.list(&self.path(r, "/discussions")).await?)
    }

    /// The MR's latest head: positions on any other head are outdated.
    async fn current_head(&self, r: &ForgeRef) -> Result<String, ForgeError> {
        let v = self.get(&self.path(r, "")).await?;
        Ok(v.get("diff_refs")
            .and_then(|d| d.get("head_sha"))
            .or_else(|| v.get("sha"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .into())
    }

    /// Submit via draft notes + bulk publish, pinned to `head_sha`: every
    /// position carries that MR version's own base/start/head triple.
    /// Drafts staged here are deleted again when a later step fails, so a
    /// retry never publishes a second copy.
    pub async fn submit_review(
        &self,
        r: &ForgeRef,
        head_sha: &str,
        event: ReviewEvent,
        body: &str,
        comments: &[ReviewComment],
    ) -> Result<SubmitResponse, ForgeError> {
        // Read-only checks first: nothing is staged for a submit GitLab
        // would refuse.
        let approve = event == ReviewEvent::Approve && self.needs_approval(r).await?;
        let version = self.version(r, head_sha).await?;
        let files = if comments.is_empty() {
            Vec::new()
        } else {
            self.version_diffs(r, &version).await
        };
        let mut staged = Vec::new();
        let res = self
            .stage_and_publish(
                r,
                (&version, head_sha, &files),
                event,
                approve,
                body,
                comments,
                &mut staged,
            )
            .await;
        if res.is_err() {
            self.discard_drafts(r, &staged).await;
        }
        res.map(|()| SubmitResponse {
            id: 0,
            html_url: r.html_url(),
            state: event.as_str().into(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn stage_and_publish(
        &self,
        r: &ForgeRef,
        (version, head_sha, files): (&Version, &str, &[serde_json::Value]),
        event: ReviewEvent,
        approve: bool,
        body: &str,
        comments: &[ReviewComment],
        staged: &mut Vec<u64>,
    ) -> Result<(), ForgeError> {
        for c in comments {
            let mut note = serde_json::json!({ "note": c.body });
            if c.line.is_some() {
                let file = files.iter().find(|f| {
                    ["new_path", "old_path"]
                        .iter()
                        .any(|k| f.get(*k).and_then(|p| p.as_str()) == Some(c.path.as_str()))
                });
                note["position"] = position(version, head_sha, file, c);
            }
            let v = self.post(&self.path(r, "/draft_notes"), note).await?;
            staged.extend(v.get("id").and_then(|i| i.as_u64()));
        }
        // Approve before publishing: a failed approve then leaves nothing
        // posted for a retry to duplicate. The token just worked, so a 401
        // here is GitLab declining the approval.
        if approve {
            self.post(&self.path(r, "/approve"), serde_json::json!({}))
                .await
                .map_err(|e| match e {
                    ForgeError::Auth { .. } => ForgeError::Refused {
                        provider: PROVIDER,
                        reason: "approval declined".into(),
                    },
                    e => e,
                })?;
        }
        let mut bulk = serde_json::json!({});
        if !body.is_empty() {
            bulk["note"] = body.into();
        }
        if event == ReviewEvent::RequestChanges {
            bulk["reviewer_state"] = "requested_changes".into();
        }
        self.post(&self.path(r, "/draft_notes/bulk_publish"), bulk)
            .await?;
        Ok(())
    }

    /// Whether an APPROVE submit still has to approve: not when the user
    /// already did (a retry, or the web UI). Refused up front when GitLab
    /// would decline — it answers that approve with a 401.
    async fn needs_approval(&self, r: &ForgeRef) -> Result<bool, ForgeError> {
        let v = self.get_fresh(&self.path(r, "/approvals")).await?;
        if v.get("user_has_approved").and_then(|b| b.as_bool()) == Some(true) {
            return Ok(false);
        }
        if v.get("user_can_approve").and_then(|b| b.as_bool()) == Some(false) {
            return Err(ForgeError::Refused {
                provider: PROVIDER,
                reason: "you cannot approve this merge request".into(),
            });
        }
        Ok(true)
    }

    /// The MR diff version whose head is `head_sha`; the latest version's
    /// refs when none is (best effort).
    async fn version(&self, r: &ForgeRef, head_sha: &str) -> Result<Version, ForgeError> {
        if let Ok(list) = self.list(&self.path(r, "/versions")).await {
            let str_of = |v: &serde_json::Value, k: &str| {
                v.get(k).and_then(|s| s.as_str()).unwrap_or("").to_string()
            };
            if let Some(v) = list
                .iter()
                .find(|v| v.get("head_commit_sha").and_then(|s| s.as_str()) == Some(head_sha))
            {
                return Ok(Version {
                    id: v.get("id").and_then(|i| i.as_u64()),
                    base: str_of(v, "base_commit_sha"),
                    start: str_of(v, "start_commit_sha"),
                });
            }
        }
        let v = self.get(&self.path(r, "")).await?;
        let refs = |k: &str| {
            v.get("diff_refs")
                .and_then(|d| d.get(k))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string()
        };
        Ok(Version {
            id: None,
            base: refs("base_sha"),
            start: refs("start_sha"),
        })
    }

    /// File diffs of `version` (the latest ones without an id); empty when
    /// unavailable, which leaves positions to the side-only fallback.
    async fn version_diffs(&self, r: &ForgeRef, version: &Version) -> Vec<serde_json::Value> {
        let diffs = match version.id {
            Some(id) => self
                .get_fresh(&self.path(r, &format!("/versions/{id}")))
                .await
                .ok()
                .and_then(|v| v.get("diffs").and_then(|d| d.as_array()).cloned()),
            None => self.list(&self.path(r, "/diffs")).await.ok(),
        };
        diffs.unwrap_or_default()
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
                &self.path(r, &format!("/discussions/{discussion_id}/notes")),
                serde_json::json!({ "body": body }),
            )
            .await?;
        gl_comment(&v)
    }

    /// Stage a reply as a draft note (bulk publish posts it with the
    /// review). Returns the draft note id, for [`Self::discard_drafts`].
    pub async fn reply_draft(
        &self,
        r: &ForgeRef,
        discussion_id: &str,
        body: &str,
    ) -> Result<u64, ForgeError> {
        let v = self
            .post(
                &self.path(r, "/draft_notes"),
                serde_json::json!({ "note": body, "in_reply_to_discussion_id": discussion_id }),
            )
            .await?;
        v.get("id")
            .and_then(|i| i.as_u64())
            .ok_or_else(|| ForgeError::Schema("draft note without id".into()))
    }

    /// Best-effort delete of staged draft notes (an unpublished review).
    pub async fn discard_drafts(&self, r: &ForgeRef, ids: &[u64]) {
        for id in ids {
            let _ = self
                .delete(&self.path(r, &format!("/draft_notes/{id}")))
                .await;
        }
    }

    pub async fn post_comment(&self, r: &ForgeRef, body: &str) -> Result<ForgeComment, ForgeError> {
        let v = self
            .post(&self.path(r, "/notes"), serde_json::json!({ "body": body }))
            .await?;
        gl_comment(&v)
    }
}

/// One MR diff version: the base/start shas a position pairs with its
/// head, and the id that serves that version's file diffs.
struct Version {
    id: Option<u64>,
    base: String,
    start: String,
}

fn pipeline_checks(pipeline: Option<&serde_json::Value>, sha: &str) -> Checks {
    let none = Checks {
        state: "none".into(),
        url: None,
    };
    let Some(p) = pipeline.filter(|p| p.is_object()) else {
        return none;
    };
    // A pipeline for an older head says nothing about this one.
    if p.get("sha")
        .and_then(|s| s.as_str())
        .is_some_and(|s| !sha.is_empty() && s != sha)
    {
        return none;
    }
    let state = match p.get("status").and_then(|s| s.as_str()).unwrap_or("") {
        "success" => "success",
        "failed" | "canceled" | "canceling" => "failure",
        "skipped" | "" => "none",
        // created, waiting_for_resource, preparing, pending, running,
        // manual, scheduled
        _ => "pending",
    };
    Checks {
        state: state.into(),
        url: p
            .get("web_url")
            .and_then(|u| u.as_str())
            .map(|s| s.to_string()),
    }
}

/// Where a line sits in one file diff, by GitLab's counters: for an added
/// line `old` is the next old line; for a removed one `new` the next new.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinePos {
    old: u64,
    new: u64,
    kind: LineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Added,
    Removed,
    Unchanged,
}

/// Locate `line` (new side when `right`, else old) in a unified diff.
/// Lines outside every hunk are unchanged, offset by the hunks before them.
fn locate(diff: &str, right: bool, line: u64) -> LinePos {
    let (mut old, mut new) = (1u64, 1u64);
    let unchanged = |old: u64, new: u64| {
        let (o, n) = if right {
            ((line + old).saturating_sub(new), line)
        } else {
            (line, (line + new).saturating_sub(old))
        };
        LinePos {
            old: o,
            new: n,
            kind: LineKind::Unchanged,
        }
    };
    for l in diff.lines() {
        if let Some((o_start, n_start)) = hunk_starts(l) {
            // The gap before this hunk is unchanged.
            if (right && line < n_start) || (!right && line < o_start) {
                return unchanged(old, new);
            }
            (old, new) = (o_start, n_start);
            continue;
        }
        let at = |kind| LinePos { old, new, kind };
        match l.as_bytes().first() {
            Some(b'+') => {
                if right && new == line {
                    return at(LineKind::Added);
                }
                new += 1;
            }
            Some(b'-') => {
                if !right && old == line {
                    return at(LineKind::Removed);
                }
                old += 1;
            }
            Some(b'\\') => {}
            _ => {
                if (right && new == line) || (!right && old == line) {
                    return at(LineKind::Unchanged);
                }
                old += 1;
                new += 1;
            }
        }
    }
    unchanged(old, new)
}

/// `(old_start, new_start)` of a `@@ -a,b +c,d @@` header.
fn hunk_starts(l: &str) -> Option<(u64, u64)> {
    let rest = l.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split(' ').next()?;
    let start = |s: &str| s.split(',').next()?.parse::<u64>().ok();
    Some((start(old)?, start(new)?))
}

/// A draft note position for `c`, in `version` pinned to `head_sha`.
/// Without the file's diff, fall back to the side alone (added / removed).
fn position(
    version: &Version,
    head_sha: &str,
    file: Option<&serde_json::Value>,
    c: &ReviewComment,
) -> serde_json::Value {
    let right = c.side.as_deref() != Some("LEFT");
    let path_of = |k: &str| {
        file.and_then(|f| f.get(k))
            .and_then(|p| p.as_str())
            .filter(|p| !p.is_empty())
            .unwrap_or(&c.path)
            .to_string()
    };
    let (old_path, new_path) = (path_of("old_path"), path_of("new_path"));
    let at = |line: u64| match file.and_then(|f| f.get("diff")).and_then(|d| d.as_str()) {
        Some(diff) => locate(diff, right, line),
        None => LinePos {
            old: line,
            new: line,
            kind: if right {
                LineKind::Added
            } else {
                LineKind::Removed
            },
        },
    };
    let end_line = c.line.unwrap_or(1);
    let end = at(end_line);
    let mut p = serde_json::json!({
        "base_sha": version.base,
        "start_sha": version.start,
        "head_sha": head_sha,
        "position_type": "text",
        "old_path": old_path,
        "new_path": new_path,
    });
    if end.kind != LineKind::Added {
        p["old_line"] = end.old.into();
    }
    if end.kind != LineKind::Removed {
        p["new_line"] = end.new.into();
    }
    if let Some(start_line) = c.start_line.filter(|s| *s < end_line) {
        let range_end = |l: LinePos| {
            serde_json::json!({
                "line_code": format!("{}_{}_{}", sha1_hex(new_path.as_bytes()), l.old, l.new),
                "type": if l.kind == LineKind::Added { "new" } else { "old" },
                "old_line": (l.kind != LineKind::Added).then_some(l.old),
                "new_line": (l.kind != LineKind::Removed).then_some(l.new),
            })
        };
        p["line_range"] = serde_json::json!({
            "start": range_end(at(start_line)),
            "end": range_end(end),
        });
    }
    p
}

/// SHA-1 hex digest: GitLab line codes are `<sha1(path)>_<old>_<new>`.
fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn threads_of(discussions: &[serde_json::Value], head: &str) -> Vec<ReviewThread> {
    discussions
        .iter()
        .filter_map(|d| thread_from_discussion(d, head))
        .collect()
}

/// Non-positioned, non-system notes (top-level + commit threads).
fn conversation_of(discussions: &[serde_json::Value]) -> Result<Vec<ForgeComment>, ForgeError> {
    let mut out = Vec::new();
    for d in discussions {
        for n in d
            .get("notes")
            .and_then(|x| x.as_array())
            .map(|a| a.as_slice())
            .unwrap_or_default()
        {
            if n.get("system").and_then(|b| b.as_bool()).unwrap_or(false)
                || n.get("position").is_some_and(|p| !p.is_null())
            {
                continue;
            }
            out.push(gl_comment(n)?);
        }
    }
    Ok(out)
}

/// A positioned, non-system discussion becomes one review thread. Its
/// position is outdated when it sits on another head than the MR's latest
/// (GitLab moves still-valid positions forward on every push).
fn thread_from_discussion(d: &serde_json::Value, head: &str) -> Option<ReviewThread> {
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
    let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(|n| n.as_u64());
    // The anchor is the position's own line (a range's last line).
    let (new_line, old_line) = (num(pos, "new_line"), num(pos, "old_line"));
    let right = new_line.is_some();
    let start_line = pos
        .get("line_range")
        .and_then(|r| r.get("start"))
        .and_then(|s| {
            if right {
                num(s, "new_line").or_else(|| num(s, "old_line"))
            } else {
                num(s, "old_line").or_else(|| num(s, "new_line"))
            }
        });
    let pos_head = pos.get("head_sha").and_then(|s| s.as_str());
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
        line: new_line.or(old_line),
        start_line: start_line.filter(|s| Some(*s) != new_line.or(old_line)),
        side: if right { "RIGHT" } else { "LEFT" }.into(),
        original_line: old_line,
        commit_sha: pos_head.map(|s| s.to_string()),
        outdated: pos_head.is_some_and(|h| !head.is_empty() && h != head),
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
    /// Metadata from `GET merge_requests/:iid`. Line and commit counts are
    /// not in it (only `changes_count`, a string like "1000+"): the client
    /// fills them from `/diffs` and `/commits`.
    pub(crate) fn from_gitlab(v: &serde_json::Value, r: &ForgeRef) -> Self {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let state_raw = s("state");
        let (state, merged) = match state_raw.as_str() {
            "merged" => ("merged", true),
            "closed" => ("closed", false),
            _ => ("open", false),
        };
        let changes_count = s("changes_count");
        let digits = changes_count.trim_end_matches('+');
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
            head_clone_url: None, // the client resolves the source project's
            is_fork: v.get("source_project_id").and_then(|i| i.as_u64())
                != v.get("target_project_id").and_then(|i| i.as_u64()),
            created_at: s("created_at"),
            updated_at: s("updated_at"),
            additions: 0,
            deletions: 0,
            changed_files: digits.parse().unwrap_or(0),
            commits: 0,
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

/// Added and removed lines of one file's diff (GitLab diffs carry no
/// `---`/`+++` headers, so every `+`/`-` line counts).
fn diff_lines(diff: &str) -> (u64, u64) {
    let mut adds = 0u64;
    let mut dels = 0u64;
    for line in diff.lines() {
        match line.as_bytes().first() {
            Some(b'+') => adds += 1,
            Some(b'-') => dels += 1,
            _ => {}
        }
    }
    (adds, dels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_known_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    const DIFF: &str = "@@ -3,4 +3,5 @@ fn x\n ctx3\n-old4\n+new4\n+new5\n ctx5\n ctx6\n@@ -20,2 +21,1 @@\n ctx20\n-old21\n";

    #[test]
    fn locate_follows_gitlab_counters() {
        let pos = |old, new, kind| LinePos { old, new, kind };
        use LineKind::*;
        // Inside hunks: context has both, added/removed one real side.
        assert_eq!(locate(DIFF, true, 3), pos(3, 3, Unchanged));
        assert_eq!(locate(DIFF, true, 4), pos(5, 4, Added));
        assert_eq!(locate(DIFF, true, 5), pos(5, 5, Added));
        assert_eq!(locate(DIFF, false, 4), pos(4, 4, Removed));
        assert_eq!(locate(DIFF, true, 6), pos(5, 6, Unchanged));
        assert_eq!(locate(DIFF, false, 21), pos(21, 22, Removed));
        // Outside hunks: unchanged, offset by the hunks before.
        assert_eq!(locate(DIFF, true, 1), pos(1, 1, Unchanged));
        assert_eq!(locate(DIFF, true, 12), pos(11, 12, Unchanged));
        assert_eq!(locate(DIFF, false, 11), pos(11, 12, Unchanged));
        assert_eq!(locate(DIFF, true, 30), pos(30, 30, Unchanged));
        // New file: removed side starts at 0.
        assert_eq!(
            locate("@@ -0,0 +1,2 @@\n+a\n+b\n", true, 2),
            pos(0, 2, Added)
        );
    }

    #[test]
    fn positions_carry_both_lines_for_unchanged_code() {
        let v = Version {
            id: Some(1),
            base: "b".into(),
            start: "s".into(),
        };
        let file = serde_json::json!({"old_path": "old.rs", "new_path": "new.rs", "diff": DIFF});
        let c = |line, start_line, side: &str| ReviewComment {
            path: "new.rs".into(),
            body: "x".into(),
            line: Some(line),
            side: Some(side.into()),
            start_line,
            start_side: start_line.map(|_| side.into()),
        };
        // Context line on the right: both lines, renamed paths kept.
        let p = position(&v, "h", Some(&file), &c(6, None, "RIGHT"));
        assert_eq!(
            (p["old_line"].as_u64(), p["new_line"].as_u64()),
            (Some(5), Some(6))
        );
        assert_eq!(
            (p["old_path"].as_str(), p["new_path"].as_str()),
            (Some("old.rs"), Some("new.rs"))
        );
        // Added line: new_line only.
        let p = position(&v, "h", Some(&file), &c(4, None, "RIGHT"));
        assert!(p.get("old_line").is_none() && p["new_line"] == 4);
        // Removed line: old_line only.
        let p = position(&v, "h", Some(&file), &c(4, None, "LEFT"));
        assert!(p.get("new_line").is_none() && p["old_line"] == 4);
        // A range: line codes over the new path, `new` only for added ends.
        let p = position(&v, "h", Some(&file), &c(5, Some(3), "RIGHT"));
        let code = sha1_hex(b"new.rs");
        assert_eq!(p["line_range"]["start"]["line_code"], format!("{code}_3_3"));
        assert_eq!(p["line_range"]["start"]["type"], "old");
        assert_eq!(p["line_range"]["end"]["line_code"], format!("{code}_5_5"));
        assert_eq!(p["line_range"]["end"]["type"], "new");
    }
}
