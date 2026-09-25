//! Git surface (API.md § 6): status, changes, log, mutations.
//! Every call runs in `spawn_blocking` with the hardened runner; paths are
//! validated by § 5.1 inside the core mutations.

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::{ApiError, ErrorCode};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/git/status", get(status))
        .route("/api/v1/git/changes", get(changes))
        .route("/api/v1/git/diff", get(diff))
        .route("/api/v1/git/log", get(log))
        .route("/api/v1/git/stage", post(stage))
        .route("/api/v1/git/unstage", post(unstage))
        .route("/api/v1/git/discard", post(discard))
        .route("/api/v1/git/commit", post(commit))
        .route("/api/v1/git/push", post(push))
        .route("/api/v1/git/pull", post(pull))
}

fn no_git() -> ApiError {
    ApiError::new(ErrorCode::Unsupported, "not a git repository")
}

fn map_err(e: ferro_core::git::GitError) -> ApiError {
    use ferro_core::git::GitError as G;
    match e {
        G::NotRepo => no_git(),
        G::Forbidden(what) => ApiError::new(ErrorCode::Forbidden, what),
        G::Cancelled => ApiError::new(ErrorCode::Cancelled, "cancelled"),
        G::Timeout { args, secs } => ApiError::detail(
            ErrorCode::GitFailed,
            format!("git {args} timed out after {secs}s"),
            serde_json::json!({ "stderr": "" }),
        ),
        G::Failed { args, stderr } => {
            // Nothing staged and diverged pulls are client states, not crashes.
            if stderr.contains("nothing staged") || stderr.contains("empty message") {
                return ApiError::new(ErrorCode::Conflict, stderr);
            }
            if args.starts_with("pull") && stderr.contains("Not possible to fast-forward") {
                return ApiError::new(ErrorCode::Conflict, stderr);
            }
            ApiError::detail(
                ErrorCode::GitFailed,
                format!("git {args} failed"),
                serde_json::json!({ "stderr": stderr }),
            )
        }
        G::Io(e) => ApiError::new(ErrorCode::Internal, e),
    }
}

async fn status(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    let st = tokio::task::spawn_blocking(move || g.status_v2())
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
        .map_err(map_err)?;
    Ok(Json(serde_json::to_value(&st).unwrap()))
}

#[derive(Deserialize)]
struct ChangesQ {
    base: Option<String>,
    target: Option<String>,
}

async fn changes(
    State(s): State<Arc<AppState>>,
    Query(q): Query<ChangesQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    let base = q.base.unwrap_or_else(|| "HEAD".into());
    let target = q.target.unwrap_or_else(|| "worktree".into());
    if base.len() > 256 || target.len() > 256 {
        return Err(ApiError::bad_request("base/target too long"));
    }
    let cs = tokio::task::spawn_blocking(move || g.changes(&base, &target))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
        .map_err(map_err)?;
    Ok(Json(serde_json::to_value(&cs).unwrap()))
}

#[derive(Deserialize)]
struct LogQ {
    limit: Option<usize>,
    path: Option<String>,
}

async fn log(
    State(s): State<Arc<AppState>>,
    Query(q): Query<LogQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    if let Some(p) = &q.path {
        if p.len() > 512 {
            return Err(ApiError::bad_request("path too long"));
        }
    }
    let out = tokio::task::spawn_blocking(move || g.log(limit, q.path.as_deref()))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
        .map_err(map_err)?;
    Ok(Json(serde_json::json!({ "commits": out })))
}

fn fresh_status(g: &ferro_core::git::GitRepo) -> Result<serde_json::Value, ApiError> {
    let st = g.status_v2().map_err(map_err)?;
    Ok(serde_json::to_value(&st).unwrap())
}

#[derive(Deserialize)]
struct PathsBody {
    paths: Vec<String>,
}

fn check_paths(paths: &[String]) -> Result<(), ApiError> {
    if paths.is_empty() || paths.len() > 1000 {
        return Err(ApiError::bad_request("paths: 1..1000 required"));
    }
    if paths.iter().any(|p| p.len() > 512) {
        return Err(ApiError::bad_request("path too long"));
    }
    Ok(())
}

async fn stage(
    State(s): State<Arc<AppState>>,
    Json(b): Json<PathsBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_paths(&b.paths)?;
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.stage_paths(&b.paths)
            .map_err(map_err)
            .and_then(|_| fresh_status(&g))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

async fn unstage(
    State(s): State<Arc<AppState>>,
    Json(b): Json<PathsBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_paths(&b.paths)?;
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.unstage_paths(&b.paths)
            .map_err(map_err)
            .and_then(|_| fresh_status(&g))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

#[derive(Deserialize)]
struct DiscardBody {
    paths: Vec<String>,
    confirm: Option<bool>,
}

async fn discard(
    State(s): State<Arc<AppState>>,
    Json(b): Json<DiscardBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !b.confirm.unwrap_or(false) {
        return Err(ApiError::bad_request("discard requires confirm: true"));
    }
    check_paths(&b.paths)?;
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.discard_paths(&b.paths)
            .map_err(map_err)
            .and_then(|_| fresh_status(&g))
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

#[derive(Deserialize)]
struct CommitBody {
    message: String,
    amend: Option<bool>,
}

async fn commit(
    State(s): State<Arc<AppState>>,
    Json(b): Json<CommitBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.message.len() > 100_000 {
        return Err(ApiError::bad_request("message too long"));
    }
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.commit_msg(&b.message, b.amend.unwrap_or(false))
            .map_err(map_err)
            .and_then(|c| {
                fresh_status(&g).map(
                    |st| serde_json::json!({ "sha": c.sha, "summary": c.subject, "status": st }),
                )
            })
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

async fn push(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.push().map_err(map_err).and_then(|output| {
            fresh_status(&g).map(|st| serde_json::json!({ "output": output, "status": st }))
        })
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

async fn pull(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    tokio::task::spawn_blocking(move || {
        g.pull_ff().map_err(map_err).and_then(|(output, updated)| {
            fresh_status(&g)
                .map(|st| serde_json::json!({ "output": output, "updated": updated, "status": st }))
        })
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))?
    .map(Json)
}

// -- file diff ---------------------------------------------------------------

/// Sides over 2 MiB are served plain (spec).
const DIFF_HL_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Deserialize)]
struct DiffQ {
    path: String,
    base: Option<String>,
    target: Option<String>,
    context: Option<usize>,
    #[serde(default, deserialize_with = "super::de_flag")]
    hl: Option<bool>,
    #[serde(default, deserialize_with = "super::de_flag")]
    intraline: Option<bool>,
    #[serde(rename = "ignoreWs", default, deserialize_with = "super::de_flag")]
    ignore_ws: Option<bool>,
    #[serde(default, deserialize_with = "super::de_flag")]
    force: Option<bool>,
}

async fn diff(
    State(s): State<Arc<AppState>>,
    Query(q): Query<DiffQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if q.path.len() > 512 {
        return Err(ApiError::bad_request("path too long"));
    }
    let ws = s.ws();
    let g = ws.git.as_ref().ok_or_else(no_git)?.repo.clone();
    let base = q.base.unwrap_or_else(|| "HEAD".into());
    let target = q.target.unwrap_or_else(|| "worktree".into());
    if base.len() > 256 || target.len() > 256 {
        return Err(ApiError::bad_request("base/target too long"));
    }
    let ctx = q.context.unwrap_or(3).min(50);
    let want_hl = q.hl.unwrap_or(true);
    let want_ch = q.intraline.unwrap_or(true);
    let ignore_ws = q.ignore_ws.unwrap_or(false);
    let force = q.force.unwrap_or(false);
    let max_rows = s.limits.max_diff_rows;
    let out = tokio::task::spawn_blocking(move || {
        render_diff(
            &g, &q.path, &base, &target, ctx, want_hl, want_ch, ignore_ws, force, max_rows,
        )
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal, "git task failed"))??;
    Ok(Json(out))
}

#[allow(clippy::too_many_arguments)]
fn render_diff(
    g: &ferro_core::git::GitRepo,
    path: &str,
    base: &str,
    target: &str,
    ctx: usize,
    want_hl: bool,
    want_ch: bool,
    ignore_ws: bool,
    force: bool,
    max_rows: usize,
) -> Result<serde_json::Value, ApiError> {
    use ferro_core::diff::{DiffSide, RowKind};
    let raw = g
        .diff_raw(path, base, target, ctx, ignore_ws)
        .map_err(map_err)?;
    let display = if raw.new_path.is_empty() {
        path.to_string()
    } else {
        raw.new_path.clone()
    };
    let language = super::files::language_of(&display).map(|x| x.to_string());
    let mut v = serde_json::json!({
        "path": display,
        "status": raw.status.as_str(),
        "binary": raw.binary,
        "tooLarge": false,
        "language": language,
        "hunks": [],
    });
    if let Some(o) = raw.old_path.as_deref() {
        if o != display {
            v["oldPath"] = serde_json::json!(o);
        }
    }
    if let Some(b) = raw.old_blob.as_deref() {
        v["oldBlob"] = serde_json::json!(b);
    }
    if let Some(b) = raw.new_blob.as_deref() {
        v["newBlob"] = serde_json::json!(b);
    }
    if raw.binary {
        return Ok(v);
    }
    let rows: usize = raw.hunks.iter().map(|h| h.rows.len()).sum();
    if rows > max_rows && !force {
        v["tooLarge"] = serde_json::json!(true);
        return Ok(v);
    }
    // Full-file highlighting of each side (exact: parsed from line 1).
    let base_sha = g.resolve_base(base).map_err(map_err)?;
    let old_path = raw.old_path.as_deref().unwrap_or(&display);
    let old_bytes = match raw.status {
        ferro_core::git::ChangeStatus::Added => Vec::new(),
        _ => g.side_bytes(old_path, &DiffSide::Rev(base_sha)),
    };
    let new_bytes = match target {
        "worktree" => g.side_bytes(&display, &DiffSide::Worktree),
        "index" => g.side_bytes(&display, &DiffSide::Index),
        rev => match g.resolve_base(rev) {
            Ok(sha) => g.side_bytes(&display, &DiffSide::Rev(sha)),
            Err(_) => Vec::new(),
        },
    };
    let old_html = highlight_side(&old_bytes, old_path, want_hl);
    let new_html = highlight_side(&new_bytes, &display, want_hl);
    let mut hunks = Vec::with_capacity(raw.hunks.len());
    for h in &raw.hunks {
        // Pair k-th del with k-th add inside each change block.
        let mut ch: std::collections::HashMap<(usize, usize), Vec<(usize, usize)>> =
            std::collections::HashMap::new();
        if want_ch {
            pair_change_blocks(h, &mut ch);
        }
        let mut rows = Vec::with_capacity(h.rows.len());
        for (ri, r) in h.rows.iter().enumerate() {
            let (side_html, ln) = match r.t {
                RowKind::Del => (&old_html, r.o),
                _ => (&new_html, r.n),
            };
            let body = ln
                .and_then(|n| side_html.as_ref().and_then(|v| v.get(n.saturating_sub(1))))
                .cloned()
                .unwrap_or_else(|| escape_plain(&r.text));
            let mut obj = serde_json::json!({
                "t": match r.t { RowKind::Ctx => "ctx", RowKind::Add => "add", RowKind::Del => "del" },
                "o": r.o,
                "n": r.n,
            });
            if want_hl {
                obj["html"] = serde_json::json!(body);
            } else {
                obj["text"] = serde_json::json!(r.text);
            }
            if r.no_eol {
                obj["noEol"] = serde_json::json!(true);
            }
            let ch_key = match r.t {
                ferro_core::diff::RowKind::Del => (ri, 0),
                _ => (ri, 1),
            };
            if let Some(ranges) = ch.get(&ch_key) {
                let arr: Vec<serde_json::Value> = ranges
                    .iter()
                    .map(|(a, b)| serde_json::json!([a, b]))
                    .collect();
                if !arr.is_empty() {
                    obj["ch"] = serde_json::Value::Array(arr);
                }
            }
            rows.push(obj);
        }
        let mut hunk = serde_json::json!({
            "id": h.id(&display),
            "header": h.header,
            "oldStart": h.old_start,
            "oldLines": h.old_lines,
            "newStart": h.new_start,
            "newLines": h.new_lines,
            "rows": rows,
        });
        if let Some(sec) = h.section.as_deref() {
            hunk["section"] = serde_json::json!(sec);
        }
        hunks.push(hunk);
    }
    v["hunks"] = serde_json::Value::Array(hunks);
    Ok(v)
}

/// Highlight a whole side file; `None` = serve plain (too big or hl off).
fn highlight_side(bytes: &[u8], path: &str, want_hl: bool) -> Option<Vec<String>> {
    if !want_hl || bytes.len() as u64 > DIFF_HL_BYTES {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let ss = crate::hl::syntax_set();
    let syntax = crate::hl::find_syntax(ss, None, Some(std::path::Path::new(path)));
    let mut parse = syntect::parsing::ParseState::new(syntax);
    let mut stack = syntect::parsing::ScopeStack::new();
    let mut out = Vec::new();
    // `split('\n')` keeps \r at line ends, matching diff row text.
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    for line in lines {
        if line.encode_utf16().count() > 20_000 {
            out.push(escape_plain(line));
        } else {
            out.push(crate::hl::highlight_line(line, ss, &mut parse, &mut stack));
        }
    }
    Some(out)
}

fn escape_plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Word tokens as byte ranges (char-boundary aligned): word runs vs
/// non-word runs.
fn word_tokens(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut word: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let w = c.is_alphanumeric() || c == '_';
        match word {
            None => {
                start = i;
                word = Some(w);
            }
            Some(cur) if cur == w => {}
            Some(_) => {
                out.push((start, i));
                start = i;
                word = Some(w);
            }
        }
    }
    if word.is_some() {
        out.push((start, s.len()));
    }
    out
}

/// Pair k-th del with k-th add inside each maximal change block; fill `ch`
/// with UTF-16 ranges keyed (row_idx, side) where side 0 = del, 1 = add.
/// Pairs under ~40 % token similarity are skipped.
fn pair_change_blocks(
    h: &ferro_core::diff::RawHunk,
    ch: &mut std::collections::HashMap<(usize, usize), Vec<(usize, usize)>>,
) {
    use ferro_core::diff::RowKind;
    let mut i = 0;
    while i < h.rows.len() {
        if h.rows[i].t == RowKind::Ctx {
            i += 1;
            continue;
        }
        let mut dels = Vec::new();
        let mut adds = Vec::new();
        while i < h.rows.len() && h.rows[i].t != RowKind::Ctx {
            if h.rows[i].t == RowKind::Del {
                dels.push(i);
            } else {
                adds.push(i);
            }
            i += 1;
        }
        for (k, (&di, &ai)) in dels.iter().zip(adds.iter()).enumerate() {
            let _ = k;
            if let Some((ra, rb)) = intraline_pair(&h.rows[di].text, &h.rows[ai].text) {
                ch.insert((di, 0), ra);
                ch.insert((ai, 1), rb);
            }
        }
    }
}

/// UTF-16 ranges inside a row's text.
type Ranges16 = Vec<(usize, usize)>;

fn intraline_pair(a: &str, b: &str) -> Option<(Ranges16, Ranges16)> {
    let ta = word_tokens(a);
    let tb = word_tokens(b);
    if ta.is_empty() && tb.is_empty() {
        return None;
    }
    let va: Vec<&str> = ta.iter().map(|(s, e)| &a[*s..*e]).collect();
    let vb: Vec<&str> = tb.iter().map(|(s, e)| &b[*s..*e]).collect();
    let diff = similar::TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .diff_slices(&va, &vb);
    let mut eq = 0usize;
    for op in diff.ops() {
        let (tag, o_range, _) = op.as_tag_tuple();
        if matches!(tag, similar::DiffTag::Equal) {
            eq += o_range.len();
        }
    }
    let total = va.len() + vb.len();
    // Skip pairs under ~40% token similarity: eq*2/total < 0.4.
    if total == 0 || eq * 5 < total * 2 {
        return None;
    }
    let mut ra: Vec<(usize, usize)> = Vec::new();
    let mut rb: Vec<(usize, usize)> = Vec::new();
    for op in diff.ops() {
        let (tag, o_range, n_range) = op.as_tag_tuple();
        if matches!(tag, similar::DiffTag::Equal) {
            continue;
        }
        if !o_range.is_empty() {
            let s = ta[o_range.start].0;
            let e = ta[o_range.end - 1].1;
            ra.push(ferro_core::text::utf8_range_to_utf16(a, s, e));
        }
        if !n_range.is_empty() {
            let s = tb[n_range.start].0;
            let e = tb[n_range.end - 1].1;
            rb.push(ferro_core::text::utf8_range_to_utf16(b, s, e));
        }
    }
    if ra.is_empty() && rb.is_empty() {
        return None;
    }
    Some((merge_ranges(ra), merge_ranges(rb)))
}

/// Merge overlapping/adjacent ranges (defensive; token spans are disjoint).
fn merge_ranges(mut v: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    v.sort();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        if let Some(last) = out.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        out.push((s, e));
    }
    out
}
