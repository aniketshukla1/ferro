//! Pull requests (API.md § 8): session open/refresh over jobs.
//! Metadata comes from ferro-forge (recorded-fixture tested); bodies render
//! through ferro's own markdown pipeline, never GitHub HTML.

use axum::{
    body::Bytes,
    extract::State,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::{AppState, PrSession, Workspace};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/pr", get(get_pr))
        .route("/api/v1/pr/open", post(open))
        .route("/api/v1/pr/refresh", post(refresh))
}

fn forge_err(e: ferro_forge::ForgeError) -> ApiError {
    let code = match e.code() {
        "bad_request" => ErrorCode::BadRequest,
        "unauthorized" => ErrorCode::Unauthorized,
        "not_found" => ErrorCode::NotFound,
        "rate_limited" => ErrorCode::RateLimited,
        _ => ErrorCode::Upstream,
    };
    ApiError::detail(code, e.to_string(), e.detail())
}

/// Assemble API.md § 8.1 from the cached session state.
pub(crate) fn pr_meta_json(ws: &Arc<Workspace>, session: &PrSession) -> serde_json::Value {
    let meta = session.meta.read();
    let state = if meta.merged {
        "merged"
    } else {
        meta.state.as_str()
    };
    let body_html =
        crate::v1::markdown::render_v2(meta.body.as_deref().unwrap_or(""), "", "", None).html;
    let head_moved = ferro_core::git::GitRepo::new(ws.root.clone())
        .head_sha()
        .map(|h| h != meta.head_sha)
        .unwrap_or(false);
    let can_push_head = if session
        .can_push_known
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        session
            .can_push
            .read()
            .map(|b| serde_json::json!(b))
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    serde_json::json!({
        "provider": "github",
        "host": session.pr_ref.host,
        "owner": session.pr_ref.owner,
        "repo": session.pr_ref.repo,
        "number": session.pr_ref.number,
        "url": session.pr_ref.html_url(),
        "title": meta.title,
        "bodyHtml": body_html,
        "author": {"login": meta.author_login, "avatarUrl": meta.author_avatar},
        "state": state,
        "draft": meta.draft,
        "baseRef": meta.base_ref,
        "headRef": meta.head_ref,
        "baseSha": meta.base_sha,
        "headSha": meta.head_sha,
        "mergeBaseSha": session.worktree.read().merge_base,
        "isFork": meta.is_fork,
        "createdAt": meta.created_at,
        "updatedAt": meta.updated_at,
        "stats": {"files": meta.changed_files, "additions": meta.additions, "deletions": meta.deletions, "commits": meta.commits},
        "checks": session.checks.read().clone().map(|c| serde_json::json!({"state": c.state, "url": c.url})).unwrap_or(serde_json::Value::Null),
        "auth": {
            "hasToken": session.github.has_token(),
            "source": session.token_source.map(|s| s.as_str()),
            "canReview": session.github.has_token(),
            "canPushHead": can_push_head,
        },
        "lastReviewedSha": session.store.last_reviewed_sha(),
        "headMoved": head_moved,
    })
}

async fn get_pr(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    match ws.pr.as_ref() {
        Some(session) => Ok(Json(
            serde_json::json!({ "pr": pr_meta_json(&ws, session) }),
        )),
        None => Ok(Json(serde_json::json!({ "pr": null }))),
    }
}

async fn open(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > 1024 * 1024 {
        return Err(ApiError::new(ErrorCode::TooLarge, "body over 1 MiB"));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let url = v.get("url").and_then(|u| u.as_str()).unwrap_or("");
    start_open_job(&s, url).await
}

pub(crate) async fn start_open_job(
    s: &Arc<AppState>,
    url: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pr_ref = ferro_forge::parse_pr_url(url)
        .ok_or_else(|| ApiError::bad_request("not a GitHub PR url"))?;
    if !pr_ref.host.eq_ignore_ascii_case("github.com") && pr_ref.host.is_empty() {
        return Err(ApiError::bad_request("not a GitHub PR url"));
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    let job = s
        .jobs
        .register(crate::jobs::Job::new("pr.open"), cancel.clone());
    let job_id = job.id.clone();
    let s2 = s.clone();
    let pr_ref2 = pr_ref.clone();
    let job_id_resp = job_id.clone();
    tokio::spawn(async move {
        run_open_job(&s2, &job_id, &pr_ref2, cancel).await;
    });
    Ok(Json(
        serde_json::json!({ "job": { "id": job_id_resp, "kind": "pr.open" } }),
    ))
}

fn job_progress(s: &Arc<AppState>, id: &str, message: &str) {
    if let Some(j) = s.jobs.update(id, |j| {
        j.state = crate::jobs::JobState::Running;
        j.progress = Some(serde_json::json!({ "message": message }));
    }) {
        s.bus.publish(crate::bus::ServerEvent::Job {
            job: serde_json::to_value(&j).unwrap(),
        });
    }
}

fn job_done(s: &Arc<AppState>, id: &str, result: serde_json::Value) {
    if let Some(j) = s.jobs.update(id, |j| {
        j.state = crate::jobs::JobState::Done;
        j.ended_at = Some(crate::jobs::now_iso());
        j.result = Some(result);
    }) {
        s.bus.publish(crate::bus::ServerEvent::Job {
            job: serde_json::to_value(&j).unwrap(),
        });
    }
}

fn job_failed(
    s: &Arc<AppState>,
    id: &str,
    code: ErrorCode,
    message: String,
    detail: serde_json::Value,
) {
    if let Some(j) = s.jobs.update(id, |j| {
        j.state = crate::jobs::JobState::Failed;
        j.ended_at = Some(crate::jobs::now_iso());
        j.error = Some(
            serde_json::json!({ "code": code.as_str(), "message": message, "detail": detail }),
        );
    }) {
        s.bus.publish(crate::bus::ServerEvent::Job {
            job: serde_json::to_value(&j).unwrap(),
        });
    }
}

fn fail_forge(s: &Arc<AppState>, id: &str, e: ferro_forge::ForgeError) {
    let (code, detail) = match e.code() {
        "bad_request" => (ErrorCode::BadRequest, serde_json::Value::Null),
        "unauthorized" => (ErrorCode::Unauthorized, e.detail()),
        "not_found" => (ErrorCode::NotFound, serde_json::Value::Null),
        "rate_limited" => (ErrorCode::RateLimited, e.detail()),
        _ => (ErrorCode::Upstream, e.detail()),
    };
    job_failed(s, id, code, e.to_string(), detail);
}

async fn run_open_job(
    s: &Arc<AppState>,
    job_id: &str,
    r: &ferro_forge::ForgeRef,
    cancel: tokio_util::sync::CancellationToken,
) {
    if cancel.is_cancelled() {
        let _ = s.jobs.cancel(job_id);
        return;
    }
    // metadata (async HTTP).
    job_progress(s, job_id, "metadata");
    let (token, source) = ferro_forge::resolve_token(&r.host)
        .map(|(t, x)| (Some(t), Some(x)))
        .unwrap_or((None, None));
    let github = Arc::new(ferro_forge::GitHub::for_ref(r, token.clone()));
    let meta = match github.pull(r).await {
        Ok(m) => m,
        Err(e) => {
            fail_forge(s, job_id, e);
            return;
        }
    };
    let (checks, can_push) =
        match tokio::join!(github.checks(r, &meta.head_sha), github.can_push(r)) {
            (Ok(c), Ok(p)) => (Some(c), Some(p)),
            (Ok(c), Err(_)) => (Some(c), None),
            _ => (None, None),
        };
    if cancel.is_cancelled() {
        let _ = s.jobs.cancel(job_id);
        return;
    }
    // fetch + worktree (blocking git) with progress forwarding.
    let (prog_tx, mut prog_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let s_prog = s.clone();
    let job_id_prog = job_id.to_string();
    let fwd = tokio::spawn(async move {
        while let Some(m) = prog_rx.recv().await {
            job_progress(&s_prog, &job_id_prog, &m);
        }
    });
    let r2 = r.clone();
    let meta2 = meta.clone();
    let token2 = token.clone();
    let dirs = s.dirs.clone();
    let url = r.clone_url();
    let local = s.ws().root.clone();
    let opened = tokio::task::spawn_blocking(move || {
        ferro_forge::open_pr(
            &r2,
            &meta2,
            &url,
            Some(&local),
            &dirs,
            token2.as_deref(),
            &|m| {
                let _ = prog_tx.send(m.to_string());
            },
            &ferro_forge::CheckoutOpts::default(),
        )
    })
    .await;
    drop(fwd);
    let opened = match opened {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            fail_forge(s, job_id, e);
            return;
        }
        Err(_) => {
            job_failed(
                s,
                job_id,
                ErrorCode::Internal,
                "checkout task failed".into(),
                serde_json::Value::Null,
            );
            return;
        }
    };
    if cancel.is_cancelled() {
        let _ = s.jobs.cancel(job_id);
        return;
    }
    // Swap workspace.
    let store_dir = s
        .dirs
        .state_dir
        .join("reviews")
        .join(&r.host)
        .join(&r.owner)
        .join(&r.repo)
        .join(r.number.to_string());
    let session = Arc::new(PrSession {
        pr_ref: r.clone(),
        meta: parking_lot::RwLock::new(meta),
        github: github.clone(),
        token: token.clone(),
        token_source: source,
        worktree: parking_lot::RwLock::new(opened.clone()),
        store: ferro_forge::store::ReviewStore::new(store_dir),
        checks: parking_lot::RwLock::new(checks),
        can_push: parking_lot::RwLock::new(can_push),
        can_push_known: std::sync::atomic::AtomicBool::new(false),
    });
    let ws = Workspace::pr(opened.dir.clone(), session.clone(), &s.dirs);
    let key = ws.key.clone();
    // Legacy routes read the core pr context: point the base at the fetched
    // ref so merge-base diffs resolve inside the worktree.
    ws.index.set_pr(ferro_core::pr::PrCtx {
        owner: r.owner.clone(),
        repo: r.repo.clone(),
        number: r.number,
        base_ref: "refs/ferro/base".into(),
        base_sha: opened.base_sha.clone(),
        head_sha: opened.head_sha.clone(),
    });
    s.ws.store(ws.clone());
    // Index the worktree in the background (workspace.open pattern).
    let ws2 = ws.clone();
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
    crate::watch::start(s);
    s.bus.publish(crate::bus::ServerEvent::Workspace {
        key,
        root: opened.dir.to_string_lossy().to_string(),
        mode: "pr".into(),
    });
    job_done(s, job_id, pr_meta_json(&ws, &session));
}

async fn refresh(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let session = ws
        .pr
        .as_ref()
        .ok_or_else(|| ApiError::new(ErrorCode::Unsupported, "not in PR mode"))?
        .clone();
    let old_head = session.meta.read().head_sha.clone();
    let meta = session
        .github
        .pull(&session.pr_ref)
        .await
        .map_err(forge_err)?;
    let head_moved = meta.head_sha != old_head;
    *session.meta.write() = meta.clone();
    // Checks + permissions refresh (best effort).
    if let Ok(c) = session.github.checks(&session.pr_ref, &meta.head_sha).await {
        *session.checks.write() = Some(c);
    }
    if let Ok(p) = session.github.can_push(&session.pr_ref).await {
        *session.can_push.write() = Some(p);
    }
    if head_moved {
        // Move the worktree to the new head (refuses when dirty), then
        // remap drafts so survivors follow their lines.
        let r = session.pr_ref.clone();
        let dirs = s.dirs.clone();
        let token = session.token.clone();
        let url = r.clone_url();
        let local = ws.root.clone();
        let meta2 = meta.clone();
        let opened = tokio::task::spawn_blocking(move || {
            ferro_forge::open_pr(
                &r,
                &meta2,
                &url,
                Some(&local),
                &dirs,
                token.as_deref(),
                &|_| {},
                &ferro_forge::CheckoutOpts::default(),
            )
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "refresh task failed"))?
        .map_err(forge_err)?;
        *session.worktree.write() = opened;
        let repo = ferro_core::git::GitRepo::new(ws.root.clone());
        let _ = session.store.remap(&repo, &old_head, &meta.head_sha);
    }
    let v = pr_meta_json(&ws, &session);
    s.bus.publish(crate::bus::ServerEvent::Pr { pr: v.clone() });
    Ok(Json(v))
}
