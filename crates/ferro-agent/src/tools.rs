//! Tool registry + dispatcher. Read-only tools run on ferro-core.
//! Write tools (apply_patch, create_file) land in P2-3; their defs are
//! listed here already so the model sees stable schemas.

use serde::{Deserialize, Serialize};

use crate::policy::{Access, Sandbox};
use ferro_core::Index;

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub access: AccessLabel,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccessLabel {
    Read,
    Write,
    Destructive,
}

impl From<Access> for AccessLabel {
    fn from(a: Access) -> Self {
        match a {
            Access::Read => AccessLabel::Read,
            Access::Write => AccessLabel::Write,
            Access::Destructive => AccessLabel::Destructive,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
    pub truncated: bool,
}

impl ToolResult {
    fn ok(output: String) -> Self {
        const CAP: usize = 12 * 1024;
        if output.len() > CAP {
            Self {
                output: output[..CAP].to_string(),
                truncated: true,
                ok: true,
            }
        } else {
            Self {
                output,
                truncated: false,
                ok: true,
            }
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            output: msg.into(),
            truncated: false,
        }
    }
}

fn schema(props: &[(&str, &str)], required: &[&str]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, t) in props {
        map.insert(k.to_string(), serde_json::json!({"type": t}));
    }
    serde_json::json!({
        "type": "object",
        "properties": map,
        "required": required,
    })
}

pub fn registry() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read_file",
            description: "Read a workspace-relative file window. Prefer over full reads.",
            access: AccessLabel::Read,
            params: schema(
                &[("path", "string"), ("start", "integer"), ("count", "integer")],
                &["path"],
            ),
        },
        ToolDef {
            name: "list_files",
            description: "List indexed workspace files.",
            access: AccessLabel::Read,
            params: schema(&[], &[]),
        },
        ToolDef {
            name: "fuzzy",
            description: "Fuzzy-find files by name.",
            access: AccessLabel::Read,
            params: schema(&[("q", "string"), ("limit", "integer")], &["q"]),
        },
        ToolDef {
            name: "grep",
            description: "Full-text search across the workspace.",
            access: AccessLabel::Read,
            params: schema(&[("q", "string"), ("limit", "integer")], &["q"]),
        },
        ToolDef {
            name: "git_status",
            description: "git status porcelain.",
            access: AccessLabel::Read,
            params: schema(&[], &[]),
        },
        ToolDef {
            name: "git_diff",
            description: "git diff vs HEAD, optionally scoped to a path.",
            access: AccessLabel::Read,
            params: schema(&[("path", "string")], &[]),
        },
        ToolDef {
            name: "apply_patch",
            description: "Apply a unified diff patch under the workspace root. Requires --allow-write. Coming in P2-3.",
            access: AccessLabel::Write,
            params: schema(&[("patch", "string")], &["patch"]),
        },
    ]
}

fn arg_str(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key)?.as_str().map(|s| s.to_string())
}

fn arg_usize(args: &serde_json::Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(default)
}

/// Dispatch a single tool call synchronously against the live index.
pub fn dispatch(index: &Index, sandbox: &Sandbox, call: &ToolCall) -> ToolResult {
    match call.name.as_str() {
        "read_file" => {
            if let Err(e) = sandbox.check(Access::Read) {
                return ToolResult::err(e.to_string());
            }
            let Some(path) = arg_str(&call.args, "path") else {
                return ToolResult::err("missing required arg: path");
            };
            let start = arg_usize(&call.args, "start", 0);
            let count = arg_usize(&call.args, "count", 200).clamp(1, 1000);
            match index.read_window(&path, start, count) {
                Some(w) => {
                    let body: Vec<String> = w
                        .lines
                        .iter()
                        .map(|l| format!("{:>6}  {}", l.n, l.text))
                        .collect();
                    ToolResult::ok(format!("{} ({} lines)\n{}", path, w.total, body.join("\n")))
                }
                None => ToolResult::err(format!("cannot read: {path}")),
            }
        }
        "list_files" => {
            if let Err(e) = sandbox.check(Access::Read) {
                return ToolResult::err(e.to_string());
            }
            let snap = index.snapshot();
            let mut out = format!("{} files\n", snap.len());
            for f in snap.iter().take(300) {
                out.push_str(&f.path);
                out.push('\n');
            }
            if snap.len() > 300 {
                out.push_str(&format!("... ({} more, use fuzzy)\n", snap.len() - 300));
            }
            ToolResult::ok(out)
        }
        "fuzzy" => {
            if let Err(e) = sandbox.check(Access::Read) {
                return ToolResult::err(e.to_string());
            }
            let Some(q) = arg_str(&call.args, "q") else {
                return ToolResult::err("missing required arg: q");
            };
            let limit = arg_usize(&call.args, "limit", 20).clamp(1, 50);
            let paths: Vec<String> = index.snapshot().into_iter().map(|f| f.path).collect();
            let ranked = ferro_core::fuzzy::rank(&q, &paths, limit);
            let body: Vec<String> = ranked.iter().map(|(p, s)| format!("{p} ({s})")).collect();
            ToolResult::ok(body.join("\n"))
        }
        "grep" => {
            if let Err(e) = sandbox.check(Access::Read) {
                return ToolResult::err(e.to_string());
            }
            let Some(q) = arg_str(&call.args, "q") else {
                return ToolResult::err("missing required arg: q");
            };
            let limit = arg_usize(&call.args, "limit", 20).clamp(1, 50);
            let hits = ferro_core::search::grep(index.root(), &q, limit);
            let body: Vec<String> = hits
                .iter()
                .map(|h| {
                    format!(
                        "{}:{} {}",
                        h.path,
                        h.line,
                        h.text.chars().take(160).collect::<String>()
                    )
                })
                .collect();
            ToolResult::ok(if body.is_empty() {
                "(no matches)".into()
            } else {
                body.join("\n")
            })
        }
        "git_status" => {
            if let Err(e) = sandbox.check(Access::Read) {
                return ToolResult::err(e.to_string());
            }
            ToolResult::ok(ferro_core::git::status(index.root()))
        }
        "git_diff" => {
            if let Err(e) = sandbox.check(Access::Read) {
                return ToolResult::err(e.to_string());
            }
            let path = arg_str(&call.args, "path");
            ToolResult::ok(ferro_core::git::diff_head(index.root(), path.as_deref()))
        }
        "apply_patch" => ToolResult::err("apply_patch lands in P2-3 (write tools disabled)"),
        other => ToolResult::err(format!("unknown tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_index() -> (tempfile::TempDir, Index) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join("hello.rs"))
            .unwrap()
            .write_all(b"fn hello() {}\nfn world() {}\n")
            .unwrap();
        let idx = Index::new(dir.path().to_path_buf());
        (dir, idx)
    }

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            name: name.into(),
            args,
        }
    }

    #[test]
    fn registry_labels_every_tool() {
        let reg = registry();
        assert!(reg.len() >= 7);
        assert!(reg.iter().any(|t| t.access == AccessLabel::Write));
        assert!(reg.iter().filter(|t| t.access == AccessLabel::Read).count() >= 6);
    }

    #[test]
    fn read_roundtrip() {
        let (_dir, idx) = test_index();
        let sb = Sandbox::readonly(idx.root().to_path_buf());
        let r = dispatch(
            &idx,
            &sb,
            &call("read_file", serde_json::json!({"path": "hello.rs"})),
        );
        assert!(r.ok);
        assert!(r.output.contains("fn hello"));
    }

    #[test]
    fn traversal_blocked() {
        let (_dir, idx) = test_index();
        let sb = Sandbox::readonly(idx.root().to_path_buf());
        let r = dispatch(
            &idx,
            &sb,
            &call("read_file", serde_json::json!({"path": "../../etc/passwd"})),
        );
        assert!(!r.ok);
    }

    #[test]
    fn unknown_tool_errors() {
        let (_dir, idx) = test_index();
        let sb = Sandbox::readonly(idx.root().to_path_buf());
        let r = dispatch(&idx, &sb, &call("rm_rf", serde_json::json!({})));
        assert!(!r.ok);
    }

    #[test]
    fn output_truncated_at_cap() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..500)
            .map(|i| format!("line {i:04} padding to make this row long enough xx\n"))
            .collect();
        std::fs::write(dir.path().join("big.txt"), &body).unwrap();
        let idx = Index::new(dir.path().to_path_buf());
        let sb = Sandbox::readonly(idx.root().to_path_buf());
        let r = dispatch(
            &idx,
            &sb,
            &call(
                "read_file",
                serde_json::json!({"path": "big.txt", "count": 500}),
            ),
        );
        assert!(r.ok);
        assert!(r.truncated);
    }
}
