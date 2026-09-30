//! Team review memory (API.md § 17): rules from what reviewers accept and dismiss, applied to
//! AI reviews and security checks. Team rules are `.ferro-rules.json` in the repository (read
//! from the PR's base, never its head, in PR mode: a PR must not be able to hide its own
//! findings); personal rules, signals and hit counts live in ferro's state directory.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
    Json, Router,
};
use ferro_core::memory::{self, Personal, Rule, Signal};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/memory", get(list))
        .route("/api/v1/memory/rules", post(create))
        .route("/api/v1/memory/rules/{id}", patch(update).delete(remove))
        .route("/api/v1/memory/signals", post(signal))
        .route(
            "/api/v1/memory/suggestions/dismiss",
            post(dismiss_suggestion),
        )
}

/// One writer at a time for the personal file and the team file.
static WRITE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn state_dir(s: &AppState) -> std::path::PathBuf {
    s.dirs.workspace_state_dir(&s.ws().key)
}

/// Team rules and where they came from: the worktree, or the PR's merge base in PR mode.
pub(crate) fn team_rules(s: &AppState) -> (Vec<Rule>, &'static str) {
    let ws = s.ws();
    if let Some(pr) = ws.pr.as_ref() {
        let base = pr.worktree.read().merge_base.clone();
        let rules = ws
            .git
            .as_ref()
            .and_then(|g| {
                g.repo
                    .blob_bytes_max(&base, memory::TEAM_FILE, 1 << 20)
                    .ok()
            })
            .map(|b| memory::parse_team(&b))
            .unwrap_or_default();
        return (rules, "base");
    }
    (memory::read_team(&ws.root), "worktree")
}

/// Every rule in force: the team's, then this user's.
pub(crate) fn active_rules(s: &AppState) -> Vec<Rule> {
    let (mut rules, _) = team_rules(s);
    rules.extend(memory::read_personal(&state_dir(s)).rules);
    rules
}

/// Remember an accept or dismiss (suggestions come from these).
pub(crate) fn record_signal(s: &AppState, sig: Signal) {
    let dir = state_dir(s);
    let _g = WRITE.lock();
    let mut p = memory::read_personal(&dir);
    p.record(sig);
    let _ = memory::write_personal(&dir, &p);
}

/// Count rule hits (shown next to each rule).
pub(crate) fn note_hits(s: &AppState, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let dir = state_dir(s);
    let _g = WRITE.lock();
    let mut p = memory::read_personal(&dir);
    for id in ids {
        p.hit(id);
    }
    let _ = memory::write_personal(&dir, &p);
}

/// The first ignore rule covering a finding, as `{ id, reason, scope }`.
pub(crate) fn suppressor(
    rules: &[Rule],
    team_ids: &std::collections::HashSet<String>,
    subject: &memory::Subject,
) -> Option<serde_json::Value> {
    rules.iter().find(|r| memory::matches(r, subject)).map(|r| {
        serde_json::json!({
            "id": r.id, "reason": r.reason,
            "scope": if team_ids.contains(&r.id) { "team" } else { "personal" },
        })
    })
}

fn author(s: &AppState) -> String {
    let ws = s.ws();
    let Some(g) = ws.git.as_ref() else {
        return String::new();
    };
    let get = |k: &str| {
        g.repo
            .run(&["config", "--get", k])
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    match (get("user.name"), get("user.email")) {
        (n, e) if !n.is_empty() && !e.is_empty() => format!("{n} <{e}>"),
        (n, _) => n,
    }
}

fn view(s: &AppState) -> serde_json::Value {
    let (team, source) = team_rules(s);
    let p = memory::read_personal(&state_dir(s));
    let row = |r: &Rule, scope: &str| {
        let mut v = serde_json::to_value(r).unwrap_or_default();
        v["scope"] = serde_json::json!(scope);
        if let Some((n, at)) = p.hits.get(&r.id) {
            v["hits"] = serde_json::json!(n);
            v["lastHit"] = serde_json::json!(at);
        } else {
            v["hits"] = serde_json::json!(0);
        }
        v
    };
    let mut rules: Vec<serde_json::Value> = team.iter().map(|r| row(r, "team")).collect();
    rules.extend(p.rules.iter().map(|r| row(r, "personal")));
    let ws = s.ws();
    serde_json::json!({
        "rules": rules,
        "suggestions": memory::suggestions(&p, &team),
        "team": {
            "path": memory::TEAM_FILE,
            "exists": ws.root.join(memory::TEAM_FILE).is_file(),
            "source": source,
            // A .gitignore that matches the file would keep the team from ever getting it.
            "gitIgnored": ws.git.as_ref().is_some_and(|g| g.repo.run(&["check-ignore", "-q", memory::TEAM_FILE]).is_ok()),
        },
        "signals": p.signals.len(),
    })
}

async fn list(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(
        tokio::task::spawn_blocking(move || view(&s))
            .await
            .unwrap_or_default(),
    )
}

#[derive(Deserialize)]
struct NewRule {
    kind: String,
    #[serde(rename = "appliesTo")]
    applies_to: String,
    rule: Option<String>,
    category: Option<String>,
    title: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    text: Option<String>,
    #[serde(default)]
    reason: String,
    /// `team` (the repository file) or `personal` (default).
    scope: Option<String>,
}

fn clean(o: Option<String>) -> Option<String> {
    o.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn save(s: &AppState, rule: Rule, team: bool) -> Result<(), ApiError> {
    let ws = s.ws();
    if team {
        let mut rules = memory::read_team(&ws.root);
        rules.retain(|r| r.id != rule.id);
        rules.push(rule);
        memory::write_team(&ws.root, &rules).map_err(|e| ApiError::new(ErrorCode::Internal, e))
    } else {
        let dir = state_dir(s);
        let mut p = memory::read_personal(&dir);
        p.rules.retain(|r| r.id != rule.id);
        p.rules.push(rule);
        memory::write_personal(&dir, &p).map_err(|e| ApiError::new(ErrorCode::Internal, e))
    }
}

async fn create(
    State(s): State<Arc<AppState>>,
    Json(b): Json<NewRule>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let rule = Rule {
        id: memory::new_id(),
        kind: b.kind,
        applies_to: b.applies_to,
        rule: clean(b.rule),
        category: clean(b.category),
        title: clean(b.title),
        paths: b
            .paths
            .into_iter()
            .map(|p| p.trim().trim_start_matches("./").to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        text: clean(b.text),
        reason: b.reason.trim().to_string(),
        author: author(&s),
        created_at: memory::now_iso(),
    };
    if let Some(why) = memory::validate(&rule) {
        return Err(ApiError::bad_request(why));
    }
    let team = b.scope.as_deref() == Some("team");
    let s2 = s.clone();
    let r2 = rule.clone();
    tokio::task::spawn_blocking(move || {
        let _g = WRITE.lock();
        save(&s2, r2, team)
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "memory task failed"))??;
    let mut v = serde_json::to_value(&rule).unwrap_or_default();
    v["scope"] = serde_json::json!(if team { "team" } else { "personal" });
    Ok((StatusCode::CREATED, Json(v)))
}

#[derive(Deserialize)]
struct RulePatch {
    scope: Option<String>,
    reason: Option<String>,
    paths: Option<Vec<String>>,
    text: Option<String>,
}

/// Edit a rule, or move it between personal and team ("share with team").
async fn update(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(b): Json<RulePatch>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let s2 = s.clone();
    tokio::task::spawn_blocking(move || {
        let _g = WRITE.lock();
        let ws = s2.ws();
        let dir = state_dir(&s2);
        let mut p = memory::read_personal(&dir);
        let mut team = memory::read_team(&ws.root);
        let (mut rule, was_team) = if let Some(i) = team.iter().position(|r| r.id == id) {
            (team.remove(i), true)
        } else if let Some(i) = p.rules.iter().position(|r| r.id == id) {
            (p.rules.remove(i), false)
        } else {
            return Err(ApiError::not_found(format!("no such rule: {id}")));
        };
        if let Some(r) = b.reason {
            rule.reason = r.trim().to_string();
        }
        if let Some(paths) = b.paths {
            rule.paths = paths;
        }
        if let Some(t) = b.text {
            rule.text = clean(Some(t));
        }
        if let Some(why) = memory::validate(&rule) {
            return Err(ApiError::bad_request(why));
        }
        let to_team = match b.scope.as_deref() {
            Some("team") => true,
            Some("personal") => false,
            _ => was_team,
        };
        if to_team {
            team.push(rule.clone());
        } else {
            p.rules.push(rule.clone());
        }
        memory::write_team(&ws.root, &team).map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
        memory::write_personal(&dir, &p).map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
        let mut v = serde_json::to_value(&rule).unwrap_or_default();
        v["scope"] = serde_json::json!(if to_team { "team" } else { "personal" });
        Ok(Json(v))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "memory task failed"))?
}

async fn remove(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    tokio::task::spawn_blocking(move || {
        let _g = WRITE.lock();
        let ws = s.ws();
        let dir = state_dir(&s);
        let mut p = memory::read_personal(&dir);
        let mut team = memory::read_team(&ws.root);
        let (nt, np) = (team.len(), p.rules.len());
        team.retain(|r| r.id != id);
        p.rules.retain(|r| r.id != id);
        if team.len() == nt && p.rules.len() == np {
            return Err(ApiError::not_found(format!("no such rule: {id}")));
        }
        if team.len() != nt {
            memory::write_team(&ws.root, &team)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
        }
        p.hits.remove(&id);
        memory::write_personal(&dir, &p).map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "memory task failed"))?
}

#[derive(Deserialize)]
struct SignalBody {
    action: String,
    source: String,
    rule: Option<String>,
    category: Option<String>,
    #[serde(default)]
    title: String,
    path: String,
}

async fn signal(
    State(s): State<Arc<AppState>>,
    Json(b): Json<SignalBody>,
) -> Result<StatusCode, ApiError> {
    if !["accept", "dismiss"].contains(&b.action.as_str())
        || !["ai", "security"].contains(&b.source.as_str())
    {
        return Err(ApiError::bad_request(
            "action must be accept or dismiss; source ai or security",
        ));
    }
    if b.path.len() > 512 || b.title.len() > 400 {
        return Err(ApiError::bad_request("path or title too long"));
    }
    let sig = Signal {
        action: b.action,
        source: b.source,
        rule: clean(b.rule),
        category: clean(b.category),
        title: b.title,
        path: b.path,
        at: memory::now_iso(),
    };
    tokio::task::spawn_blocking(move || record_signal(&s, sig))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "memory task failed"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
}

/// "Not now" on a suggestion: it does not come back.
async fn dismiss_suggestion(
    State(s): State<Arc<AppState>>,
    Json(b): Json<KeyBody>,
) -> Result<StatusCode, ApiError> {
    if b.key.len() > 1000 {
        return Err(ApiError::bad_request("key too long"));
    }
    tokio::task::spawn_blocking(move || {
        let dir = state_dir(&s);
        let _g = WRITE.lock();
        let mut p: Personal = memory::read_personal(&dir);
        if !p.dismissed_suggestions.contains(&b.key) {
            p.dismissed_suggestions.push(b.key);
        }
        let _ = memory::write_personal(&dir, &p);
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "memory task failed"))?;
    Ok(StatusCode::NO_CONTENT)
}
