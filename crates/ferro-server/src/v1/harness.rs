//! Harness registry, edit dispatch, and snapshot revert (API.md § 10.5).

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use ferro_agent::harness::{self, ActiveEdits, DispatchOutput, HarnessError};

use crate::error::{ApiError, ErrorCode};
use crate::jobs::{Job, JobState};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/harness", get(get_harness).put(put_harness))
        .route("/api/v1/harness/edit", post(edit))
        .route("/api/v1/harness/revert", post(revert))
        .route(
            "/api/v1/harness/threads",
            get(list_threads).post(create_thread),
        )
        .route(
            "/api/v1/harness/threads/{id}",
            get(get_thread).delete(delete_thread),
        )
        .route("/api/v1/harness/threads/{id}/turns", post(add_turn))
}

fn no_git() -> ApiError {
    ApiError::new(ErrorCode::Unsupported, "not a git repository")
}

fn selected_model(s: &AppState) -> (String, String) {
    let eff = s.settings.effective(&s.ws().key);
    let selected = eff
        .get("harness.selected")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model = eff
        .get("harness.model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (selected, model)
}

fn view(s: &AppState) -> serde_json::Value {
    let host = s.harness_host.lock().clone();
    let infos = harness::detect(&host);
    let (selected_raw, model) = selected_model(s);
    let known = infos.iter().any(|h| h.id == selected_raw);
    let selected = if known && !selected_raw.is_empty() {
        Some(selected_raw)
    } else {
        None
    };
    serde_json::json!({
        "selected": selected,
        "pinned": selected.is_some(),
        "model": model,
        "harnesses": infos.iter().map(|h| serde_json::json!({
            "id": h.id,
            "label": h.label,
            "installed": h.installed,
            "models": h.models,
            "defaultModel": h.default_model,
        })).collect::<Vec<_>>(),
    })
}

async fn get_harness(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(view(&s))
}

#[derive(Deserialize)]
struct PutBody {
    id: Option<String>,
    model: Option<String>,
}

async fn put_harness(
    State(s): State<Arc<AppState>>,
    Json(b): Json<PutBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let host = s.harness_host.lock().clone();
    let infos = harness::detect(&host);
    let clearing = b.id.as_deref().unwrap_or("").is_empty();
    let id = b.id.unwrap_or_default();
    if !id.is_empty() && !infos.iter().any(|h| h.id == id) {
        return Err(ApiError::bad_request(format!("unknown harness: {id}")));
    }
    if !id.is_empty() && !infos.iter().any(|h| h.id == id && h.installed) {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            format!("{id} is not installed"),
        ));
    }
    if let Some(model) = b.model.as_deref() {
        if !model.is_empty() && !valid_model(model) {
            return Err(ApiError::bad_request("invalid model"));
        }
    }
    let mut patch = std::collections::BTreeMap::new();
    patch.insert("harness.selected".into(), serde_json::Value::String(id));
    if clearing {
        patch.insert(
            "harness.model".into(),
            serde_json::Value::String(String::new()),
        );
    } else if let Some(model) = b.model {
        patch.insert("harness.model".into(), serde_json::Value::String(model));
    }
    let eff = s
        .settings
        .save(&s.ws().key, "workspace", patch)
        .map_err(ApiError::bad_request)?;
    s.bus.publish(crate::bus::ServerEvent::Settings {
        values: serde_json::json!(eff),
    });
    Ok(Json(view(&s)))
}

#[derive(Deserialize)]
struct EditBody {
    path: String,
    #[serde(rename = "startLine")]
    start_line: u32,
    #[serde(rename = "endLine")]
    end_line: u32,
    instruction: String,
    harness: Option<String>,
    model: Option<String>,
}

fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 128
        && !model.starts_with('-')
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c))
}

async fn edit(
    State(s): State<Arc<AppState>>,
    Json(b): Json<EditBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.path.len() > 512 || b.path.is_empty() {
        return Err(ApiError::bad_request("bad path"));
    }
    if b.start_line == 0 || b.end_line == 0 || b.end_line < b.start_line || b.end_line > 10_000_000
    {
        return Err(ApiError::bad_request("bad line range"));
    }
    let instruction = b.instruction.trim();
    if instruction.is_empty() || instruction.len() > 100_000 {
        return Err(ApiError::bad_request("instruction is empty or too long"));
    }
    let ws = s.ws();
    let root = ws.git.as_ref().ok_or_else(no_git)?.repo.root.clone();
    let rel = ferro_core::paths::git_rel(&root, &b.path, ferro_core::paths::Access::Write)
        .map_err(|_| ApiError::new(ErrorCode::Forbidden, "path escapes root"))?;
    let (selected, stored_model) = selected_model(&s);
    if selected.is_empty() {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "select a harness with PUT /api/v1/harness",
        ));
    }
    if let Some(asked) = b.harness.as_deref() {
        if asked != selected {
            return Err(ApiError::bad_request(
                "harness does not match the opted-in selection",
            ));
        }
    }
    let model = match b.model {
        Some(m) => m,
        None => stored_model,
    };
    if !model.is_empty() && !valid_model(&model) {
        return Err(ApiError::bad_request("invalid model"));
    }
    let host = s.harness_host.lock().clone();
    let prompt = harness::edit_prompt(&rel, b.start_line, b.end_line, instruction);
    let argv = harness::command_argv(
        &selected,
        &prompt,
        Some(model.as_str()).filter(|m| !m.is_empty()),
        &host,
    )
    .map_err(map_harness)?;

    let progress = serde_json::json!({
        "path": rel,
        "startLine": b.start_line,
        "endLine": b.end_line,
    });
    let id = spawn_harness_job(
        &s,
        root,
        argv,
        &host,
        progress,
        Some((rel, b.start_line, b.end_line)),
        None,
    )?;
    Ok(Json(
        serde_json::json!({ "job": { "id": id, "kind": "harness.edit" } }),
    ))
}

/// Called with the finished job (done, failed or cancelled) after its event went out.
type OnSettled = Box<dyn FnOnce(&Job) + Send>;

/// Register and run one `harness.edit` job: snapshot, dispatch, changed paths. `claim` refuses
/// overlapping line-range edits (409); thread turns pass none and serialize per thread instead.
fn spawn_harness_job(
    s: &Arc<AppState>,
    root: std::path::PathBuf,
    argv: Vec<String>,
    host: &harness::HarnessHost,
    progress: serde_json::Value,
    claim: Option<(String, u32, u32)>,
    on_settled: Option<OnSettled>,
) -> Result<String, ApiError> {
    let token = tokio_util::sync::CancellationToken::new();
    let mut job = Job::new("harness.edit");
    job.progress = Some(progress.clone());
    if let Some((rel, start, end)) = &claim {
        if let Err(other) = s.harness_edits.try_claim(&job.id, rel, *start, *end) {
            return Err(ApiError::detail(
                ErrorCode::Conflict,
                format!("overlapping harness edit on {rel}:{start}-{end}"),
                serde_json::json!({
                    "path": rel,
                    "startLine": start,
                    "endLine": end,
                    "jobId": other,
                }),
            ));
        }
    }
    let job = s.jobs.register(job, token.clone());
    s.jobs.update(&job.id, |j| j.state = JobState::Running);
    publish_job(s, &job.id);

    let rel_hold = Release {
        edits: s.harness_edits.clone(),
        id: job.id.clone(),
    };
    let jobs = s.jobs.clone();
    let bus = s.bus.clone();
    let live = job.id.clone();
    let timeout = host.timeout;
    let run = EditRun {
        root,
        argv,
        timeout,
        token: token.clone(),
        extra: host.extra_env.clone(),
        jobs: jobs.clone(),
        id: live.clone(),
        progress,
    };
    tokio::spawn(async move {
        let _rel_hold = rel_hold;
        let joined = tokio::task::spawn_blocking(move || run_edit(run)).await;
        match joined {
            Ok(Ok(outcome)) => settle(&jobs, &live, &token, timeout, outcome),
            Ok(Err(e)) => fail_job(&jobs, &live, &token, e.to_string()),
            Err(_) => fail_job(&jobs, &live, &token, "harness task failed".into()),
        }
        if let Some(j) = jobs.get(&live) {
            if let Some(f) = on_settled {
                f(&j);
            }
            bus.publish(crate::bus::ServerEvent::Job {
                job: serde_json::json!(j),
            });
        }
    });
    Ok(job.id)
}

struct Release {
    edits: ActiveEdits,
    id: String,
}

impl Drop for Release {
    fn drop(&mut self) {
        self.edits.release(&self.id);
    }
}

struct EditOutcome {
    base: String,
    changed: Vec<String>,
    dispatch: DispatchOutput,
}

struct EditRun {
    root: std::path::PathBuf,
    argv: Vec<String>,
    timeout: Duration,
    token: tokio_util::sync::CancellationToken,
    extra: Vec<(String, String)>,
    jobs: crate::jobs::JobManager,
    id: String,
    /// The job's progress payload; the snapshot `base` is added once taken.
    progress: serde_json::Value,
}

fn run_edit(run: EditRun) -> Result<EditOutcome, HarnessError> {
    if run.token.is_cancelled() {
        return Ok(EditOutcome {
            base: String::new(),
            changed: Vec::new(),
            dispatch: cancelled_dispatch(),
        });
    }
    let base = harness::snapshot_tree(&run.root)?;
    let mut progress = run.progress.clone();
    progress["base"] = serde_json::Value::String(base.clone());
    run.jobs.update(&run.id, |j| j.progress = Some(progress));
    if run.token.is_cancelled() {
        return Ok(EditOutcome {
            base,
            changed: Vec::new(),
            dispatch: cancelled_dispatch(),
        });
    }
    let dispatch = harness::dispatch(&run.argv, &run.root, run.timeout, &run.token, &run.extra)?;
    let changed = harness::changed_paths(&run.root, &base)?;
    Ok(EditOutcome {
        base,
        changed,
        dispatch,
    })
}

fn cancelled_dispatch() -> DispatchOutput {
    DispatchOutput {
        exit_code: 137,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        timed_out: false,
        cancelled: true,
        ms: 0,
    }
}

fn result_json(outcome: &EditOutcome) -> serde_json::Value {
    let (stdout_tail, _) = ferro_agent::redact::redact_text(&outcome.dispatch.stdout_tail);
    let (stderr_tail, _) = ferro_agent::redact::redact_text(&outcome.dispatch.stderr_tail);
    serde_json::json!({
        "changed": outcome.changed,
        "base": outcome.base,
        "exitCode": outcome.dispatch.exit_code,
        "stdoutTail": stdout_tail,
        "stderrTail": stderr_tail,
        "stdoutTruncated": outcome.dispatch.stdout_truncated,
        "stderrTruncated": outcome.dispatch.stderr_truncated,
        "timedOut": outcome.dispatch.timed_out,
        "ms": outcome.dispatch.ms,
    })
}

fn settle(
    jobs: &crate::jobs::JobManager,
    id: &str,
    token: &tokio_util::sync::CancellationToken,
    timeout: Duration,
    outcome: EditOutcome,
) {
    let cancelled = token.is_cancelled() || outcome.dispatch.cancelled || outcome.base.is_empty();
    let timed_out = outcome.dispatch.timed_out && !cancelled;
    let result = if outcome.base.is_empty() {
        None
    } else {
        Some(result_json(&outcome))
    };
    jobs.update(id, |j| {
        j.ended_at = Some(crate::jobs::now_iso());
        j.result = result;
        if cancelled {
            j.state = JobState::Cancelled;
        } else if timed_out {
            j.state = JobState::Failed;
            j.error = Some(serde_json::json!({
                "code": "timeout",
                "message": format!("harness timed out after {}s", timeout.as_secs()),
            }));
        } else {
            j.state = JobState::Done;
        }
    });
}

fn fail_job(
    jobs: &crate::jobs::JobManager,
    id: &str,
    token: &tokio_util::sync::CancellationToken,
    message: String,
) {
    let (message, _) = ferro_agent::redact::redact_text(&message);
    jobs.update(id, |j| {
        j.ended_at = Some(crate::jobs::now_iso());
        if token.is_cancelled() {
            j.state = JobState::Cancelled;
        } else {
            j.state = JobState::Failed;
            j.error = Some(serde_json::json!({
                "code": "internal",
                "message": message,
            }));
        }
    });
}

fn publish_job(s: &AppState, id: &str) {
    if let Some(j) = s.jobs.get(id) {
        s.bus.publish(crate::bus::ServerEvent::Job {
            job: serde_json::json!(j),
        });
    }
}

#[derive(Deserialize)]
struct RevertBody {
    #[serde(rename = "jobId")]
    job_id: String,
    paths: Option<Vec<String>>,
    /// A thread turn's job: finished jobs are only kept in memory, the turn is on disk, so a
    /// turn can still be reverted after a restart.
    #[serde(rename = "threadId")]
    thread_id: Option<String>,
    #[serde(rename = "turnId")]
    turn_id: Option<String>,
}

/// (base, changed) of a finished harness edit, from the live job or its persisted thread turn.
fn edit_snapshot(s: &AppState, b: &RevertBody) -> Result<(String, Vec<String>), ApiError> {
    let from_result = |result: Option<&serde_json::Value>, progress: Option<&serde_json::Value>| {
        let base = result
            .and_then(|r| r.get("base"))
            .or_else(|| progress.and_then(|p| p.get("base")))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| ApiError::new(ErrorCode::Conflict, "job has no snapshot"))?;
        let changed = result
            .and_then(|r| r.get("changed"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok((base, changed))
    };
    if let Some(job) = s.jobs.get(&b.job_id) {
        if job.kind != "harness.edit" {
            return Err(ApiError::bad_request("job is not a harness edit"));
        }
        if job.state == JobState::Queued || job.state == JobState::Running {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "harness edit is still running",
            ));
        }
        return from_result(job.result.as_ref(), job.progress.as_ref());
    }
    if let (Some(tid), Some(uid)) = (&b.thread_id, &b.turn_id) {
        let t = load_thread(&threads_dir(s), tid)?;
        let turn = t
            .turns
            .iter()
            .find(|x| &x.id == uid && x.job_id == b.job_id)
            .ok_or_else(|| ApiError::not_found("no such turn"))?;
        if turn.state == "running" {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "harness edit is still running",
            ));
        }
        return from_result(turn.result.as_ref(), None);
    }
    Err(ApiError::not_found(format!("no such job: {}", b.job_id)))
}

async fn revert(
    State(s): State<Arc<AppState>>,
    Json(b): Json<RevertBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (base, changed) = edit_snapshot(&s, &b)?;
    let paths = match b.paths {
        None => changed,
        Some(asked) => {
            if asked.is_empty() || asked.len() > 1000 || asked.iter().any(|p| p.len() > 512) {
                return Err(ApiError::bad_request("paths: 1..1000 required"));
            }
            for p in &asked {
                if !changed.iter().any(|c| c == p) {
                    return Err(ApiError::bad_request(format!(
                        "{p} was not changed by the job"
                    )));
                }
            }
            asked
        }
    };
    let ws = s.ws();
    let root = ws.git.as_ref().ok_or_else(no_git)?.repo.root.clone();
    let reverted = tokio::task::spawn_blocking(move || harness::revert_paths(&root, &base, &paths))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
        .map_err(map_harness)?;
    Ok(Json(serde_json::json!({ "reverted": reverted })))
}

fn map_harness(e: HarnessError) -> ApiError {
    match e {
        HarnessError::NotRepo => no_git(),
        HarnessError::NotInstalled(id) => {
            ApiError::new(ErrorCode::Unsupported, format!("{id} is not installed"))
        }
        HarnessError::Unknown(id) => ApiError::bad_request(format!("unknown harness: {id}")),
        HarnessError::Message(m) => {
            let (m, _) = ferro_agent::redact::redact_text(&m);
            ApiError::new(ErrorCode::Internal, m)
        }
    }
}

// ---------- agent threads (API.md § 10.7) ----------
//
// A thread is a conversation with the opted-in harness, one JSON file per thread under the
// workspace's state dir. Each turn is a `harness.edit` job (snapshot, run, changed paths) whose
// prompt replays the earlier turns, so every harness keeps context, not only ones with a native
// session resume. Turns may carry review comments as context (batch edits from a review).

const MAX_THREADS: usize = 200;
const MAX_TURNS: usize = 100;
const REPLAY_TURNS: usize = 8;
const REPLAY_OUTPUT: usize = 1_500;

#[derive(Serialize, Deserialize, Clone)]
struct Thread {
    id: String,
    title: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    turns: Vec<Turn>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Turn {
    id: String,
    at: String,
    message: String,
    #[serde(default)]
    context: Vec<TurnContext>,
    harness: String,
    #[serde(default)]
    model: String,
    #[serde(rename = "jobId")]
    job_id: String,
    /// running | done | failed | cancelled
    state: String,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Clone)]
struct TurnContext {
    path: String,
    #[serde(rename = "startLine", default, skip_serializing_if = "Option::is_none")]
    start_line: Option<u32>,
    #[serde(rename = "endLine", default, skip_serializing_if = "Option::is_none")]
    end_line: Option<u32>,
    /// A review comment or other note attached to the range (≤ 4 KB).
    #[serde(default)]
    note: String,
}

/// One writer at a time across threads (a turn finishing and a new turn starting race otherwise).
static THREAD_WRITE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn threads_dir(s: &AppState) -> std::path::PathBuf {
    s.dirs.workspace_state_dir(&s.ws().key).join("threads")
}

fn valid_thread_id(id: &str) -> bool {
    id.len() == 28 && id.starts_with("t_") && id[2..].chars().all(|c| c.is_ascii_alphanumeric())
}

fn load_thread(dir: &std::path::Path, id: &str) -> Result<Thread, ApiError> {
    if !valid_thread_id(id) {
        return Err(ApiError::not_found("no such thread"));
    }
    let bytes = std::fs::read(dir.join(format!("{id}.json")))
        .map_err(|_| ApiError::not_found("no such thread"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::new(ErrorCode::Internal, "thread file is damaged"))
}

fn save_thread(dir: &std::path::Path, t: &Thread) -> Result<(), ApiError> {
    std::fs::create_dir_all(dir).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    let bytes = serde_json::to_vec_pretty(t)
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    ferro_core::settings::atomic_write(&dir.join(format!("{}.json", t.id)), &bytes)
        .map_err(|e| ApiError::new(ErrorCode::Internal, e))
}

fn thread_summary(t: &Thread) -> serde_json::Value {
    serde_json::json!({
        "id": t.id,
        "title": t.title,
        "createdAt": t.created_at,
        "updatedAt": t.updated_at,
        "turns": t.turns.len(),
        "lastState": t.turns.last().map(|x| x.state.as_str()),
    })
}

fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Turn prompt: the earlier turns (message, context, outcome, output tail), then this one.
fn thread_prompt(t: &Thread, message: &str, context: &[TurnContext]) -> String {
    let mut p = String::new();
    let earlier = &t.turns[t.turns.len().saturating_sub(REPLAY_TURNS)..];
    if !earlier.is_empty() {
        p.push_str("You are continuing a conversation about this repository. Earlier turns, oldest first:\n");
        for (k, turn) in earlier.iter().enumerate() {
            p.push_str(&format!(
                "\n--- Turn {} ---\nUser: {}\n",
                k + 1,
                turn.message
            ));
            for c in &turn.context {
                p.push_str(&format!("Context: {}\n", context_line(c)));
            }
            match (&turn.result, turn.state.as_str()) {
                (Some(r), state) => {
                    let changed: Vec<&str> = r["changed"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    p.push_str(&format!(
                        "Outcome: {state}, exit {}; changed: {}\n",
                        r["exitCode"],
                        if changed.is_empty() {
                            "nothing".to_string()
                        } else {
                            changed.join(", ")
                        }
                    ));
                    let tail = r["stdoutTail"].as_str().unwrap_or("");
                    let tail = &tail[tail.len().saturating_sub(REPLAY_OUTPUT)..];
                    let tail = tail.trim_start_matches(|c: char| !c.is_ascii()); // stay on a char boundary
                    if !tail.trim().is_empty() {
                        p.push_str(&format!("Your output (tail):\n{tail}\n"));
                    }
                }
                (None, state) => p.push_str(&format!("Outcome: {state}\n")),
            }
        }
        p.push_str("\n--- Now ---\n");
    }
    p.push_str(message);
    if !context.is_empty() {
        p.push_str("\n\nContext:\n");
        for c in context {
            p.push_str(&format!("- {}\n", context_line(c)));
        }
    }
    p
}

fn context_line(c: &TurnContext) -> String {
    let range = match (c.start_line, c.end_line) {
        (Some(a), Some(b)) if a != b => format!(" lines {a}-{b}"),
        (Some(a), _) => format!(" line {a}"),
        _ => String::new(),
    };
    if c.note.is_empty() {
        format!("{}{range}", c.path)
    } else {
        format!("{}{range}: {}", c.path, c.note)
    }
}

async fn list_threads(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let dir = threads_dir(&s);
    let mut out: Vec<Thread> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice::<Thread>(&b).ok())
        .collect();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Json(serde_json::json!({ "threads": out.iter().map(thread_summary).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct CreateThreadBody {
    title: Option<String>,
}

async fn create_thread(
    State(s): State<Arc<AppState>>,
    Json(b): Json<CreateThreadBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let dir = threads_dir(&s);
    let count = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
    if count >= MAX_THREADS {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("at most {MAX_THREADS} threads; delete some first"),
        ));
    }
    let now = crate::jobs::now_iso();
    let title = b
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("New thread");
    let t = Thread {
        id: format!("t_{}", ulid::Ulid::new()),
        title: truncate_chars(title, 120).to_string(),
        created_at: now.clone(),
        updated_at: now,
        turns: Vec::new(),
    };
    let _w = THREAD_WRITE.lock();
    save_thread(&dir, &t)?;
    Ok(Json(serde_json::json!(t)))
}

async fn get_thread(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!(load_thread(&threads_dir(&s), &id)?)))
}

async fn delete_thread(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    let dir = threads_dir(&s);
    let t = load_thread(&dir, &id)?;
    if t.turns.iter().any(|x| {
        x.state == "running"
            && s.jobs
                .get(&x.job_id)
                .is_some_and(|j| j.state == JobState::Running)
    }) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            "a turn is still running",
        ));
    }
    let _w = THREAD_WRITE.lock();
    std::fs::remove_file(dir.join(format!("{id}.json")))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TurnBody {
    message: String,
    #[serde(default)]
    context: Vec<TurnContext>,
}

async fn add_turn(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(b): Json<TurnBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let message = b.message.trim();
    if message.is_empty() || message.len() > 20_000 {
        return Err(ApiError::bad_request("message is empty or too long"));
    }
    if b.context.len() > 50 {
        return Err(ApiError::bad_request("at most 50 context items"));
    }
    let ws = s.ws();
    let root = ws.git.as_ref().ok_or_else(no_git)?.repo.root.clone();
    let mut context = Vec::with_capacity(b.context.len());
    for c in b.context {
        let rel = ferro_core::paths::git_rel(&root, &c.path, ferro_core::paths::Access::Read)
            .map_err(|_| ApiError::new(ErrorCode::Forbidden, "context path escapes root"))?;
        if matches!((c.start_line, c.end_line), (Some(a), Some(z)) if a == 0 || z < a) {
            return Err(ApiError::bad_request("bad context line range"));
        }
        context.push(TurnContext {
            path: rel,
            start_line: c.start_line,
            end_line: c.end_line,
            note: truncate_chars(c.note.trim(), 4_000).to_string(),
        });
    }
    let (selected, model) = selected_model(&s);
    if selected.is_empty() {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "select a harness with PUT /api/v1/harness",
        ));
    }
    let dir = threads_dir(&s);
    let _w = THREAD_WRITE.lock();
    let mut t = load_thread(&dir, &id)?;
    if t.turns.len() >= MAX_TURNS {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("a thread holds at most {MAX_TURNS} turns; start a new one"),
        ));
    }
    if let Some(prev) = t.turns.iter().find(|x| x.state == "running") {
        if s.jobs
            .get(&prev.job_id)
            .is_some_and(|j| matches!(j.state, JobState::Running | JobState::Queued))
        {
            return Err(ApiError::detail(
                ErrorCode::Conflict,
                "the previous turn is still running",
                serde_json::json!({ "jobId": prev.job_id }),
            ));
        }
    }
    let host = s.harness_host.lock().clone();
    let prompt = thread_prompt(&t, message, &context);
    let argv = harness::command_argv(
        &selected,
        &prompt,
        Some(model.as_str()).filter(|m| !m.is_empty()),
        &host,
    )
    .map_err(map_harness)?;
    let turn_id = format!("u_{}", ulid::Ulid::new());
    let settled_dir = dir.clone();
    let (tid, uid) = (t.id.clone(), turn_id.clone());
    let on_settled: OnSettled = Box::new(move |job: &Job| {
        let _w = THREAD_WRITE.lock();
        if let Ok(mut t) = load_thread(&settled_dir, &tid) {
            if let Some(turn) = t.turns.iter_mut().find(|x| x.id == uid) {
                turn.state = match job.state {
                    JobState::Done => "done",
                    JobState::Cancelled => "cancelled",
                    _ => "failed",
                }
                .into();
                turn.result = job.result.clone();
                turn.error = job.error.clone();
            }
            t.updated_at = crate::jobs::now_iso();
            if let Err(e) = save_thread(&settled_dir, &t) {
                tracing::warn!("thread {tid}: could not save the finished turn: {e:?}");
            }
        }
    });
    let progress = serde_json::json!({ "threadId": t.id, "turnId": turn_id });
    let job_id = spawn_harness_job(&s, root, argv, &host, progress, None, Some(on_settled))?;
    let turn = Turn {
        id: turn_id,
        at: crate::jobs::now_iso(),
        message: message.to_string(),
        context,
        harness: selected,
        model,
        job_id: job_id.clone(),
        state: "running".into(),
        result: None,
        error: None,
    };
    if t.turns.is_empty() && t.title == "New thread" {
        t.title = truncate_chars(message.lines().next().unwrap_or(message), 80).to_string();
    }
    t.turns.push(turn.clone());
    t.updated_at = crate::jobs::now_iso();
    save_thread(&dir, &t)?;
    Ok(Json(
        serde_json::json!({ "job": { "id": job_id, "kind": "harness.edit" }, "turn": turn }),
    ))
}
