//! Review threads, drafts, submit, viewed, rounds (API.md §§ 8.3, 9).
//! Bodies render through ferro's own markdown pipeline. Drafts live in the
//! per-PR store (disk, restart-proof); submit pins `commit_id` to the
//! current head SHA and posts reply drafts afterwards.

use axum::{
    body::Bytes,
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::{AppState, PrSession};
use crate::v1::pr::forge_err;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/pr/threads", get(threads))
        .route("/api/v1/pr/threads/{id}/reply", post(reply))
        .route("/api/v1/pr/conversation", post(conversation))
        .route("/api/v1/review/drafts", get(drafts_list).post(drafts_add))
        .route(
            "/api/v1/review/drafts/{id}",
            axum::routing::patch(drafts_patch).delete(drafts_delete),
        )
        .route("/api/v1/review/submit", post(submit))
        .route("/api/v1/review/viewed", get(viewed_get).put(viewed_put))
        .route("/api/v1/review/rounds", get(rounds))
}

fn session(s: &Arc<AppState>) -> Result<(Arc<crate::state::Workspace>, Arc<PrSession>), ApiError> {
    let ws = s.ws();
    let pr = ws
        .pr
        .as_ref()
        .ok_or_else(|| ApiError::new(ErrorCode::Unsupported, "not in PR mode"))?
        .clone();
    Ok((ws, pr))
}

fn render_comment(c: &ferro_forge::github::ForgeComment) -> serde_json::Value {
    let html = crate::v1::markdown::render_v2(&c.body, "", "", None).html;
    serde_json::json!({
        "id": c.id,
        "author": {"login": c.author_login, "avatarUrl": c.author_avatar},
        "body": c.body,
        "bodyHtml": html,
        "createdAt": c.created_at,
        "url": c.url,
    })
}

async fn threads(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let (_, pr) = session(&s)?;
    let (threads, conversation) = tokio::join!(
        pr.github.threads(&pr.pr_ref),
        pr.github.conversation(&pr.pr_ref)
    );
    let threads = threads.map_err(forge_err)?;
    let conversation = conversation.map_err(forge_err)?;
    let out: Vec<serde_json::Value> = threads
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "path": t.path,
                "line": t.line,
                "startLine": t.start_line,
                "side": t.side,
                "originalLine": t.original_line,
                "commitSha": t.commit_sha,
                "outdated": t.outdated,
                "resolved": t.resolved,
                "comments": t.comments.iter().map(render_comment).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "threads": out,
        "conversation": conversation.iter().map(render_comment).collect::<Vec<_>>(),
    })))
}

async fn reply(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (_, pr) = session(&s)?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let text = v.get("body").and_then(|b| b.as_str()).unwrap_or("");
    if text.trim().is_empty() {
        return Err(ApiError::bad_request("body required"));
    }
    // Reply targets take the thread's head comment id; drafts store the
    // GraphQL thread id, so resolve it through a fresh thread list.
    let comment_id: u64 = if let Ok(n) = id.parse::<u64>() {
        n
    } else {
        let threads = pr.github.threads(&pr.pr_ref).await.map_err(forge_err)?;
        threads
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.comments.first())
            .and_then(|c| c.id.parse::<u64>().ok())
            .ok_or_else(|| ApiError::not_found("no such thread".to_string()))?
    };
    let c = pr
        .github
        .reply(&pr.pr_ref, comment_id, text)
        .await
        .map_err(forge_err)?;
    Ok(Json(render_comment(&c)))
}

async fn conversation(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (_, pr) = session(&s)?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let text = v.get("body").and_then(|b| b.as_str()).unwrap_or("");
    if text.trim().is_empty() {
        return Err(ApiError::bad_request("body required"));
    }
    let c = pr
        .github
        .post_comment(&pr.pr_ref, text)
        .await
        .map_err(forge_err)?;
    Ok(Json(render_comment(&c)))
}

// -- drafts --------------------------------------------------------------------

/// `drafts` event: the whole list (API.md § 12 keeps every tab in sync from
/// it), after any change — add, edit, delete, remap, submit.
pub(crate) fn publish_drafts(s: &Arc<AppState>, pr: &PrSession) {
    let all: Vec<serde_json::Value> = pr.store.drafts().iter().map(draft_json).collect();
    s.bus.publish(crate::bus::ServerEvent::Drafts {
        drafts: serde_json::Value::Array(all),
    });
}

fn draft_json(d: &ferro_forge::store::Draft) -> serde_json::Value {
    serde_json::json!({
        "id": d.id, "path": d.path, "line": d.line, "startLine": d.start_line,
        "side": d.side, "body": d.body, "threadId": d.thread_id,
        "source": match d.source { ferro_forge::store::DraftSource::Human => "human", ferro_forge::store::DraftSource::Ai => "ai" },
        "findingId": d.finding_id, "createdAt": d.created_at, "updatedAt": d.updated_at,
        "stale": d.stale,
    })
}

async fn drafts_list(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let (_, pr) = session(&s)?;
    let all = tokio::task::spawn_blocking(move || pr.store.drafts())
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "store task failed"))?;
    Ok(Json(
        serde_json::json!({ "drafts": all.iter().map(draft_json).collect::<Vec<_>>() }),
    ))
}

async fn drafts_add(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > 1024 * 1024 {
        return Err(ApiError::new(ErrorCode::TooLarge, "body over 1 MiB"));
    }
    let (_, pr) = session(&s)?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let nd: ferro_forge::store::NewDraft = serde_json::from_value(v)
        .map_err(|e| ApiError::bad_request(format!("invalid draft: {e}")))?;
    let pr2 = pr.clone();
    let d = tokio::task::spawn_blocking(move || pr2.store.add(nd))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "store task failed"))?
        .map_err(ApiError::bad_request)?;
    publish_drafts(&s, &pr);
    Ok(Json(draft_json(&d)))
}

async fn drafts_patch(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (_, pr) = session(&s)?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let p: ferro_forge::store::DraftPatch = serde_json::from_value(v)
        .map_err(|e| ApiError::bad_request(format!("invalid patch: {e}")))?;
    let pr2 = pr.clone();
    let d = tokio::task::spawn_blocking(move || pr2.store.patch(&id, &p))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "store task failed"))?
        .map_err(|e| {
            if e == ferro_forge::store::NO_DRAFT {
                ApiError::not_found(e)
            } else {
                ApiError::bad_request(e)
            }
        })?;
    publish_drafts(&s, &pr);
    Ok(Json(draft_json(&d)))
}

async fn drafts_delete(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    let (_, pr) = session(&s)?;
    let pr2 = pr.clone();
    let gone = tokio::task::spawn_blocking(move || pr2.store.remove(&id))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "store task failed"))?;
    if gone {
        publish_drafts(&s, &pr);
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("no such draft".to_string()))
    }
}

// -- submit ----------------------------------------------------------------------

async fn submit(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (ws, pr) = session(&s)?;
    if !pr.github.has_token() {
        return Err(crate::v1::pr::no_token());
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let event = match v.get("event").and_then(|e| e.as_str()).unwrap_or("COMMENT") {
        "APPROVE" => ferro_forge::github::ReviewEvent::Approve,
        "REQUEST_CHANGES" => ferro_forge::github::ReviewEvent::RequestChanges,
        _ => ferro_forge::github::ReviewEvent::Comment,
    };
    let review_body = v
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    // The head the drafts were written against (the checked-out one), not
    // session.meta: the poll moves meta to a newer push, and pinning that
    // would place every old-head line number on different code.
    let head_sha = pr.worktree.read().head_sha.clone();
    let drafts = pr.store.drafts();
    // Stale drafts are skipped and reported; reply drafts defer to replies.
    let mut failed = Vec::new();
    let mut comments = Vec::new();
    let mut replies: Vec<(String, String, String)> = Vec::new(); // (draft_id, thread_id, body)
    for d in &drafts {
        if d.stale {
            failed.push(serde_json::json!({ "draftId": d.id, "error": "stale at current head" }));
            continue;
        }
        if let Some(tid) = d.thread_id.as_deref() {
            replies.push((d.id.clone(), tid.to_string(), d.body.clone()));
            continue;
        }
        comments.push(ferro_forge::github::ReviewComment {
            path: d.path.clone(),
            body: d.body.clone(),
            line: Some(d.line as u64),
            side: Some(d.side.clone()),
            start_line: d.start_line.map(|l| l as u64),
            start_side: d.start_line.map(|_| d.side.clone()),
        });
    }
    let resp = pr
        .github
        .submit_review(&pr.pr_ref, &head_sha, event, &review_body, &comments)
        .await
        .map_err(forge_err)?;
    let mut posted = comments.len();
    // Reply drafts post after the review; failures stay as drafts.
    let live_threads = pr.github.threads(&pr.pr_ref).await.map_err(forge_err)?;
    for (draft_id, thread_id, text) in replies {
        let cid = live_threads
            .iter()
            .find(|t| t.id == thread_id)
            .and_then(|t| t.comments.first())
            .and_then(|c| c.id.parse::<u64>().ok());
        match cid {
            Some(cid) => match pr.github.reply(&pr.pr_ref, cid, &text).await {
                Ok(_) => {
                    posted += 1;
                    pr.store.remove(&draft_id);
                }
                Err(e) => {
                    failed.push(serde_json::json!({ "draftId": draft_id, "error": e.to_string() }))
                }
            },
            None => failed.push(serde_json::json!({ "draftId": draft_id, "error": "thread gone" })),
        }
    }
    // Posted drafts leave the store; failed ones stay.
    for d in &drafts {
        if !d.stale && d.thread_id.is_none() {
            pr.store.remove(&d.id);
        }
    }
    pr.store.record_round(&head_sha, "submitted");
    publish_drafts(&s, &pr);
    let _ = ws;
    Ok(Json(serde_json::json!({
        "url": resp.html_url,
        "submittedAt": crate::jobs::now_iso(),
        "posted": posted,
        "failed": failed,
    })))
}

// -- viewed + rounds ---------------------------------------------------------------

/// Worktree blob ids for `paths` (what `git hash-object` computes), in one
/// git call. Paths are validated workspace-relative paths; a path with no
/// regular file (deleted) has no blob.
fn worktree_blobs(
    repo: &ferro_core::git::GitRepo,
    paths: &[String],
) -> std::collections::HashMap<String, Option<String>> {
    let is_id =
        |h: &str| (h.len() == 40 || h.len() == 64) && h.bytes().all(|b| b.is_ascii_hexdigit());
    let mut out = std::collections::HashMap::new();
    let mut present: Vec<(String, String)> = Vec::new();
    for p in paths {
        let rel = ferro_core::paths::git_rel(&repo.root, p, ferro_core::paths::Access::Read);
        match rel {
            // `--stdin-paths` is line-based: a name with a newline has no
            // stable blob id here and simply counts as changed.
            Ok(rel)
                if !rel.contains('\n')
                    && std::fs::symlink_metadata(repo.root.join(&rel))
                        .is_ok_and(|m| m.is_file()) =>
            {
                present.push((p.clone(), rel))
            }
            _ => {
                out.insert(p.clone(), None);
            }
        }
    }
    if !present.is_empty() {
        let input: String = present.iter().map(|(_, rel)| format!("{rel}\n")).collect();
        let hashes = repo
            .run_stdin(&["hash-object", "--stdin-paths"], input.as_bytes())
            .unwrap_or_default();
        let mut lines = hashes.lines();
        for (p, _) in present {
            let h = lines
                .next()
                .map(|s| s.trim().to_string())
                .filter(|h| is_id(h));
            out.insert(p, h);
        }
    }
    out
}

async fn viewed_get(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let (ws, pr) = session(&s)?;
    let head = pr.meta.read().head_sha.clone();
    let repo = ws.git.as_ref().map(|g| g.repo.clone());
    let out = tokio::task::spawn_blocking(move || {
        let v: serde_json::Value = pr.store.viewed(&head);
        let mut files = serde_json::Map::new();
        if let (Some(repo), Some(map)) = (repo, v.get("files").and_then(|m| m.as_object())) {
            let paths: Vec<String> = map.keys().cloned().collect();
            let blobs = worktree_blobs(&repo, &paths);
            for path in paths {
                let blob = blobs.get(&path).cloned().flatten();
                files.insert(
                    path.clone(),
                    serde_json::to_value(pr.store.viewed_state(&path, blob.as_deref())).unwrap(),
                );
            }
        }
        serde_json::json!({ "headSha": head, "files": files })
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "store task failed"))?;
    Ok(Json(out))
}

async fn viewed_put(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (ws, pr) = session(&s)?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("");
    let viewed = v.get("viewed").and_then(|b| b.as_bool()).unwrap_or(false);
    if path.is_empty() || path.len() > 512 {
        return Err(ApiError::bad_request("path required"));
    }
    // Workspace-relative, like every other path (§ 5.1): the stored key is
    // later hashed, so nothing outside the checkout or option-shaped passes.
    let path_s = ferro_core::paths::git_rel(&ws.root, path, ferro_core::paths::Access::Read)
        .map_err(|_| ApiError::new(ErrorCode::Forbidden, "path outside workspace"))?;
    let repo = ws.git.as_ref().map(|g| g.repo.clone());
    let out = tokio::task::spawn_blocking(move || {
        let blob = repo
            .as_ref()
            .and_then(|r| worktree_blobs(r, std::slice::from_ref(&path_s)).remove(&path_s))
            .flatten();
        pr.store.set_viewed(&path_s, viewed, blob)
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "store task failed"))?;
    Ok(Json(serde_json::to_value(out).unwrap()))
}

async fn rounds(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let (_, pr) = session(&s)?;
    let all = tokio::task::spawn_blocking(move || pr.store.rounds())
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "store task failed"))?;
    Ok(Json(serde_json::json!({ "rounds": all })))
}
