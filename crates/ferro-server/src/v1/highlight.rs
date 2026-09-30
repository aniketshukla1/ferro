//! POST /api/v1/highlight — highlight ad-hoc code (API.md § 4.6).
//! Used for fences in AI answers and comments.

use axum::{body::Bytes, routing::post, Json, Router};
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/highlight", post(highlight))
}

async fn highlight(body: Bytes) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > 256 * 1024 {
        return Err(ApiError::new(
            crate::error::ErrorCode::TooLarge,
            "code over 256 KiB",
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let code = v.get("code").and_then(|c| c.as_str()).unwrap_or("");
    let language = v.get("language").and_then(|l| l.as_str());
    let path = v.get("path").and_then(|p| p.as_str());
    let path_buf;
    let path_opt = if let Some(p) = path {
        path_buf = std::path::PathBuf::from(p);
        Some(path_buf.as_path())
    } else {
        None
    };
    let out = tokio::task::spawn_blocking({
        let code = code.to_string();
        let language = language.map(|s| s.to_string());
        let path_owned = path_opt.map(|p| p.to_path_buf());
        move || highlight_text(&code, language.as_deref(), path_owned.as_deref())
    })
    .await
    .map_err(|_| ApiError::new(crate::error::ErrorCode::Internal, "highlight task failed"))?;
    Ok(Json(
        serde_json::json!({ "language": out.0, "lines": out.1 }),
    ))
}

fn highlight_text(
    code: &str,
    language: Option<&str>,
    path: Option<&std::path::Path>,
) -> (Option<String>, Vec<String>) {
    use crate::hl::{find_syntax, highlight_line, syntax_set};
    use syntect::parsing::{ParseState, ScopeStack};
    let ss = syntax_set();
    let syntax = find_syntax(ss, language, path);
    let name = if syntax.name == "Plain Text" {
        None
    } else {
        Some(syntax.name.clone())
    };
    let mut parse = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let lines = code
        .split('\n')
        .map(|raw| {
            if raw.encode_utf16().count() > 20_000 {
                let mut e = String::new();
                for c in raw.chars() {
                    match c {
                        '&' => e.push_str("&amp;"),
                        '<' => e.push_str("&lt;"),
                        '>' => e.push_str("&gt;"),
                        _ => e.push(c),
                    }
                }
                e
            } else {
                highlight_line(raw, ss, &mut parse, &mut stack)
            }
        })
        .collect();
    (name, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_and_long_lines() {
        let (lang, lines) = highlight_text("fn main() {}\n", Some("rs"), None);
        assert_eq!(lang.as_deref(), Some("Rust"));
        assert!(lines[0].contains("t-k"));
        let big = "x".repeat(21_000);
        let (_, lines) = highlight_text(&big, Some("rs"), None);
        assert!(!lines[0].contains("class="));
    }
}
