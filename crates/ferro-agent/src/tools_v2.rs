//! Read-only agent tools v2 (B5): windowed reads, new-engine search, fuzzy,
//! outline, base-aware diffs, change lists, and old-side blob reads.
//! Every tool runs synchronously so the loop can confine it to
//! `spawn_blocking`; the loop wraps all output as untrusted data.

use std::sync::Arc;

use ferro_core::fileindex::FileSnapshot;
use ferro_core::fuzzy;
use ferro_core::git::GitRepo;
use ferro_core::scan;
use ferro_core::Index;

use crate::provider_v2::{ToolCallV2, ToolSchema};

/// Tool output cap (API.md § 10.2 `tool_result.output`).
pub const TOOL_OUTPUT_CHARS: usize = 2000;

/// Execution context: live index snapshot plus read-only git access.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    pub index: Arc<Index>,
    pub snapshot: Arc<FileSnapshot>,
    pub git_root: Option<std::path::PathBuf>,
    pub default_exclude: Vec<String>,
    pub max_file_bytes: u64,
    pub max_files: usize,
    pub max_per_file: usize,
}

impl ToolCtx {
    pub fn new(index: Arc<Index>) -> Self {
        let snapshot = index.file_index.load();
        let root = index.root().to_path_buf();
        let git_root = root.join(".git").exists().then_some(root);
        Self {
            index,
            snapshot,
            git_root,
            default_exclude: vec!["**/vendor/**".into()],
            max_file_bytes: 8 * 1024 * 1024,
            max_files: 50,
            max_per_file: 5,
        }
    }

    /// Refresh the snapshot handle (cheap `Arc` load).
    pub fn refresh(&mut self) {
        self.snapshot = self.index.file_index.load();
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub ok: bool,
    pub output: String,
    pub truncated: bool,
}

impl ToolOutput {
    fn ok(output: String) -> Self {
        let mut chars = output.chars();
        let head: String = chars.by_ref().take(TOOL_OUTPUT_CHARS).collect();
        let truncated = chars.next().is_some();
        Self {
            ok: true,
            output: head,
            truncated,
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

fn prop(kind: &str, description: &str) -> serde_json::Value {
    serde_json::json!({"type": kind, "description": description})
}

fn schema(props: &[(&str, &str, &str)], required: &[&str]) -> serde_json::Value {
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

/// Tool schemas served to the model (deterministic order for caching).
pub fn tool_schemas() -> Vec<ToolSchema> {
    vec![
        ToolSchema {
            name: "read_file".into(),
            description:
                "Read a workspace-relative file window with line numbers. Prefer over full reads."
                    .into(),
            input_schema: schema(
                &[
                    ("path", "string", "Workspace-relative file path."),
                    ("start", "integer", "First line, 1-based. Defaults to 1."),
                    ("count", "integer", "Max lines. Defaults to 200."),
                ],
                &["path"],
            ),
            strict: false,
        },
        ToolSchema {
            name: "search".into(),
            description: "Full-text search across the workspace. Returns path:line hits.".into(),
            input_schema: schema(
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
            strict: false,
        },
        ToolSchema {
            name: "fuzzy".into(),
            description: "Fuzzy-find files by name. Returns ranked paths.".into(),
            input_schema: schema(
                &[
                    ("q", "string", "Filename characters in order."),
                    ("limit", "integer", "Max results."),
                    ("boost", "string", "Comma-separated paths to prefer."),
                ],
                &["q"],
            ),
            strict: false,
        },
        ToolSchema {
            name: "outline".into(),
            description: "List symbols (functions, classes, headings) in a file.".into(),
            input_schema: schema(
                &[("path", "string", "Workspace-relative file path.")],
                &["path"],
            ),
            strict: false,
        },
        ToolSchema {
            name: "git_diff".into(),
            description: "Unified diff for one file between base and target.".into(),
            input_schema: schema(
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
            strict: false,
        },
        ToolSchema {
            name: "list_changes".into(),
            description: "Changed files with stats between base and target.".into(),
            input_schema: schema(
                &[
                    ("base", "string", "Base rev. Defaults to HEAD."),
                    (
                        "target",
                        "string",
                        "'worktree' (default), 'index', or a rev.",
                    ),
                ],
                &[],
            ),
            strict: false,
        },
        ToolSchema {
            name: "read_blob".into(),
            description: "Read a file window from a revision (old side).".into(),
            input_schema: schema(
                &[
                    ("rev", "string", "Revision. Defaults to HEAD."),
                    ("path", "string", "Workspace-relative file path."),
                    ("start", "integer", "First line, 1-based. Defaults to 1."),
                    ("count", "integer", "Max lines. Defaults to 200."),
                ],
                &["path"],
            ),
            strict: false,
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

fn arg_bool(args: &serde_json::Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn require_str(args: &serde_json::Value, key: &str) -> Result<String, ToolOutput> {
    match arg_str(args, key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(s) => Ok(s),
        None => Err(ToolOutput::err(format!("missing required arg: {key}"))),
    }
}

/// Dispatch one validated tool call. Synchronous: the loop runs it in
/// `spawn_blocking`. Unknown tools and bad args become errors, never panics.
pub fn dispatch_v2(ctx: &ToolCtx, call: &ToolCallV2) -> ToolOutput {
    if !call.input_ok {
        let raw: String = call.input_raw.chars().take(TOOL_OUTPUT_CHARS).collect();
        return ToolOutput::err(format!("{{\"INVALID_JSON\": {raw}}}"));
    }
    match call.name.as_str() {
        "read_file" => read_file(ctx, &call.input),
        "search" => search(ctx, &call.input),
        "fuzzy" => fuzzy(ctx, &call.input),
        "outline" => outline(ctx, &call.input),
        "git_diff" => git_diff(ctx, &call.input),
        "list_changes" => list_changes(ctx, &call.input),
        "read_blob" => read_blob(ctx, &call.input),
        other => ToolOutput::err(format!("unknown tool: {other}")),
    }
}

fn read_file(ctx: &ToolCtx, args: &serde_json::Value) -> ToolOutput {
    let path = match require_str(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let start = arg_usize(args, "start", 1).max(1);
    let count = arg_usize(args, "count", 200).clamp(1, 1000);
    match ctx.index.read_window(&path, start - 1, count) {
        Some(w) => {
            let body: Vec<String> = w
                .lines
                .iter()
                .map(|l| format!("{:>6}  {}", l.n, l.text))
                .collect();
            ToolOutput::ok(format!("{} ({} lines)\n{}", path, w.total, body.join("\n")))
        }
        None => ToolOutput::err(format!("cannot read: {path}")),
    }
}

fn search(ctx: &ToolCtx, args: &serde_json::Value) -> ToolOutput {
    let pattern = match require_str(args, "q") {
        Ok(p) => p,
        Err(e) => return e,
    };
    if pattern.len() > 512 {
        return ToolOutput::err("q over 512 chars");
    }
    let mut q = scan::Query::literal(pattern);
    q.mode = match args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("literal")
    {
        "regex" => scan::Mode::Regex,
        "literal" => scan::Mode::Literal,
        other => return ToolOutput::err(format!("bad mode: {other}")),
    };
    q.case = match args.get("case").and_then(|v| v.as_str()).unwrap_or("smart") {
        "insensitive" => scan::Case::Insensitive,
        "sensitive" => scan::Case::Sensitive,
        "smart" => scan::Case::Smart,
        other => return ToolOutput::err(format!("bad case: {other}")),
    };
    q.word = arg_bool(args, "word");
    let split = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    };
    q.include = split("include");
    q.exclude = split("exclude");
    q.default_exclude = ctx.default_exclude.clone();
    q.max_files = arg_usize(args, "maxFiles", ctx.max_files).clamp(1, 1000);
    q.max_per_file = arg_usize(args, "maxPerFile", ctx.max_per_file).clamp(1, 100);
    q.max_file_bytes = ctx.max_file_bytes;
    let stop = std::sync::atomic::AtomicBool::new(false);
    match scan::search(&ctx.snapshot, ctx.index.root(), &q, &stop) {
        Ok(r) => {
            let mut out = format!(
                "engine={} scanned={} matched={}\n",
                r.engine, r.files_scanned, r.files_matched
            );
            for f in &r.files {
                for h in &f.hits {
                    out.push_str(&format!("{}:{} {}\n", f.path, h.line, h.text));
                }
                if f.more {
                    out.push_str(&format!("{}: (more hits capped)\n", f.path));
                }
            }
            if r.files.is_empty() {
                out.push_str("(no matches)");
            }
            ToolOutput::ok(out)
        }
        Err(e) => ToolOutput::err(format!("bad regex: {e}")),
    }
}

fn fuzzy(ctx: &ToolCtx, args: &serde_json::Value) -> ToolOutput {
    let q = match require_str(args, "q") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let limit = arg_usize(args, "limit", 20).clamp(1, 50);
    let boost: std::collections::HashSet<String> = args
        .get("boost")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .split(',')
        .take(20)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let hits = fuzzy::rank_snap(&ctx.snapshot, &q, limit, &boost);
    if hits.is_empty() {
        return ToolOutput::ok("(no matches)".into());
    }
    ToolOutput::ok(
        hits.iter()
            .map(|h| format!("{} ({})", ctx.snapshot.paths[h.index], h.score))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn outline(ctx: &ToolCtx, args: &serde_json::Value) -> ToolOutput {
    let path = match require_str(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let abs = match ctx.index.safe_join(&path) {
        Some(p) => p,
        None => return ToolOutput::err(format!("cannot read: {path}")),
    };
    let bytes = match std::fs::read(&abs) {
        Ok(b) => b,
        Err(_) => return ToolOutput::err(format!("cannot read: {path}")),
    };
    if bytes.len() > 8 * 1024 * 1024 {
        return ToolOutput::err("file over 8 MiB");
    }
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    let text = String::from_utf8_lossy(&bytes);
    let syms = ferro_core::outline::extract(&ext, &text);
    if syms.is_empty() {
        return ToolOutput::ok("(no symbols)".into());
    }
    ToolOutput::ok(
        syms.iter()
            .map(|(name, kind, line, depth)| {
                let pad = " ".repeat(depth * 2);
                format!("{line} {kind} {pad}{name}")
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn git_repo(ctx: &ToolCtx) -> Result<GitRepo, ToolOutput> {
    ctx.git_root
        .clone()
        .map(GitRepo::new)
        .ok_or_else(|| ToolOutput::err("not a git repository"))
}

fn git_diff(ctx: &ToolCtx, args: &serde_json::Value) -> ToolOutput {
    let repo = match git_repo(ctx) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let base = arg_str(args, "base").unwrap_or_else(|| "HEAD".into());
    let target = arg_str(args, "target").unwrap_or_else(|| "worktree".into());
    let context = arg_usize(args, "context", 3).clamp(0, 10);
    // Whole-tree diff when path is omitted.
    let path = arg_str(args, "path").unwrap_or_default();
    if path.is_empty() {
        return whole_diff(&repo, &base, &target, context);
    }
    match repo.diff_raw(&path, &base, &target, context, false) {
        Ok(d) => ToolOutput::ok(render_raw_diff(&d)),
        Err(e) => ToolOutput::err(e.stderr()),
    }
}

fn whole_diff(repo: &GitRepo, base: &str, target: &str, context: usize) -> ToolOutput {
    // Cheap path list first; per-file raw diffs stay small via output cap.
    let cs = match repo.changes(base, target) {
        Ok(c) => c,
        Err(e) => return ToolOutput::err(e.stderr()),
    };
    let mut out = format!(
        "base={} target={} files={} +{} -{}\n",
        cs.base_sha, cs.target, cs.stats.files, cs.stats.additions, cs.stats.deletions
    );
    for f in cs.files.iter().take(100) {
        out.push_str(&format!(
            "{} {} (+{}/-{})\n",
            f.status.0.as_str(),
            f.path,
            f.additions,
            f.deletions
        ));
        if out.len() > TOOL_OUTPUT_CHARS * 4 {
            break;
        }
    }
    let _ = context;
    ToolOutput::ok(out)
}

fn render_raw_diff(d: &ferro_core::diff::FileDiffRaw) -> String {
    use ferro_core::diff::RowKind;
    use ferro_core::git::ChangeStatus;
    let mut out = String::new();
    out.push_str(&format!(
        "status={} path={}\n",
        match d.status {
            ChangeStatus::Added => "A",
            ChangeStatus::Modified => "M",
            ChangeStatus::Deleted => "D",
            ChangeStatus::Renamed => "R",
            ChangeStatus::Copied => "C",
            ChangeStatus::Typechange => "T",
            ChangeStatus::Untracked => "?",
        },
        d.new_path
    ));
    if let Some(old) = d.old_path.as_deref() {
        out.push_str(&format!("old_path={old}\n"));
    }
    if d.binary {
        out.push_str("binary file differs\n");
        return out;
    }
    for h in &d.hunks {
        out.push_str(&h.header);
        out.push('\n');
        for r in &h.rows {
            let mark = match r.t {
                RowKind::Ctx => ' ',
                RowKind::Add => '+',
                RowKind::Del => '-',
            };
            out.push(mark);
            out.push_str(&r.text);
            out.push('\n');
        }
    }
    out
}

fn list_changes(ctx: &ToolCtx, args: &serde_json::Value) -> ToolOutput {
    let repo = match git_repo(ctx) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let base = arg_str(args, "base").unwrap_or_else(|| "HEAD".into());
    let target = arg_str(args, "target").unwrap_or_else(|| "worktree".into());
    match repo.changes(&base, &target) {
        Ok(cs) => {
            let mut out = format!(
                "base={} target={} files={} +{} -{}\n",
                cs.base_sha, cs.target, cs.stats.files, cs.stats.additions, cs.stats.deletions
            );
            for f in cs.files.iter().take(100) {
                out.push_str(&format!(
                    "{} {} (+{}/-{})\n",
                    f.status.0.as_str(),
                    f.path,
                    f.additions,
                    f.deletions
                ));
            }
            ToolOutput::ok(out)
        }
        Err(e) => ToolOutput::err(e.stderr()),
    }
}

fn read_blob(ctx: &ToolCtx, args: &serde_json::Value) -> ToolOutput {
    let repo = match git_repo(ctx) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let path = match require_str(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    let rev = arg_str(args, "rev").unwrap_or_else(|| "HEAD".into());
    let start = arg_usize(args, "start", 1).max(1);
    let count = arg_usize(args, "count", 200).clamp(1, 1000);
    // Validate the path lexically; the blob may not exist on disk.
    if let Err(e) =
        ferro_core::paths::resolve(ctx.index.root(), &path, ferro_core::paths::Access::Read)
    {
        return ToolOutput::err(format!("bad path: {e}"));
    }
    let bytes = match repo.blob_bytes(&rev, &path) {
        Ok(b) => b,
        Err(e) => return ToolOutput::err(e.stderr()),
    };
    if bytes.len() > 2 * 1024 * 1024 {
        return ToolOutput::err("blob over 2 MiB");
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return ToolOutput::err("binary blob");
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let body: Vec<String> = lines
        .iter()
        .enumerate()
        .skip(start - 1)
        .take(count)
        .map(|(i, l)| format!("{:>6}  {}", i + 1, l))
        .collect();
    ToolOutput::ok(format!(
        "{}@{} ({} lines)\n{}",
        path,
        rev,
        lines.len(),
        body.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(files: &[(&str, &str)]) -> (tempfile::TempDir, ToolCtx) {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for (p, content) in files {
            let full = dir.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, content).unwrap();
            paths.push(p.to_string());
        }
        paths.sort();
        let index = Arc::new(Index::with_dirs(
            dir.path().to_path_buf(),
            ferro_core::dirs::FerroDirs::new(
                dir.path().join("home/c"),
                dir.path().join("home/s"),
                dir.path().join("home/h"),
            ),
        ));
        let sizes = vec![0u64; paths.len()];
        let mtimes = vec![0i64; paths.len()];
        index.file_index.store(paths.clone(), sizes, mtimes);
        let ctx = ToolCtx::new(index);
        (dir, ctx)
    }

    fn call(name: &str, args: serde_json::Value) -> ToolCallV2 {
        ToolCallV2 {
            id: "t1".into(),
            name: name.into(),
            input: args.clone(),
            input_raw: args.to_string(),
            input_ok: true,
        }
    }

    #[test]
    fn tool_schemas_are_deterministic() {
        let schemas = tool_schemas();
        let names: Vec<&str> = schemas.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "read_file",
                "search",
                "fuzzy",
                "outline",
                "git_diff",
                "list_changes",
                "read_blob"
            ]
        );
        for t in tool_schemas() {
            assert!(t.input_schema.get("type").is_some(), "{}", t.name);
        }
    }

    #[test]
    fn read_search_fuzzy_outline_roundtrip() {
        let (_dir, ctx) = ctx_with(&[
            ("src/main.rs", "fn main() {\n    serve();\n}\n"),
            ("src/lib.rs", "pub fn serve() {}\n"),
        ]);
        let r = dispatch_v2(
            &ctx,
            &call(
                "read_file",
                serde_json::json!({"path": "src/main.rs", "count": 2}),
            ),
        );
        assert!(r.ok && r.output.contains("serve();"));
        let r = dispatch_v2(
            &ctx,
            &call("search", serde_json::json!({"q": "serve", "maxFiles": 10})),
        );
        assert!(r.ok && r.output.contains("src/main.rs:2"), "{}", r.output);
        let r = dispatch_v2(&ctx, &call("fuzzy", serde_json::json!({"q": "main"})));
        assert!(r.ok && r.output.contains("src/main.rs"), "{}", r.output);
        let r = dispatch_v2(
            &ctx,
            &call("outline", serde_json::json!({"path": "src/lib.rs"})),
        );
        assert!(r.ok && r.output.contains("serve"), "{}", r.output);
    }

    #[test]
    fn git_tools_need_a_repo_and_cap_output() {
        let (_dir, ctx) = ctx_with(&[("a.txt", "x\n")]);
        let r = dispatch_v2(&ctx, &call("list_changes", serde_json::json!({})));
        assert!(!r.ok);
        let big = "y\n".repeat(5000);
        let (_dir, ctx) = ctx_with(&[("big.txt", &big)]);
        let r = dispatch_v2(
            &ctx,
            &call(
                "read_file",
                serde_json::json!({"path": "big.txt", "count": 5000}),
            ),
        );
        assert!(r.ok && r.truncated && r.output.chars().count() <= TOOL_OUTPUT_CHARS);
        let r = dispatch_v2(&ctx, &call("nope", serde_json::json!({})));
        assert!(!r.ok);
    }
}
