//! Search surface (API.md §§ 5.1–5.3, 5.5–5.6): fuzzy, search, stream, find, resolve.

use axum::{
    body::Bytes,
    extract::{Query, State},
    response::sse::{Event, Sse},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;
use tokio_stream::StreamExt as _;

use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/fuzzy", get(fuzzy))
        .route("/api/v1/search", get(search))
        .route("/api/v1/search/stream", get(search_stream))
        .route("/api/v1/file/find", get(file_find))
        .route("/api/v1/paths/resolve", post(paths_resolve))
}

#[derive(Deserialize, Default)]
struct FuzzyQ {
    q: Option<String>,
    limit: Option<usize>,
    boost: Option<String>,
}

async fn fuzzy(
    State(s): State<Arc<AppState>>,
    Query(q): Query<FuzzyQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = q.q.unwrap_or_default();
    if query.len() > 512 {
        return Err(ApiError::bad_request("q over 512 chars"));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let ws = s.ws();
    let snap = ws.index.file_index.load();
    let boost: HashSet<String> = q
        .boost
        .unwrap_or_default()
        .split(',')
        .take(20)
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    let t0 = std::time::Instant::now();
    let qstr = query.clone();
    let snap2 = snap.clone();
    let hits = tokio::task::spawn_blocking(move || {
        ferro_core::fuzzy::rank_snap(&snap2, &query, limit, &boost)
    })
    .await
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "fuzzy task failed"))?;
    let results: Vec<serde_json::Value> = hits
        .into_iter()
        .map(|h| {
            let path = snap.paths[h.index].clone();
            serde_json::json!({ "path": path, "score": h.score, "positions": h.positions })
        })
        .collect();
    let total = results.len();
    Ok(Json(serde_json::json!({
        "q": qstr,
        "total": total,
        "ms": t0.elapsed().as_millis(),
        "generation": ws.generation.load(std::sync::atomic::Ordering::Relaxed),
        "results": results,
    })))
}

#[derive(Deserialize, Default)]
struct SearchQ {
    q: Option<String>,
    mode: Option<String>,
    case: Option<String>,
    #[serde(default, deserialize_with = "de_flag")]
    word: Option<bool>,
    include: Option<String>,
    exclude: Option<String>,
    #[serde(rename = "maxFiles")]
    max_files: Option<usize>,
    #[serde(rename = "maxPerFile")]
    max_per_file: Option<usize>,
}

/// Accept `?word=1`, `?word=0`, `?word=true`, `?word=false` (API.md writes 0/1).
fn de_flag<'de, D>(d: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = serde::Deserialize::deserialize(d)?;
    match opt.as_deref() {
        None => Ok(None),
        Some("1" | "true" | "yes") => Ok(Some(true)),
        Some("0" | "false" | "no") => Ok(Some(false)),
        Some(other) => Err(serde::de::Error::custom(format!("bad flag: {other}"))),
    }
}

fn build_query(s: &Arc<AppState>, q: &SearchQ) -> Result<ferro_core::scan::Query, ApiError> {
    let pattern = q.q.clone().unwrap_or_default();
    if pattern.is_empty() {
        return Err(ApiError::bad_request("q required"));
    }
    if pattern.len() > 512 {
        return Err(ApiError::bad_request("q over 512 chars"));
    }
    let mode = match q.mode.as_deref().unwrap_or("literal") {
        "literal" => ferro_core::scan::Mode::Literal,
        "regex" => ferro_core::scan::Mode::Regex,
        other => return Err(ApiError::bad_request(format!("bad mode: {other}"))),
    };
    let case = match q.case.as_deref().unwrap_or("smart") {
        "smart" => ferro_core::scan::Case::Smart,
        "insensitive" => ferro_core::scan::Case::Insensitive,
        "sensitive" => ferro_core::scan::Case::Sensitive,
        other => return Err(ApiError::bad_request(format!("bad case: {other}"))),
    };
    let split = |v: &Option<String>| {
        v.clone()
            .unwrap_or_default()
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
    };
    let ws = s.ws();
    let eff = s.settings.effective(&ws.key);
    let mut query = ferro_core::scan::Query::literal(pattern);
    query.mode = mode;
    query.case = case;
    query.word = q.word.unwrap_or(false);
    query.include = split(&q.include);
    query.exclude = split(&q.exclude);
    query.default_exclude = eff
        .get("search.exclude")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|x| x.to_string()))
                .collect()
        })
        .unwrap_or_else(|| vec!["**/vendor/**".into()]);
    query.max_files = q
        .max_files
        .unwrap_or(200)
        .clamp(1, s.limits.max_search_files);
    query.max_per_file = q.max_per_file.unwrap_or(20).clamp(1, 1000);
    query.max_file_bytes = eff
        .get("search.maxFileBytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(8 * 1024 * 1024);
    Ok(query)
}

fn to_response(r: ferro_core::scan::SearchResponse) -> serde_json::Value {
    serde_json::json!({
        "q": r.q,
        "engine": r.engine,
        "ms": r.ms,
        "filesScanned": r.files_scanned,
        "filesMatched": r.files_matched,
        "truncated": r.truncated,
        "excluded": { "globs": r.excluded.globs, "files": r.excluded.files },
        "files": r.files.iter().map(|f| serde_json::json!({
            "path": f.path,
            "hits": f.hits.iter().map(|h| serde_json::json!({
                "line": h.line, "text": h.text,
                "ranges": h.ranges.iter().map(|(a, b)| vec![a, b]).collect::<Vec<_>>(),
                "cutStart": h.cut_start, "cutEnd": h.cut_end, "def": h.def,
            })).collect::<Vec<_>>(),
            "more": f.more,
        })).collect::<Vec<_>>(),
    })
}

async fn search(
    State(s): State<Arc<AppState>>,
    Query(q): Query<SearchQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = build_query(&s, &q)?;
    // Compile up front so regex errors are 400 with detail.position.
    if let Err((msg, pos)) = ferro_core::scan::compile_query(&query) {
        return Err(ApiError::detail(
            crate::error::ErrorCode::BadRequest,
            format!("bad regex: {msg}"),
            serde_json::json!({ "position": pos }),
        ));
    }
    // At most 2 concurrent full scans (§ 4.3); extra ones wait here.
    let _permit = s
        .search_slots
        .acquire()
        .await
        .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "search overloaded"))?;
    let ws = s.ws();
    let snap = ws.index.file_index.load();
    let root = ws.root.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let out =
        tokio::task::spawn_blocking(move || ferro_core::scan::search(&snap, &root, &query, &stop))
            .await
            .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "search task failed"))?;
    match out {
        Ok(r) => Ok(Json(to_response(r))),
        Err(e) => Err(ApiError::detail(
            crate::error::ErrorCode::BadRequest,
            format!("bad regex: {e}"),
            serde_json::json!({ "position": 0 }),
        )),
    }
}
async fn search_stream(
    State(s): State<Arc<AppState>>,
    Query(q): Query<SearchQ>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>>, ApiError>
{
    let query = build_query(&s, &q)?;
    // Compile up front so regex errors are 400, not stream errors.
    let re = ferro_core::scan::compile_query(&query).map_err(|(msg, pos)| {
        ApiError::detail(
            crate::error::ErrorCode::BadRequest,
            format!("bad regex: {msg}"),
            serde_json::json!({ "position": pos }),
        )
    })?;
    let ws = s.ws();
    let snap = ws.index.file_index.load();
    let root = ws.root.clone();
    let (cands, exclude_globs, excluded_files) = ferro_core::scan::candidates(&snap, &query);
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);
    // The semaphore is held by this worker only (released when it finishes);
    // the handler future itself must stay responsive to client disconnects.
    let permit = s
        .search_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "search overloaded"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let t0 = std::time::Instant::now();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut scanned = 0usize;
        let mut matched = 0usize;
        let mut truncated = false;
        let mut since_progress = 0usize;
        for shard in cands.chunks(2000) {
            let (mut files, sc, mt, trunc) =
                ferro_core::scan::search_candidates(&snap, &root, &query, &re, &stop, shard);
            scanned += sc;
            matched += mt;
            truncated = truncated || trunc;
            // search_candidates sorts per shard; global order restored at done.
            // Emit in path order per shard for stable streaming.
            for f in std::mem::take(&mut files) {
                let ev = Event::default().event("file").json_data(serde_json::json!({
                    "path": f.path,
                    "hits": f.hits.iter().map(|h| serde_json::json!({
                        "line": h.line, "text": h.text,
                        "ranges": h.ranges.iter().map(|(a, b)| vec![a, b]).collect::<Vec<_>>(),
                        "cutStart": h.cut_start, "cutEnd": h.cut_end, "def": h.def,
                    })).collect::<Vec<_>>(),
                    "more": f.more,
                }));
                match ev {
                    Ok(ev) => {
                        if tx.blocking_send(ev).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            since_progress += sc;
            if since_progress >= 2000 {
                since_progress = 0;
                let ev = Event::default()
                    .event("progress")
                    .json_data(serde_json::json!({ "filesScanned": scanned }));
                if let Ok(ev) = ev {
                    if tx.blocking_send(ev).is_err() {
                        return;
                    }
                }
            }
            if truncated {
                break;
            }
        }
        let done = Event::default().event("done").json_data(serde_json::json!({
            "engine": "scan",
            "ms": t0.elapsed().as_millis(),
            "filesScanned": scanned,
            "filesMatched": matched,
            "truncated": truncated,
            "excluded": { "globs": exclude_globs, "files": excluded_files },
        }));
        if let Ok(done) = done {
            let _ = tx.blocking_send(done);
        }
    });
    Ok(Sse::new(
        tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok),
    ))
}

#[derive(Deserialize)]
struct FindQ {
    path: String,
    q: Option<String>,
    mode: Option<String>,
    case: Option<String>,
    #[serde(default, deserialize_with = "de_flag")]
    word: Option<bool>,
    limit: Option<usize>,
}

async fn file_find(
    State(s): State<Arc<AppState>>,
    Query(q): Query<FindQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pattern = q.q.unwrap_or_default();
    if pattern.is_empty() {
        return Err(ApiError::bad_request("q required"));
    }
    let ws = s.ws();
    let abs = super::files::resolve_pub(&s, &q.path)?;
    if abs.metadata().map(|m| !m.is_file()).unwrap_or(true) {
        return Err(ApiError::not_found(format!("not found: {}", q.path)));
    }
    let mut query = ferro_core::scan::Query::literal(pattern);
    query.mode = match q.mode.as_deref().unwrap_or("literal") {
        "literal" => ferro_core::scan::Mode::Literal,
        "regex" => ferro_core::scan::Mode::Regex,
        other => return Err(ApiError::bad_request(format!("bad mode: {other}"))),
    };
    query.case = match q.case.as_deref().unwrap_or("smart") {
        "smart" => ferro_core::scan::Case::Smart,
        "insensitive" => ferro_core::scan::Case::Insensitive,
        "sensitive" => ferro_core::scan::Case::Sensitive,
        other => return Err(ApiError::bad_request(format!("bad case: {other}"))),
    };
    query.word = q.word.unwrap_or(false);
    if let Err((msg, pos)) = ferro_core::scan::compile_query(&query) {
        return Err(ApiError::detail(
            crate::error::ErrorCode::BadRequest,
            format!("bad regex: {msg}"),
            serde_json::json!({ "position": pos }),
        ));
    }
    let limit = q.limit.unwrap_or(10000).clamp(1, 10000);
    let root = ws.root.clone();
    let rel = q.path.clone();
    let out = tokio::task::spawn_blocking(move || {
        ferro_core::scan::find_in_file(&root, &rel, &query, limit)
    })
    .await
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "find task failed"))?;
    match out {
        Ok(r) => Ok(Json(serde_json::json!({
            "total": r.total,
            "truncated": r.truncated,
            "matches": r.matches.iter().map(|m| serde_json::json!({
                "line": m.line,
                "ranges": m.ranges.iter().map(|(a, b)| vec![a, b]).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }))),
        Err(e) => Err(ApiError::detail(
            crate::error::ErrorCode::BadRequest,
            format!("bad regex: {e}"),
            serde_json::json!({ "position": 0 }),
        )),
    }
}

async fn paths_resolve(
    State(s): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > 1024 * 1024 {
        return Err(ApiError::new(
            crate::error::ErrorCode::TooLarge,
            "body over 1 MiB",
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let cands: Vec<String> = v
        .get("candidates")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|x| x.to_string()))
                .take(200)
                .collect()
        })
        .unwrap_or_default();
    let ws = s.ws();
    let snap = ws.index.file_index.load();
    let out =
        tokio::task::spawn_blocking(move || ferro_core::scan::resolve_candidates(&snap, &cands))
            .await
            .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "resolve task failed"))?;
    let map: serde_json::Map<String, serde_json::Value> = out
        .into_iter()
        .map(|(k, r)| {
            let v = match r {
                Some(x) => serde_json::json!({ "path": x.path, "line": x.line, "endLine": x.end_line, "col": x.col }),
                None => serde_json::Value::Null,
            };
            (k, v)
        })
        .collect();
    Ok(Json(serde_json::json!({ "resolved": map })))
}
