//! GitHub REST v3 + GraphQL v4 client (B4, Appendix C).
//! Recorded-fixture tests spin a local mock server — no network in CI.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

use super::error::ForgeError;
use super::parse::ForgeRef;

const PROVIDER: &str = "github";

/// Never constructed with a token in `Debug`: headers carry it per request.
pub struct GitHub {
    http: reqwest::Client,
    api_base: String,
    graphql_url: String,
    token: Option<String>,
    etags: Mutex<HashMap<String, (String, Vec<u8>)>>,
}

impl std::fmt::Debug for GitHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHub")
            .field("api_base", &self.api_base)
            .field("has_token", &self.token.is_some())
            .finish()
    }
}

impl GitHub {
    pub fn new(api_base: String, graphql_url: String, token: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("client");
        Self {
            http,
            api_base,
            graphql_url,
            token,
            etags: Mutex::new(HashMap::new()),
        }
    }

    pub fn for_ref(r: &ForgeRef, token: Option<String>) -> Self {
        Self::new(r.api_base(), r.graphql_url(), token)
    }

    pub fn has_token(&self) -> bool {
        self.token.as_ref().is_some_and(|t| !t.is_empty())
    }

    fn req(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut b = self
            .http
            .request(method, url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "ferro");
        if let Some(t) = self.token.as_deref() {
            b = b.bearer_auth(t);
        }
        b
    }

    /// GET with ETag caching: 304 reuses the stored body.
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
            return Err(ForgeError::NotFound("github resource".into()));
        }
        if status == 429
            || (status == 403
                && resp
                    .headers()
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v == "0"))
        {
            return Err(ForgeError::RateLimited {
                retry_after_ms: retry_after(resp),
            });
        }
        Err(ForgeError::Upstream {
            provider: PROVIDER,
            status,
        })
    }

    // -- Appendix C calls ---------------------------------------------------

    pub async fn pull(&self, r: &ForgeRef) -> Result<PullMeta, ForgeError> {
        let v = self
            .get(&format!(
                "{}/repos/{}/{}/pulls/{}",
                self.api_base, r.owner, r.repo, r.number
            ))
            .await?;
        PullMeta::parse(&v)
    }

    pub async fn can_push(&self, r: &ForgeRef) -> Result<bool, ForgeError> {
        let v = self
            .get(&format!("{}/repos/{}/{}", self.api_base, r.owner, r.repo))
            .await?;
        Ok(v.get("permissions")
            .and_then(|p| p.get("push"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false))
    }

    pub async fn checks(&self, r: &ForgeRef, sha: &str) -> Result<Checks, ForgeError> {
        let runs = self
            .get(&format!(
                "{}/repos/{}/{}/commits/{sha}/check-runs",
                self.api_base, r.owner, r.repo
            ))
            .await?;
        let statuses = self
            .get(&format!(
                "{}/repos/{}/{}/commits/{sha}/status",
                self.api_base, r.owner, r.repo
            ))
            .await?;
        Ok(Checks::summarize(&runs, &statuses))
    }

    pub async fn threads(&self, r: &ForgeRef) -> Result<Vec<ReviewThread>, ForgeError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let (nodes, next) = self.threads_page(r, cursor.as_deref()).await?;
            out.extend(nodes);
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    async fn threads_page(
        &self,
        r: &ForgeRef,
        cursor: Option<&str>,
    ) -> Result<(Vec<ReviewThread>, Option<String>), ForgeError> {
        let q = serde_json::json!({
            "query": THREADS_QUERY,
            "variables": { "owner": r.owner, "name": r.repo, "number": r.number, "cursor": cursor },
        });
        let v = self.post(&self.graphql_url.clone(), q).await?;
        if let Some(errs) = v.get("errors") {
            return Err(ForgeError::Schema(errs.to_string()));
        }
        let pr = &v["data"]["repository"]["pullRequest"];
        if pr.is_null() {
            return Err(ForgeError::NotFound("pull request".into()));
        }
        let mut nodes = Vec::new();
        for n in pr["reviewThreads"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            nodes.push(ReviewThread::parse(&n)?);
        }
        let page = &pr["reviewThreads"]["pageInfo"];
        let next = if page
            .get("hasNextPage")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            page.get("endCursor")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };
        Ok((nodes, next))
    }

    pub async fn conversation(&self, r: &ForgeRef) -> Result<Vec<ForgeComment>, ForgeError> {
        // Issue comments double as the PR conversation.
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let v = self
                .get(&format!(
                    "{}/repos/{}/{}/issues/{}/comments?per_page=100&page={page}",
                    self.api_base, r.owner, r.repo, r.number
                ))
                .await?;
            let arr = v.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                break;
            }
            let full = arr.len() == 100;
            for c in &arr {
                out.push(ForgeComment::parse(c)?);
            }
            if !full {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    pub async fn submit_review(
        &self,
        r: &ForgeRef,
        head_sha: &str,
        event: ReviewEvent,
        body: &str,
        comments: &[ReviewComment],
    ) -> Result<SubmitResponse, ForgeError> {
        let v = self
            .post(
                &format!(
                    "{}/repos/{}/{}/pulls/{}/reviews",
                    self.api_base, r.owner, r.repo, r.number
                ),
                serde_json::json!({
                    "commit_id": head_sha,
                    "body": body,
                    "event": event.as_str(),
                    "comments": comments.iter().map(|c| c.json()).collect::<Vec<_>>(),
                }),
            )
            .await?;
        Ok(SubmitResponse {
            id: v.get("id").and_then(|i| i.as_u64()).unwrap_or(0),
            html_url: v
                .get("html_url")
                .and_then(|u| u.as_str())
                .unwrap_or("")
                .into(),
            state: v.get("state").and_then(|s| s.as_str()).unwrap_or("").into(),
        })
    }

    pub async fn reply(
        &self,
        r: &ForgeRef,
        comment_id: u64,
        body: &str,
    ) -> Result<ForgeComment, ForgeError> {
        let v = self
            .post(
                &format!(
                    "{}/repos/{}/{}/pulls/{}/comments/{comment_id}/replies",
                    self.api_base, r.owner, r.repo, r.number
                ),
                serde_json::json!({ "body": body }),
            )
            .await?;
        ForgeComment::parse(&v)
    }

    pub async fn post_comment(&self, r: &ForgeRef, body: &str) -> Result<ForgeComment, ForgeError> {
        let v = self
            .post(
                &format!(
                    "{}/repos/{}/{}/issues/{}/comments",
                    self.api_base, r.owner, r.repo, r.number
                ),
                serde_json::json!({ "body": body }),
            )
            .await?;
        ForgeComment::parse(&v)
    }
}

fn retry_after(resp: &reqwest::Response) -> u64 {
    if let Some(v) = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    {
        return v * 1000;
    }
    if let Some(reset) = resp
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return reset.saturating_sub(now).saturating_add(1) * 1000;
    }
    60_000
}

// -- shapes -------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PullMeta {
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub merged: bool,
    pub draft: bool,
    pub base_ref: String,
    pub head_ref: String,
    pub base_sha: String,
    pub head_sha: String,
    pub head_clone_url: Option<String>,
    pub is_fork: bool,
    pub created_at: String,
    pub updated_at: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub commits: u64,
    pub html_url: String,
    pub author_login: String,
    pub author_avatar: Option<String>,
}

impl PullMeta {
    fn parse(v: &serde_json::Value) -> Result<Self, ForgeError> {
        let req = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let head = &v["head"];
        let base = &v["base"];
        Ok(Self {
            title: req("title"),
            body: v
                .get("body")
                .and_then(|b| b.as_str())
                .map(|s| s.to_string()),
            state: req("state"),
            merged: v.get("merged").and_then(|b| b.as_bool()).unwrap_or(false),
            draft: v.get("draft").and_then(|b| b.as_bool()).unwrap_or(false),
            base_ref: base
                .get("ref")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .into(),
            head_ref: head
                .get("ref")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .into(),
            base_sha: base
                .get("sha")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .into(),
            head_sha: head
                .get("sha")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .into(),
            head_clone_url: head
                .get("repo")
                .and_then(|r| r.get("clone_url"))
                .and_then(|u| u.as_str())
                .map(|s| s.into()),
            is_fork: head
                .get("repo")
                .and_then(|r| r.get("fork"))
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            created_at: req("created_at"),
            updated_at: req("updated_at"),
            additions: v.get("additions").and_then(|n| n.as_u64()).unwrap_or(0),
            deletions: v.get("deletions").and_then(|n| n.as_u64()).unwrap_or(0),
            changed_files: v.get("changed_files").and_then(|n| n.as_u64()).unwrap_or(0),
            commits: v.get("commits").and_then(|n| n.as_u64()).unwrap_or(0),
            html_url: req("html_url"),
            author_login: v
                .get("user")
                .and_then(|u| u.get("login"))
                .and_then(|l| l.as_str())
                .unwrap_or("")
                .into(),
            author_avatar: v
                .get("user")
                .and_then(|u| u.get("avatar_url"))
                .and_then(|l| l.as_str())
                .map(|s| s.into()),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Checks {
    pub state: String,
    pub url: Option<String>,
}

impl Checks {
    fn summarize(runs: &serde_json::Value, statuses: &serde_json::Value) -> Self {
        let mut url = None;
        let mut pending = false;
        // check-runs first (richer), then legacy statuses.
        for r in runs
            .get("check_runs")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let status = r.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let conclusion = r.get("conclusion").and_then(|s| s.as_str()).unwrap_or("");
            if url.is_none() {
                url = r
                    .get("details_url")
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string());
            }
            if matches!(
                conclusion,
                "failure" | "timed_out" | "action_required" | "cancelled"
            ) {
                return Self {
                    state: "failure".into(),
                    url,
                };
            }
            if conclusion.is_empty()
                || matches!(
                    status,
                    "queued" | "in_progress" | "requested" | "waiting" | "pending"
                )
            {
                pending = true;
            }
        }
        for s in statuses
            .get("statuses")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let state = s.get("state").and_then(|x| x.as_str()).unwrap_or("");
            if url.is_none() {
                url = s
                    .get("target_url")
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string());
            }
            if matches!(state, "failure" | "error") {
                return Self {
                    state: "failure".into(),
                    url,
                };
            }
            if state == "pending" {
                pending = true;
            }
        }
        let any = runs
            .get("total_count")
            .and_then(|n| n.as_u64())
            .unwrap_or(0)
            > 0
            || statuses
                .get("total_count")
                .and_then(|n| n.as_u64())
                .unwrap_or(0)
                > 0;
        Self {
            state: if pending {
                "pending"
            } else if any {
                "success"
            } else {
                "none"
            }
            .into(),
            url,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ForgeComment {
    pub id: String,
    pub author_login: String,
    pub author_avatar: Option<String>,
    pub body: String,
    pub created_at: String,
    pub url: String,
}

impl ForgeComment {
    fn parse(v: &serde_json::Value) -> Result<Self, ForgeError> {
        Ok(Self {
            id: v
                .get("databaseId")
                .and_then(|i| i.as_u64())
                .map(|i| i.to_string())
                .or_else(|| v.get("id").and_then(|i| i.as_u64()).map(|i| i.to_string()))
                .or_else(|| v.get("id").and_then(|i| i.as_str()).map(|s| s.into()))
                .ok_or_else(|| ForgeError::Schema("comment without id".into()))?,
            author_login: v
                .get("author")
                .and_then(|a| a.get("login"))
                .and_then(|l| l.as_str())
                .or_else(|| {
                    v.get("user")
                        .and_then(|u| u.get("login"))
                        .and_then(|l| l.as_str())
                })
                .unwrap_or("")
                .into(),
            author_avatar: v
                .get("author")
                .and_then(|a| a.get("avatarUrl"))
                .and_then(|l| l.as_str())
                .or_else(|| {
                    v.get("user")
                        .and_then(|u| u.get("avatar_url"))
                        .and_then(|l| l.as_str())
                })
                .map(|s| s.into()),
            body: v.get("body").and_then(|b| b.as_str()).unwrap_or("").into(),
            created_at: v
                .get("createdAt")
                .and_then(|t| t.as_str())
                .or_else(|| v.get("created_at").and_then(|t| t.as_str()))
                .unwrap_or("")
                .into(),
            url: v
                .get("url")
                .and_then(|u| u.as_str())
                .or_else(|| v.get("html_url").and_then(|u| u.as_str()))
                .unwrap_or("")
                .into(),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    pub line: Option<u64>,
    pub start_line: Option<u64>,
    pub side: String,
    pub original_line: Option<u64>,
    pub outdated: bool,
    pub resolved: bool,
    pub comments: Vec<ForgeComment>,
}

impl ReviewThread {
    fn parse(v: &serde_json::Value) -> Result<Self, ForgeError> {
        let side = v
            .get("diffSide")
            .and_then(|s| s.as_str())
            .unwrap_or("RIGHT");
        Ok(Self {
            id: v.get("id").and_then(|i| i.as_str()).unwrap_or("").into(),
            path: v.get("path").and_then(|p| p.as_str()).unwrap_or("").into(),
            line: v.get("line").and_then(|n| n.as_u64()),
            start_line: v.get("startLine").and_then(|n| n.as_u64()),
            side: side.into(),
            original_line: v.get("originalLine").and_then(|n| n.as_u64()),
            outdated: v
                .get("isOutdated")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            resolved: v
                .get("isResolved")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            comments: v
                .get("comments")
                .and_then(|c| c.get("nodes"))
                .and_then(|n| n.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(ForgeComment::parse)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewEvent::Comment => "COMMENT",
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReviewComment {
    pub path: String,
    pub body: String,
    pub line: Option<u64>,
    pub side: Option<String>,
    pub start_line: Option<u64>,
    pub start_side: Option<String>,
}

impl ReviewComment {
    fn json(&self) -> serde_json::Value {
        let mut o = serde_json::json!({ "path": self.path, "body": self.body });
        if let Some(l) = self.line {
            o["line"] = l.into();
        }
        if let Some(s) = self.side.as_deref() {
            o["side"] = s.into();
        }
        if let Some(l) = self.start_line {
            o["start_line"] = l.into();
        }
        if let Some(s) = self.start_side.as_deref() {
            o["start_side"] = s.into();
        }
        o
    }
}

#[derive(Debug, Clone)]
pub struct SubmitResponse {
    pub id: u64,
    pub html_url: String,
    pub state: String,
}

const THREADS_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$cursor:String){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100,after:$cursor){nodes{id isResolved isOutdated path line startLine diffSide originalLine comments(first:100){nodes{id databaseId author{login avatarUrl}body createdAt url}}}pageInfo{hasNextPage endCursor}}}}}";
