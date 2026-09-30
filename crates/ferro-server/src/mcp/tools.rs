//! MCP tool schemas and dispatch (maps `ferro_*` to ferro-agent / review APIs).

use std::sync::Arc;

use ferro_agent::provider_v2::ToolCallV2;
use serde_json::Value;

use crate::agent_ctx;
use crate::state::{AppState, PrSession};
use crate::v1::review::publish_drafts;

use ferro_forge::store::{DraftSource, NewDraft};

pub const PROTOCOL_VERSION: &str = "2024-11-05";

fn prop(kind: &str, description: &str) -> Value {
    serde_json::json!({"type": kind, "description": description})
}

fn schema(props: &[(&str, &str, &str)], required: &[&str]) -> Value {
    let mut map = serde_json::Map::new();
    for (name, kind, desc) in props {
        map.insert(name.to_string(), prop(kind, desc));
    }
    serde_json::json!({
        "type": "object",
        "properties": map,
        "required": required,
    })
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool_def(
            "ferro_search",
            "Full-text search across the workspace. Returns path:line hits.",
            schema(
                &[
                    ("q", "string", "Literal query, or regex when mode is regex."),
                    ("mode", "string", "'literal' (default) or 'regex'."),
                    (
                        "case",
                        "string",
                        "'smart' (default), 'insensitive', or 'sensitive'.",
                    ),
                    ("word", "boolean", "Whole-word match."),
                    (
                        "include",
                        "string",
                        "Comma-separated gitignore-style globs to include.",
                    ),
                    ("exclude", "string", "Comma-separated globs to exclude."),
                    ("maxFiles", "integer", "Max files with hits."),
                    ("maxPerFile", "integer", "Max hits per file."),
                ],
                &["q"],
            ),
        ),
        tool_def(
            "ferro_fuzzy",
            "Fuzzy-find files by name. Returns ranked paths.",
            schema(
                &[
                    ("q", "string", "Filename characters in order."),
                    ("limit", "integer", "Max results."),
                    ("boost", "string", "Comma-separated paths to prefer."),
                ],
                &["q"],
            ),
        ),
        tool_def(
            "ferro_read",
            "Read a workspace-relative file window with line numbers.",
            schema(
                &[
                    ("path", "string", "Workspace-relative file path."),
                    ("start", "integer", "First line, 1-based. Defaults to 1."),
                    ("count", "integer", "Max lines. Defaults to 200."),
                ],
                &["path"],
            ),
        ),
        tool_def(
            "ferro_outline",
            "List symbols (functions, classes, headings) in a file.",
            schema(
                &[("path", "string", "Workspace-relative file path.")],
                &["path"],
            ),
        ),
        tool_def(
            "ferro_definition",
            "Go to the definition of the identifier at a position.",
            schema(
                &[
                    ("path", "string", "Workspace-relative file path."),
                    ("line", "integer", "1-based line of the identifier use."),
                    ("col", "integer", "1-based column of the identifier use."),
                ],
                &["path", "line", "col"],
            ),
        ),
        tool_def(
            "ferro_references",
            "Find references to the identifier at a position.",
            schema(
                &[
                    ("path", "string", "Workspace-relative file path."),
                    ("line", "integer", "1-based line of the identifier."),
                    ("col", "integer", "1-based column of the identifier."),
                    ("limit", "integer", "Max results."),
                ],
                &["path", "line", "col"],
            ),
        ),
        tool_def(
            "ferro_diff",
            "Unified diff for one file or the whole tree between base and target.",
            schema(
                &[
                    (
                        "path",
                        "string",
                        "Workspace-relative file path. Omit for the whole tree.",
                    ),
                    ("base", "string", "Base rev. Defaults to HEAD."),
                    (
                        "target",
                        "string",
                        "'worktree' (default), 'index', or a rev.",
                    ),
                    ("context", "integer", "Context lines. Defaults to 3."),
                ],
                &[],
            ),
        ),
        tool_def(
            "ferro_pr_threads",
            "List PR review threads and conversation comments (PR mode only).",
            schema(&[], &[]),
        ),
        tool_def(
            "ferro_add_draft",
            "Add a review draft comment on the current PR for the human to publish.",
            schema(
                &[
                    ("path", "string", "Workspace-relative file path."),
                    ("line", "integer", "End line of the comment range."),
                    ("startLine", "integer", "Start line (optional)."),
                    ("side", "string", "'LEFT' or 'RIGHT' (default RIGHT)."),
                    ("body", "string", "Draft comment body (markdown)."),
                    ("threadId", "string", "Optional thread to attach to."),
                ],
                &["path", "line", "body"],
            ),
        ),
    ]
}

fn tool_def(name: &str, description: &str, input_schema: Value) -> Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn map_tool_name(mcp_name: &str) -> Option<&'static str> {
    match mcp_name {
        "ferro_search" => Some("search"),
        "ferro_fuzzy" => Some("fuzzy"),
        "ferro_read" => Some("read_file"),
        "ferro_outline" => Some("outline"),
        "ferro_definition" => Some("definition"),
        "ferro_references" => Some("references"),
        "ferro_diff" => Some("git_diff"),
        _ => None,
    }
}

fn tool_result_text(text: String, is_error: bool) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// Synchronous MCP tools (workspace reads).
pub fn call_sync(s: &Arc<AppState>, name: &str, args: Value) -> Value {
    let ws = s.ws();
    match name {
        "ferro_pr_threads" | "ferro_add_draft" => {
            return tool_result_text("use async handler for PR tools".into(), true);
        }
        _ => {}
    }
    let internal = map_tool_name(name);
    if internal.is_none() {
        return tool_result_text(format!("unknown tool: {name}"), true);
    }
    let internal = internal.unwrap();
    let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".into());
    let input_ok = true;
    let call = ToolCallV2 {
        id: "mcp".into(),
        name: internal.to_string(),
        input: args,
        input_raw: args_str,
        input_ok,
    };
    let ctx = agent_ctx::tool_ctx_for(&ws, s);
    let out = ferro_agent::dispatch_v2(&ctx, &call);
    tool_result_text(out.output, !out.ok)
}

pub async fn call_async(s: &Arc<AppState>, name: &str, args: Value) -> Value {
    match name {
        "ferro_pr_threads" => pr_threads(s).await,
        "ferro_add_draft" => add_draft(s, args).await,
        other => {
            // Search, diff, and outline read the worktree. Same rule as the
            // HTTP handlers: do not block the async runtime.
            let s = Arc::clone(s);
            let name = other.to_string();
            match tokio::task::spawn_blocking(move || call_sync(&s, &name, args)).await {
                Ok(v) => v,
                Err(_) => tool_result_text("tool task failed".into(), true),
            }
        }
    }
}

fn scrub_tool_text(s: &AppState, text: String) -> String {
    let ws = s.ws();
    let ctx = agent_ctx::tool_ctx_for(&ws, s);
    if ctx.redact_secrets {
        ferro_agent::redact_text(&text).0
    } else {
        text
    }
}

fn tool_text(s: &AppState, text: impl Into<String>, is_error: bool) -> Value {
    tool_result_text(scrub_tool_text(s, text.into()), is_error)
}

async fn pr_threads(s: &Arc<AppState>) -> Value {
    let ws = s.ws();
    let pr = match ws.pr.as_ref() {
        Some(p) => p.clone(),
        None => {
            return tool_text(
                s,
                serde_json::json!({
                    "threads": [],
                    "conversation": [],
                    "note": "not in PR mode"
                })
                .to_string(),
                false,
            );
        }
    };
    let (threads, conversation) = match pr.client.review_state(&pr.pr_ref).await {
        Ok(t) => t,
        Err(e) => return tool_text(s, format!("upstream: {e}"), true),
    };
    let out = serde_json::json!({
        "threads": threads.iter().map(|t| {
            serde_json::json!({
                "id": t.id,
                "path": t.path,
                "line": t.line,
                "startLine": t.start_line,
                "side": t.side,
                "comments": t.comments.iter().map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "author": c.author_login,
                        "body": c.body,
                    })
                }).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "conversation": conversation.iter().map(|c| {
            serde_json::json!({
                "id": c.id,
                "author": c.author_login,
                "body": c.body,
            })
        }).collect::<Vec<_>>(),
    });
    tool_text(s, out.to_string(), false)
}

/// Validate an agent draft before it touches the review store.
/// `source` is forced to `ai` and `findingId` is dropped so a client cannot
/// record the comment as a human or attach it to a finding.
fn prepare_draft(root: &std::path::Path, args: Value) -> Result<NewDraft, String> {
    let mut nd: NewDraft =
        serde_json::from_value(args).map_err(|e| format!("invalid draft: {e}"))?;
    nd.source = Some(DraftSource::Ai);
    nd.finding_id = None;
    nd.path = nd.path.trim().to_string();
    if nd.line == 0 || nd.path.is_empty() || nd.path.len() > 512 {
        return Err("path and line>0 required".into());
    }
    if nd.body.trim().is_empty() || nd.body.len() > 100_000 {
        return Err("body required (≤100 KiB)".into());
    }
    if let Some(sl) = nd.start_line {
        if sl == 0 || sl > nd.line {
            return Err("startLine must satisfy 0 < startLine <= line".into());
        }
    }
    if let Some(side) = &nd.side {
        if side != "LEFT" && side != "RIGHT" {
            return Err("side must be LEFT or RIGHT".into());
        }
    }
    ferro_core::paths::resolve(root, &nd.path, ferro_core::paths::Access::Read)
        .map_err(|e| e.to_string())?;
    Ok(nd)
}

async fn add_draft(s: &Arc<AppState>, args: Value) -> Value {
    let ws = s.ws();
    let root = ws.index.root().to_path_buf();
    let nd = match prepare_draft(&root, args) {
        Ok(d) => d,
        Err(e) => return tool_text(s, e, true),
    };
    let ctx = agent_ctx::tool_ctx_for(&ws, s);
    if ctx.is_refused(&nd.path) {
        return tool_text(s, "refused: never-send path", true);
    }
    let pr: Arc<PrSession> = match ws.pr.as_ref() {
        Some(p) => p.clone(),
        None => {
            return tool_text(s, "not in PR mode (open a PR workspace first)", true);
        }
    };
    let pr2 = pr.clone();
    let d = match tokio::task::spawn_blocking(move || pr2.store.add(nd)).await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return tool_text(s, e, true),
        Err(_) => return tool_text(s, "store task failed", true),
    };
    publish_drafts(s, &pr);
    tool_text(
        s,
        serde_json::to_string(&d).unwrap_or_else(|_| "{}".into()),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_draft_is_ai_and_stays_in_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let nd = prepare_draft(
            dir.path(),
            serde_json::json!({
                "path": "a.rs",
                "line": 2,
                "body": "check this",
                "source": "human",
                "findingId": "f1",
                "startLine": 1,
                "side": "RIGHT"
            }),
        )
        .unwrap();
        assert_eq!(nd.source, Some(DraftSource::Ai));
        assert!(nd.finding_id.is_none());
        assert_eq!(nd.path, "a.rs");

        let escaped = prepare_draft(
            dir.path(),
            serde_json::json!({"path": "../x", "line": 1, "body": "no"}),
        );
        assert!(escaped.is_err(), "{escaped:?}");
    }

    #[test]
    fn scrub_replaces_an_aws_key() {
        let (out, n) = ferro_agent::redact_text("token AKIAIOSFODNN7EXAMPLE in a comment");
        assert!(n >= 1);
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
    }
}
