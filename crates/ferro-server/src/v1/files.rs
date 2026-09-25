//! Files surface (API.md § 4): tree, file, lines, raw.
//! O(window) reads via the line-offset index; UTF-16 columns; lossy decode flagged.

use axum::{
    extract::{Query, State},
    http::header,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/tree", get(tree))
        .route("/api/v1/file", get(file))
        .route("/api/v1/file/lines", get(lines))
        .route("/api/v1/file/raw", get(raw))
}

fn ws_root(s: &Arc<AppState>) -> std::path::PathBuf {
    s.ws().root.clone()
}

pub(crate) fn resolve_pub(s: &Arc<AppState>, rel: &str) -> Result<std::path::PathBuf, ApiError> {
    resolve(s, rel)
}

fn resolve(s: &Arc<AppState>, rel: &str) -> Result<std::path::PathBuf, ApiError> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err(ApiError::bad_request("path required"));
    }
    // API paths are workspace-relative; a leading slash is tolerated and stripped.
    let rel = rel.trim_start_matches('/');
    ferro_core::paths::resolve(&ws_root(s), rel, ferro_core::paths::Access::Read).map_err(|e| {
        match e {
            ferro_core::paths::PathError::Empty => ApiError::bad_request("empty path"),
            ferro_core::paths::PathError::Escapes => {
                ApiError::new(crate::error::ErrorCode::Forbidden, "path outside workspace")
            }
            ferro_core::paths::PathError::Protected => {
                ApiError::new(crate::error::ErrorCode::Forbidden, "protected path")
            }
        }
    })
}

#[derive(Deserialize)]
struct TreeQ {
    dir: Option<String>,
}

async fn tree(
    State(s): State<Arc<AppState>>,
    Query(q): Query<TreeQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let dir_rel = q.dir.unwrap_or_default();
    let dir = if dir_rel.is_empty() || dir_rel == "/" {
        ws_root(&s)
    } else {
        let p = resolve(&s, &dir_rel)?;
        if !p.is_dir() {
            return Err(ApiError::not_found(format!("not a directory: {dir_rel}")));
        }
        p
    };
    let mut entries = Vec::new();
    let rd = std::fs::read_dir(&dir)
        .map_err(|_| ApiError::not_found(format!("cannot list: {dir_rel}")))?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), ".git" | ".hg" | ".svn") {
            continue;
        }
        let ft = e.file_type().ok();
        let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
        let is_symlink = ft.map(|t| t.is_symlink()).unwrap_or(false);
        let rel = e
            .path()
            .strip_prefix(ws_root(&s))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(name.clone());
        let (size, symlink_outside) = if is_dir {
            (None, None)
        } else {
            let abs = e.path();
            let outside = is_symlink
                && abs
                    .canonicalize()
                    .map(|c| !c.starts_with(ws_root(&s)))
                    .unwrap_or(false);
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            (Some(size), Some(outside))
        };
        entries.push(serde_json::json!({
            "name": name,
            "path": rel,
            "dir": is_dir,
            "size": size,
            "symlink": symlink_outside.unwrap_or(false),
        }));
    }
    // B3: merge cached git status (no per-request git spawn).
    let ws = s.ws();
    let git_cache = ws.git_status.read().clone();
    if let Some(st) = git_cache {
        use std::collections::{HashMap, HashSet};
        let mut codes: HashMap<&str, (&str, bool)> = HashMap::new();
        let mut changed_dirs: HashSet<String> = HashSet::new();
        for f in &st.files {
            let code = if f.untracked {
                "?"
            } else if f.conflicted {
                "U"
            } else {
                f.index.or(f.worktree).unwrap_or("M")
            };
            codes.insert(f.path.as_str(), (code, f.untracked));
            // Every ancestor directory is dirty.
            let mut dir = f.path.as_str();
            while let Some((parent, _)) = dir.rsplit_once('/') {
                changed_dirs.insert(parent.to_string());
                dir = parent;
            }
        }
        for e in entries.iter_mut() {
            let p = e["path"].as_str().unwrap_or("").to_string();
            let is_dir = e["dir"].as_bool().unwrap_or(false);
            if is_dir {
                if changed_dirs.contains(&p) {
                    e["dirty"] = serde_json::json!(true);
                }
            } else if let Some((code, _)) = codes.get(p.as_str()) {
                e["git"] = serde_json::json!(code);
            }
        }
    }
    entries.sort_by(|a, b| {
        let da = a["dir"].as_bool().unwrap_or(false);
        let db = b["dir"].as_bool().unwrap_or(false);
        db.cmp(&da).then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
        })
    });
    Ok(Json(serde_json::json!({
        "dir": dir_rel,
        "generation": s.ws().generation.load(std::sync::atomic::Ordering::Relaxed),
        "entries": entries,
    })))
}

pub(crate) fn language_of(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    Some(match ext.as_str() {
        "rs" => "Rust",
        "go" => "Go",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "java" => "Java",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "kt" | "kts" => "Kotlin",
        "sh" | "bash" => "Shell",
        "sql" => "SQL",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "md" | "markdown" => "Markdown",
        "html" | "xml" => "HTML",
        "css" => "CSS",
        _ => return None,
    })
}

fn eol_of(bytes: &[u8]) -> &'static str {
    let sample: &[u8] = &bytes[..bytes.len().min(1024 * 1024)];
    let crlf = sample.windows(2).filter(|w| w == b"\r\n").count();
    let lf = byte_count(sample, b'\n').saturating_sub(crlf);
    match (crlf > 0, lf > 0) {
        (true, true) => "mixed",
        (true, false) => "crlf",
        (false, true) => "lf",
        (false, false) => "none",
    }
}

fn byte_count(hay: &[u8], needle: u8) -> usize {
    hay.iter().filter(|&&b| b == needle).count()
}

fn has_nul(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}

#[derive(Deserialize)]
struct PathQ {
    path: String,
}

async fn file(
    State(s): State<Arc<AppState>>,
    Query(q): Query<PathQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let abs = resolve(&s, &q.path)?;
    let md = std::fs::metadata(&abs)
        .map_err(|_| ApiError::not_found(format!("not found: {}", q.path)))?;
    if !md.is_file() {
        return Err(ApiError::bad_request("not a file"));
    }
    let size = md.len();
    let mtime_ms = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let too_large = size > s.limits.max_raw_bytes;
    let head = read_head(&abs, 8192).unwrap_or_default();
    let is_img = ferro_core::media::is_image(&q.path);
    let kind = if is_img {
        "image"
    } else if has_nul(&head) {
        "binary"
    } else {
        "text"
    };
    let (lines, encoding) = if kind != "text" || too_large {
        (0, "utf-8")
    } else {
        let ws = s.ws();
        let total = ws.lines.total_lines(&abs).unwrap_or(0);
        // Encoding from a 1 MiB sample; deeper breakage still decodes lossy per line.
        let sample = read_head(&abs, 1024 * 1024).unwrap_or_default();
        let enc = if std::str::from_utf8(&sample).is_ok() {
            "utf-8"
        } else {
            "utf-8-lossy"
        };
        (total, enc)
    };
    Ok(Json(serde_json::json!({
        "path": q.path,
        "size": size,
        "mtimeMs": mtime_ms,
        "lines": lines,
        "kind": kind,
        "mime": is_img.then(|| ferro_core::media::content_type(&q.path)),
        "language": language_of(&q.path),
        "markdown": ferro_core::media::is_markdown(&q.path),
        "eol": if kind == "text" { eol_of(&read_head(&abs, 1024 * 1024).unwrap_or_default()) } else { "none" },
        "encoding": encoding,
        "git": null,
        "tooLarge": too_large,
    })))
}

fn read_head(path: &std::path::Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n.min(f.metadata().map(|m| m.len() as usize).unwrap_or(n))];
    let mut got = 0;
    while got < buf.len() {
        let r = f.read(&mut buf[got..])?;
        if r == 0 {
            break;
        }
        got += r;
    }
    buf.truncate(got);
    Ok(buf)
}

#[derive(Deserialize)]
struct LinesQ {
    path: String,
    from: Option<usize>,
    count: Option<usize>,
    hl: Option<u8>,
    #[serde(rename = "maxCols")]
    max_cols: Option<usize>,
}

async fn lines(
    State(s): State<Arc<AppState>>,
    Query(q): Query<LinesQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let abs = resolve(&s, &q.path)?;
    if abs.metadata().map(|m| !m.is_file()).unwrap_or(true) {
        return Err(ApiError::not_found(format!("not found: {}", q.path)));
    }
    let from = q.from.unwrap_or(1).max(1);
    let count = q.count.unwrap_or(500).clamp(1, s.limits.max_window_lines);
    let max_cols = q.max_cols.unwrap_or(s.limits.max_cols).max(1);
    // maxCols may go to 1,000,000 only when count == 1.
    let max_cols = if max_cols > s.limits.max_cols && count != 1 {
        s.limits.max_cols
    } else {
        max_cols.min(1_000_000)
    };
    let want_hl = q.hl.unwrap_or(1) != 0;
    let ws = s.ws();
    let path_s = q.path.clone();
    // All IO/CPU off the async runtime (§ 4.3).
    let out = tokio::task::spawn_blocking(move || {
        let (win, total) = crate::lines::read_window_bytes_with(&ws.lines, &abs, from, count)
            .map_err(|_| ApiError::not_found(format!("cannot read: {path_s}")))?;
        let mtime_ms = abs
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let hl = if want_hl {
            ws.hl
                .lock()
                .window(&abs, &path_s, from.saturating_sub(1), win.len(), None)
        } else {
            None
        };
        let mut lines = Vec::with_capacity(win.len());
        for (n, bytes) in win {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let units: usize = text.encode_utf16().count();
            // Lines over 20,000 UTF-16 units are returned unhighlighted.
            let force_plain = units > 20_000;
            let (body, cut) = if units > max_cols {
                let kept: Vec<u16> = text.encode_utf16().take(max_cols).collect();
                (String::from_utf16_lossy(&kept), Some(units))
            } else {
                (text, None)
            };
            let mut obj = serde_json::Map::new();
            obj.insert("n".into(), serde_json::json!(n));
            if want_hl && !force_plain {
                if let Some(h) = hl.as_ref().and_then(|w| w.lines.iter().find(|l| l.n == n)) {
                    obj.insert("html".into(), serde_json::Value::String(h.html.clone()));
                } else {
                    obj.insert("text".into(), serde_json::Value::String(body));
                }
            } else {
                obj.insert("text".into(), serde_json::Value::String(body));
            }
            if let Some(c) = cut {
                obj.insert("cut".into(), serde_json::json!(c));
            }
            lines.push(serde_json::Value::Object(obj));
        }
        Ok::<_, ApiError>((lines, total, mtime_ms, hl.map(|w| (w.syntax, w.exact))))
    })
    .await
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "lines task failed"))??;
    let (lines, total, mtime_ms, hl_meta) = out;
    let (language, exact) = match hl_meta {
        Some((syn, ex)) => (Some(syn), ex),
        None if want_hl => (language_of(&q.path).map(|x| x.to_string()), false),
        None => (language_of(&q.path).map(|x| x.to_string()), true),
    };
    Ok(Json(serde_json::json!({
        "path": q.path,
        "from": from,
        "total": total,
        "mtimeMs": mtime_ms,
        "exact": exact,
        "language": language,
        "lines": lines,
    })))
}

#[derive(Deserialize)]
struct RawQ {
    path: String,
    download: Option<u8>,
}

async fn raw(State(s): State<Arc<AppState>>, Query(q): Query<RawQ>) -> impl IntoResponse {
    use axum::http::HeaderMap;
    let abs = match resolve(&s, &q.path) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let size = abs.metadata().map(|m| m.len()).unwrap_or(0);
    if size > s.limits.max_raw_bytes {
        return ApiError::new(crate::error::ErrorCode::TooLarge, "file over maxRawBytes")
            .into_response();
    }
    let ws = s.ws();
    let bytes = match tokio::task::spawn_blocking(move || std::fs::read(&abs)).await {
        Ok(Ok(b)) => b,
        _ => {
            return ApiError::not_found(format!("cannot read: {}", ws.root.display()))
                .into_response()
        }
    };
    // Sniffed content type: known images by extension, binary on NUL bytes.
    let mime = if ferro_core::media::is_image(&q.path) {
        ferro_core::media::content_type(&q.path)
    } else if bytes[..bytes.len().min(8192)].contains(&0) {
        "application/octet-stream"
    } else {
        "text/plain; charset=utf-8"
    };
    let name = q
        .path
        .rsplit('/')
        .next()
        .unwrap_or("file")
        .replace(['"', '\\', '\r', '\n'], "");
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, mime.parse().unwrap());
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; sandbox"
            .parse()
            .unwrap(),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!(
            "{}; filename=\"{name}\"",
            if q.download.unwrap_or(0) != 0 {
                "attachment"
            } else {
                "inline"
            }
        )
        .parse()
        .unwrap(),
    );
    (headers, bytes).into_response()
}
