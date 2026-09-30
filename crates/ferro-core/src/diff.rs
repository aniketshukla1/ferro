//! Unified-diff parser and side-content helpers (B3).
//! Parsing covers renames, binary, mode changes, no-EOL markers, CRLF
//! content, and C-style quoted paths. Presentation (highlighting,
//! intraline ranges) lives in the server layer, which owns the class map.

use super::git::{ChangeStatus, GitError, GitRepo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Ctx,
    Add,
    Del,
}

#[derive(Debug, Clone)]
pub struct RawRow {
    pub t: RowKind,
    /// 1-based old-side line (ctx, del).
    pub o: Option<usize>,
    /// 1-based new-side line (ctx, add).
    pub n: Option<usize>,
    pub text: String,
    pub no_eol: bool,
}

#[derive(Debug, Clone)]
pub struct RawHunk {
    pub header: String,
    pub section: Option<String>,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub rows: Vec<RawRow>,
}

impl RawHunk {
    pub fn id(&self, path: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        path.hash(&mut h);
        self.old_start.hash(&mut h);
        self.new_start.hash(&mut h);
        self.header.hash(&mut h);
        format!("{:x}", h.finish())
    }
}

#[derive(Debug, Clone)]
pub struct FileDiffRaw {
    pub status: ChangeStatus,
    pub old_path: Option<String>,
    pub new_path: String,
    pub binary: bool,
    pub old_blob: Option<String>,
    pub new_blob: Option<String>,
    pub hunks: Vec<RawHunk>,
}

#[derive(Debug, Clone)]
pub enum DiffSide {
    Worktree,
    Index,
    Rev(String),
    Empty,
}

impl GitRepo {
    /// Raw unified diff for one path between base and target.
    /// `target`: `worktree` | `index` | any rev. Deleted files, renames and
    /// untracked files (diffed against an empty base) are all handled.
    ///
    /// Hot path (modified file, plain base) costs one git spawn: `base` is
    /// passed through unresolved unless it is a `merge-base` form, and
    /// rename pairing consults full status only for pure add/delete results.
    pub fn diff_raw(
        &self,
        path: &str,
        base: &str,
        target: &str,
        context: usize,
        ignore_ws: bool,
    ) -> Result<FileDiffRaw, GitError> {
        // § 5.1, lexically: git diffs a symlink as a link, never its target.
        let rel = crate::paths::git_rel(&self.root, path, crate::paths::Access::Read)
            .map_err(|_| GitError::Forbidden("path escapes root".into()))?;
        // Base passes straight to git unless it is a merge-base form; either
        // way nothing option-shaped reaches argv.
        let mut base_owned = if base == "merge-base" || base.starts_with("merge-base:") {
            self.resolve_base(base)?
        } else {
            super::git::check_rev(base)?;
            base.to_string()
        };
        let rev_target = match target {
            "worktree" | "index" => None,
            rev => Some(self.rev_parse(rev)?),
        };
        let ctx_arg = format!("-U{}", context.clamp(0, 50));
        // Fixed a/ b/ prefixes: `diff.noprefix`, `diff.mnemonicPrefix` or
        // `diff.srcPrefix` in the user's config must not change the output
        // the parser reads.
        let build = |base_arg: &str| -> Vec<String> {
            let mut args: Vec<String> = [
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "-M",
            ]
            .map(String::from)
            .to_vec();
            args.push(ctx_arg.clone());
            if ignore_ws {
                args.push("-w".into());
            }
            if target == "index" {
                args.push("--cached".into());
            }
            args.push(base_arg.to_string());
            if let Some(t) = &rev_target {
                args.push(t.clone());
            }
            args.push("--".into());
            args.push(rel.clone());
            args
        };
        let run = |args: &[String]| {
            self.run_diff_bytes(&args.iter().map(String::as_str).collect::<Vec<_>>())
        };
        let out = match run(&build(&base_owned)) {
            Ok(out) => out,
            // A fresh `git init` has no HEAD yet: diff against the empty tree.
            Err(GitError::Failed { .. }) if base_owned == "HEAD" && self.is_unborn() => {
                base_owned = self.resolve_base("HEAD")?;
                run(&build(&base_owned))?
            }
            Err(e) => return Err(e),
        };
        let base_arg = base_owned.as_str();
        let d = parse_diff(&out, ChangeStatus::Modified);
        if d.hunks.is_empty() && !d.binary {
            // Empty diff: unchanged, or untracked (worktree only), or unknown.
            let abs = self.root.join(&rel);
            let on_disk = std::fs::symlink_metadata(&abs)
                .is_ok_and(|m| m.is_file() || m.file_type().is_symlink());
            if target == "worktree"
                && on_disk
                && self
                    .run(&["ls-files", "--error-unmatch", "--", &rel])
                    .is_err()
            {
                let out = self.run_diff_bytes(&[
                    "diff",
                    "--no-index",
                    "--no-color",
                    "--no-ext-diff",
                    &ctx_arg,
                    "/dev/null",
                    &abs.to_string_lossy(),
                ])?;
                // `--no-index` headers carry the absolute path; the file is
                // the workspace-relative one that was asked for.
                let mut d = parse_diff(&out, ChangeStatus::Added);
                d.new_path = rel;
                d.old_path = None;
                d.status = ChangeStatus::Added;
                return Ok(d);
            }
            return Ok(d);
        }
        // Pure add/delete: a staged rename may hide behind the pathspec
        // (git cannot pair across it). One full status consults the R map.
        let all_rows: Vec<_> = d.hunks.iter().flat_map(|h| &h.rows).collect();
        let pure_add = !all_rows.is_empty() && all_rows.iter().all(|r| r.t == RowKind::Add);
        let pure_del = !all_rows.is_empty() && all_rows.iter().all(|r| r.t == RowKind::Del);
        if (pure_add || pure_del) && (target == "worktree" || target == "index") {
            if let Ok(st) = self.status_v2() {
                let hit = st.files.iter().find(|f| {
                    f.path == rel
                        && f.orig_path.is_some()
                        && (f.index == Some("R") || f.worktree == Some("R"))
                });
                if let Some(orig) = hit.and_then(|f| f.orig_path.clone()) {
                    let mut args2: Vec<&str> = vec![
                        "diff",
                        "--no-color",
                        "--no-ext-diff",
                        "--src-prefix=a/",
                        "--dst-prefix=b/",
                        "-M",
                        &ctx_arg,
                    ];
                    if ignore_ws {
                        args2.push("-w");
                    }
                    if target == "index" {
                        args2.push("--cached");
                    }
                    args2.push(base_arg);
                    args2.push("--");
                    args2.push(&orig);
                    args2.push(&rel);
                    if let Ok(out2) = self.run_diff_bytes(&args2) {
                        let d2 = parse_diff(&out2, ChangeStatus::Renamed);
                        if d2.status == ChangeStatus::Renamed {
                            return Ok(d2);
                        }
                    }
                }
            }
        }
        Ok(d)
    }

    /// Full side content for highlighting: worktree file, index blob, rev
    /// blob, or empty. Sides over `max` bytes fail with
    /// [`GitError::TooLarge`] before they are read. A worktree symlink yields
    /// its target path, which is what git diffs, never the target's content.
    pub fn side_bytes(&self, path: &str, side: &DiffSide, max: u64) -> Result<Vec<u8>, GitError> {
        let rel = crate::paths::git_rel(&self.root, path, crate::paths::Access::Read)
            .map_err(|_| GitError::Forbidden("path escapes root".into()))?;
        match side {
            DiffSide::Empty => Ok(Vec::new()),
            DiffSide::Worktree => {
                let full = self.root.join(&rel);
                let md =
                    std::fs::symlink_metadata(&full).map_err(|e| GitError::Io(e.to_string()))?;
                if md.file_type().is_symlink() {
                    let target =
                        std::fs::read_link(&full).map_err(|e| GitError::Io(e.to_string()))?;
                    return Ok(target.to_string_lossy().into_owned().into_bytes());
                }
                if !md.is_file() {
                    return Err(GitError::Io(format!("not a file: {rel}")));
                }
                if md.len() > max {
                    return Err(GitError::TooLarge);
                }
                std::fs::read(&full).map_err(|e| GitError::Io(e.to_string()))
            }
            DiffSide::Index => self.blob_bytes_max("", &rel, max),
            DiffSide::Rev(rev) => self.blob_bytes_max(rev, &rel, max),
        }
    }
}

/// Parse `git diff` output (one file) into hunks and rows.
pub fn parse_diff(out: &[u8], default_status: ChangeStatus) -> FileDiffRaw {
    let text = String::from_utf8_lossy(out);
    let mut d = FileDiffRaw {
        status: default_status,
        old_path: None,
        new_path: String::new(),
        binary: false,
        old_blob: None,
        new_blob: None,
        hunks: Vec::new(),
    };
    let mut cur: Option<RawHunk> = None;
    let mut o_line = 0usize;
    let mut n_line = 0usize;
    let mut last_row: Option<(usize, usize)> = None; // (hunk idx, row idx) for no-EOL
                                                     // Track ---/+++ to learn paths and added/deleted shape.
    let mut minus_path: Option<String> = None;
    let mut plus_path: Option<String> = None;

    let flush = |cur: &mut Option<RawHunk>, d: &mut FileDiffRaw| {
        if let Some(h) = cur.take() {
            d.hunks.push(h);
        }
    };
    for raw_line in text.split('\n') {
        // Content lines may carry \r (CRLF files) — kept verbatim.
        if let Some(h) = cur.as_mut() {
            if raw_line.starts_with("@@ ") {
                flush(&mut cur, &mut d);
            } else if let Some(body) = raw_line.strip_prefix(' ') {
                o_line += 1;
                n_line += 1;
                h.rows.push(RawRow {
                    t: RowKind::Ctx,
                    o: Some(o_line),
                    n: Some(n_line),
                    text: body.to_string(),
                    no_eol: false,
                });
                last_row = Some((d.hunks.len(), h.rows.len() - 1));
                continue;
            } else if let Some(body) = raw_line.strip_prefix('-') {
                // `--- a/path` also starts with '-': disambiguate — inside a
                // hunk body a minus-path line never appears.
                o_line += 1;
                h.rows.push(RawRow {
                    t: RowKind::Del,
                    o: Some(o_line),
                    n: None,
                    text: body.to_string(),
                    no_eol: false,
                });
                last_row = Some((d.hunks.len(), h.rows.len() - 1));
                continue;
            } else if let Some(body) = raw_line.strip_prefix('+') {
                n_line += 1;
                h.rows.push(RawRow {
                    t: RowKind::Add,
                    o: None,
                    n: Some(n_line),
                    text: body.to_string(),
                    no_eol: false,
                });
                last_row = Some((d.hunks.len(), h.rows.len() - 1));
                continue;
            } else if raw_line.starts_with("\\ No newline") {
                if let Some((hi, ri)) = last_row {
                    if hi < d.hunks.len() {
                        if let Some(r) = d.hunks[hi].rows.get_mut(ri) {
                            r.no_eol = true;
                        }
                    } else if let Some(r) = h.rows.last_mut() {
                        r.no_eol = true;
                    }
                } else if let Some(r) = h.rows.last_mut() {
                    r.no_eol = true;
                }
                continue;
            } else if raw_line.is_empty() {
                // Trailing empty split; a truly empty context line would be " ".
                continue;
            } else {
                // New file header (another `diff --git`) — end this hunk.
                flush(&mut cur, &mut d);
            }
        }
        if raw_line.starts_with("@@ ") {
            if let Some(h) = parse_hunk_header(raw_line) {
                o_line = h.old_start.saturating_sub(1);
                // Hunks with 0 lines (pure add/delete at an edge) start at
                // the line *before*; rows still count up from there.
                n_line = h.new_start.saturating_sub(1);
                last_row = None;
                cur = Some(h);
            }
        } else if let Some(rest) = raw_line.strip_prefix("diff --git ") {
            let _ = rest;
        } else if let Some(rest) = raw_line.strip_prefix("old mode") {
            let _ = rest;
        } else if let Some(rest) = raw_line.strip_prefix("new mode") {
            let _ = rest;
        } else if let Some(rest) = raw_line.strip_prefix("new file mode") {
            let _ = rest;
            d.status = ChangeStatus::Added;
        } else if let Some(rest) = raw_line.strip_prefix("deleted file mode") {
            let _ = rest;
            d.status = ChangeStatus::Deleted;
        } else if let Some(rest) = raw_line.strip_prefix("similarity index ") {
            let _ = rest;
        } else if let Some(rest) = raw_line.strip_prefix("rename from ") {
            d.old_path = Some(unquote(rest));
            d.status = ChangeStatus::Renamed;
        } else if let Some(rest) = raw_line.strip_prefix("rename to ") {
            d.new_path = rest.trim().to_string();
            d.new_path = unquote(&d.new_path);
        } else if let Some(rest) = raw_line.strip_prefix("index ") {
            // `index <old>..<new> <mode>`
            let mut parts = rest.split_whitespace();
            if let Some(hashes) = parts.next() {
                let mut hs = hashes.split("..");
                d.old_blob = hs
                    .next()
                    .filter(|s| !s.is_empty() && *s != "0000000")
                    .map(|s| s.to_string());
                d.new_blob = hs
                    .next()
                    .filter(|s| !s.is_empty() && *s != "0000000")
                    .map(|s| s.to_string());
            }
        } else if raw_line.starts_with("Binary files ") {
            d.binary = true;
        } else if let Some(rest) = raw_line.strip_prefix("--- ") {
            minus_path = Some(strip_ab(unquote(rest.trim())));
        } else if let Some(rest) = raw_line.strip_prefix("+++ ") {
            plus_path = Some(strip_ab(unquote(rest.trim())));
        }
    }
    flush(&mut cur, &mut d);
    // Resolve display paths.
    if d.new_path.is_empty() {
        d.new_path = plus_path.or(minus_path.clone()).unwrap_or_default();
    }
    if d.old_path.is_none() {
        d.old_path = minus_path.filter(|p| p != "/dev/null" && *p != d.new_path);
    }
    if d.new_path == "/dev/null" {
        d.new_path = d.old_path.clone().unwrap_or_default();
        d.status = ChangeStatus::Deleted;
    }
    d
}

fn parse_hunk_header(line: &str) -> Option<RawHunk> {
    // `@@ -a[,b] +c[,d] @@ section`
    let mut parts = line.split("@@");
    parts.next()?;
    let ranges = parts.next()?.trim();
    let section = parts
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut it = ranges.split_whitespace();
    let old = it.next()?.strip_prefix('-')?;
    let new = it.next()?.strip_prefix('+')?;
    let (old_start, old_lines) = parse_range(old);
    let (new_start, new_lines) = parse_range(new);
    Some(RawHunk {
        header: line.to_string(),
        section,
        old_start,
        old_lines,
        new_start,
        new_lines,
        rows: Vec::new(),
    })
}

fn parse_range(s: &str) -> (usize, usize) {
    match s.split_once(',') {
        Some((a, b)) => (a.parse().unwrap_or(1), b.parse().unwrap_or(1)),
        // `-0,0` for added files is explicit; bare `-5` means one line.
        None => (s.parse().unwrap_or(1), 1),
    }
}

/// `a/path` → `path`; anything else verbatim.
fn strip_ab(p: String) -> String {
    if let Some(rest) = p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")) {
        // Only strip when it looks like git's prefix (paired ---/+++).
        return rest.to_string();
    }
    p
}

/// C-style unquote for git's quoted paths (`"pa\"th"`, `"\303\251"`).
/// Octal escapes are raw UTF-8 bytes, so decoding happens after unescaping.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return s.to_string();
    }
    let inner = &s.as_bytes()[1..s.len() - 1];
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        i += 1;
        match inner.get(i) {
            Some(b'n') => {
                out.push(b'\n');
                i += 1;
            }
            Some(b't') => {
                out.push(b'\t');
                i += 1;
            }
            Some(b'"') => {
                out.push(b'"');
                i += 1;
            }
            Some(b'\\') => {
                out.push(b'\\');
                i += 1;
            }
            Some(d @ b'0'..=b'7') => {
                let mut v = (d - b'0') as u32;
                for _ in 0..2 {
                    if let Some(h @ b'0'..=b'7') = inner.get(i + 1) {
                        v = v * 8 + (h - b'0') as u32;
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(v as u8);
                i += 1;
            }
            Some(other) => {
                out.push(b'\\');
                out.push(*other);
                i += 1;
            }
            None => out.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl GitRepo {
    /// Reverse-apply one worktree hunk of `path` against `base`.
    /// Lines outside that hunk stay byte-for-byte. The real git index is
    /// not updated: the file is rewritten in place.
    pub fn revert_hunk(&self, path: &str, hunk_id: &str, base: &str) -> Result<(), GitError> {
        if hunk_id.is_empty() || hunk_id.len() > 128 {
            return Err(no_such_hunk());
        }
        super::git::check_rev(base)?;
        let raw = self.diff_raw(path, base, "worktree", 3, false)?;
        if raw.binary {
            return Err(GitError::Failed {
                args: "hunk".into(),
                stderr: "binary file".into(),
            });
        }
        let display = if raw.new_path.is_empty() {
            path.to_string()
        } else {
            raw.new_path.clone()
        };
        let hunk = raw
            .hunks
            .iter()
            .find(|h| h.id(&display) == hunk_id || h.id(path) == hunk_id)
            .ok_or_else(no_such_hunk)?;
        let rel = crate::paths::git_rel(&self.root, &display, crate::paths::Access::Write)
            .map_err(|_| GitError::Forbidden("path escapes root".into()))?;
        let abs = self.root.join(&rel);
        // The final component is the symlink itself (§ 5.1). `std::fs::read`
        // and `OpenOptions::open` follow it and would create or truncate the
        // target outside the workspace.
        if std::fs::symlink_metadata(&abs).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(GitError::Forbidden("refusing to follow symlink".into()));
        }
        let bytes = match crate::paths::read_nofollow(&abs) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(symlink_or_io(&abs, e)),
        };
        let (lines, trailing) = split_file_lines(&bytes);
        let (rewritten, remove) = reverse_hunk_bytes(&lines, trailing, hunk);
        if remove {
            match std::fs::symlink_metadata(&abs) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(GitError::Forbidden("refusing to follow symlink".into()));
                }
                Ok(_) => {
                    std::fs::remove_file(&abs).map_err(|e| GitError::Io(e.to_string()))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(GitError::Io(e.to_string())),
            }
            return Ok(());
        }
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| GitError::Io(e.to_string()))?;
        }
        crate::paths::write_nofollow(&abs, &rewritten).map_err(|e| symlink_or_io(&abs, e))?;
        Ok(())
    }
}

fn symlink_or_io(path: &std::path::Path, err: std::io::Error) -> GitError {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        GitError::Forbidden("refusing to follow symlink".into())
    } else {
        GitError::Io(err.to_string())
    }
}

fn no_such_hunk() -> GitError {
    GitError::Failed {
        args: "hunk".into(),
        stderr: "no such hunk".into(),
    }
}

/// Split on `\n`, keeping any `\r`. `trailing` is whether the file ended
/// with a newline. Untouched lines are copied back unchanged.
fn split_file_lines(bytes: &[u8]) -> (Vec<Vec<u8>>, bool) {
    if bytes.is_empty() {
        return (Vec::new(), false);
    }
    let trailing = bytes.last() == Some(&b'\n');
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            lines.push(bytes[start..i].to_vec());
            start = i + 1;
        }
    }
    if !trailing {
        lines.push(bytes[start..].to_vec());
    }
    (lines, trailing)
}

fn join_file_lines(lines: &[Vec<u8>], trailing: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        out.extend_from_slice(line);
    }
    if trailing && !lines.is_empty() {
        out.push(b'\n');
    }
    out
}

/// Returns `(bytes, delete_file)`.
fn reverse_hunk_bytes(lines: &[Vec<u8>], trailing: bool, hunk: &RawHunk) -> (Vec<u8>, bool) {
    let start = hunk.new_start.saturating_sub(1).min(lines.len());
    let end = start.saturating_add(hunk.new_lines).min(lines.len());
    let mut old_side = Vec::new();
    let mut last_no_eol = false;
    for row in &hunk.rows {
        match row.t {
            RowKind::Add => {}
            RowKind::Ctx | RowKind::Del => {
                old_side.push(row.text.as_bytes().to_vec());
                last_no_eol = row.no_eol;
            }
        }
    }
    let mut out = Vec::with_capacity(lines.len());
    out.extend_from_slice(&lines[..start]);
    out.extend(old_side.iter().cloned());
    out.extend_from_slice(&lines[end..]);
    if out.is_empty() {
        return (Vec::new(), true);
    }
    let touches_eof = end >= lines.len();
    let trailing = if touches_eof { !last_no_eol } else { trailing };
    (join_file_lines(&out, trailing), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, GitRepo) {
        let dir = tempfile::tempdir().unwrap();
        let r = GitRepo::new(dir.path().to_path_buf());
        r.run(&["init", "-b", "main"]).unwrap();
        r.run(&["config", "user.email", "t@t"]).unwrap();
        r.run(&["config", "user.name", "t"]).unwrap();
        r.run(&["config", "commit.gpgsign", "false"]).unwrap();
        std::fs::write(dir.path().join("a.txt"), "l1\nl2\nl3\n").unwrap();
        r.run(&["add", "."]).unwrap();
        r.run(&["commit", "-m", "init"]).unwrap();
        (dir, r)
    }

    #[test]
    fn modify_and_untracked() {
        let (_dir, r) = fixture();
        std::fs::write(r.root.join("a.txt"), "l1\nL2\nl3\nl4\n").unwrap();
        let d = r.diff_raw("a.txt", "HEAD", "worktree", 3, false).unwrap();
        assert!(!d.binary);
        assert_eq!(d.hunks.len(), 1);
        let kinds: Vec<RowKind> = d.hunks[0].rows.iter().map(|x| x.t).collect();
        assert!(kinds.contains(&RowKind::Add) && kinds.contains(&RowKind::Del));
        assert!(!d.hunks[0].id("a.txt").is_empty());
        std::fs::write(r.root.join("new.txt"), "x\n").unwrap();
        let d = r.diff_raw("new.txt", "HEAD", "worktree", 3, false).unwrap();
        assert_eq!(d.status, ChangeStatus::Added);
        assert!(d
            .hunks
            .iter()
            .flat_map(|h| &h.rows)
            .all(|x| x.t == RowKind::Add));
    }

    #[test]
    fn rename_binary_noeol_crlf_quoted() {
        let (_dir, r) = fixture();
        // Rename.
        r.run(&["mv", "a.txt", "b.txt"]).unwrap();
        let d = r.diff_raw("b.txt", "HEAD", "worktree", 3, false).unwrap();
        assert_eq!(d.status, ChangeStatus::Renamed);
        assert_eq!(d.old_path.as_deref(), Some("a.txt"));
        r.run(&["mv", "b.txt", "a.txt"]).unwrap();
        // Binary.
        std::fs::write(r.root.join("img.bin"), [0u8, 1, 2, 3]).unwrap();
        r.run(&["add", "img.bin"]).unwrap();
        r.run(&["commit", "-m", "bin"]).unwrap();
        std::fs::write(r.root.join("img.bin"), [0u8, 9, 9]).unwrap();
        let d = r.diff_raw("img.bin", "HEAD", "worktree", 3, false).unwrap();
        assert!(d.binary);
        assert!(d.hunks.is_empty());
        // No-EOL + CRLF + quoted (space) path.
        std::fs::write(r.root.join("sp ace.txt"), "a\r\nb").unwrap();
        r.run(&["add", "sp ace.txt"]).unwrap();
        r.run(&["commit", "-m", "sp"]).unwrap();
        std::fs::write(r.root.join("sp ace.txt"), "a\r\nB").unwrap();
        let d = r
            .diff_raw("sp ace.txt", "HEAD", "worktree", 3, false)
            .unwrap();
        assert_eq!(d.new_path, "sp ace.txt");
        let rows = &d.hunks[0].rows;
        assert!(rows.iter().any(|x| x.text.ends_with('\r')));
        assert!(rows.last().map(|x| x.no_eol).unwrap_or(false));
    }

    #[test]
    fn unquote_cases() {
        assert_eq!(unquote("\"a\\\"b\""), "a\"b");
        assert_eq!(unquote("\"\\303\\251\""), "é");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(strip_ab("a/x".into()), "x");
    }

    #[test]
    fn deleted_modechange_unicode() {
        let (_dir, r) = fixture();
        // Deleted file.
        r.run(&["rm", "a.txt"]).unwrap();
        let d = r.diff_raw("a.txt", "HEAD", "worktree", 3, false).unwrap();
        assert_eq!(d.status, ChangeStatus::Deleted);
        assert!(d
            .hunks
            .iter()
            .flat_map(|h| &h.rows)
            .all(|x| x.t == RowKind::Del));
        r.run(&["checkout", "HEAD", "--", "a.txt"]).unwrap();
        // Mode change only (no content change).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(r.root.join("a.txt"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let d = r.diff_raw("a.txt", "HEAD", "worktree", 3, false).unwrap();
            assert!(d.hunks.is_empty() && !d.binary);
            std::fs::set_permissions(r.root.join("a.txt"), std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        // Unicode path.
        std::fs::write(r.root.join("café.txt"), "x\n").unwrap();
        r.run(&["add", "café.txt"]).unwrap();
        r.run(&["commit", "-m", "uni"]).unwrap();
        std::fs::write(r.root.join("café.txt"), "y\n").unwrap();
        let d = r
            .diff_raw("café.txt", "HEAD", "worktree", 3, false)
            .unwrap();
        assert_eq!(d.new_path, "café.txt");
        assert_eq!(d.hunks.len(), 1);
    }

    #[test]
    fn revert_one_hunk_leaves_the_rest() {
        let (dir, r) = fixture();
        let mut original = String::new();
        for i in 1..=30 {
            original.push_str(&format!("line{i}\n"));
        }
        std::fs::write(dir.path().join("a.txt"), &original).unwrap();
        r.run(&["add", "a.txt"]).unwrap();
        r.run(&["commit", "-m", "lines"]).unwrap();
        let mut edited = original.clone();
        edited = edited.replace("line2\n", "LINE2\n");
        edited = edited.replace("line20\n", "LINE20\n");
        std::fs::write(dir.path().join("a.txt"), &edited).unwrap();
        let d = r.diff_raw("a.txt", "HEAD", "worktree", 3, false).unwrap();
        assert_eq!(
            d.hunks.len(),
            2,
            "line 2 and line 20 must be separate hunks"
        );
        let index_before = std::fs::read(dir.path().join(".git/index")).unwrap();
        let hunk = d
            .hunks
            .iter()
            .find(|h| h.rows.iter().any(|row| row.text == "LINE2"))
            .expect("hunk containing LINE2");
        r.revert_hunk("a.txt", &hunk.id("a.txt"), "HEAD").unwrap();
        let after = std::fs::read(dir.path().join("a.txt")).unwrap();
        let mut expect = original;
        expect = expect.replace("line20\n", "LINE20\n");
        assert_eq!(after, expect.into_bytes());
        assert_eq!(
            std::fs::read(dir.path().join(".git/index")).unwrap(),
            index_before
        );
        let err = r.revert_hunk("a.txt", "no-such-hunk", "HEAD").unwrap_err();
        assert!(err.to_string().contains("no such hunk"));
    }

    /// A worktree symlink is the link itself (§ 5.1). Reverting a hunk must
    /// not open the target: `std::fs::read` / `OpenOptions::open` follow it,
    /// and a dangling link is created at the outside path.
    #[cfg(unix)]
    #[test]
    fn revert_hunk_does_not_follow_symlink() {
        let (dir, r) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let created = outside.path().join("created.txt");
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        std::os::unix::fs::symlink("old-target", dir.path().join("a.txt")).unwrap();
        r.run(&["add", "a.txt"]).unwrap();
        r.run(&["commit", "-m", "link"]).unwrap();
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        std::os::unix::fs::symlink(&created, dir.path().join("a.txt")).unwrap();
        let d = r.diff_raw("a.txt", "HEAD", "worktree", 3, false).unwrap();
        assert!(
            !d.binary && !d.hunks.is_empty(),
            "symlink retarget should be a text hunk"
        );
        let id = d.hunks[0].id("a.txt");
        let err = r.revert_hunk("a.txt", &id, "HEAD").unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "expected a symlink refusal, got {err}"
        );
        assert!(
            !created.exists(),
            "hunk revert created {}",
            created.display()
        );
        assert!(
            std::fs::symlink_metadata(dir.path().join("a.txt"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the in-repo symlink must stay a symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn revert_hunk_does_not_truncate_an_outside_target() {
        let (dir, r) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let created = outside.path().join("created.txt");
        std::fs::write(&created, "keep-me\n").unwrap();
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        std::os::unix::fs::symlink("old-target", dir.path().join("a.txt")).unwrap();
        r.run(&["add", "a.txt"]).unwrap();
        r.run(&["commit", "-m", "link"]).unwrap();
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        std::os::unix::fs::symlink(&created, dir.path().join("a.txt")).unwrap();
        let d = r.diff_raw("a.txt", "HEAD", "worktree", 3, false).unwrap();
        let id = d.hunks[0].id("a.txt");
        assert!(r.revert_hunk("a.txt", &id, "HEAD").is_err());
        assert_eq!(std::fs::read(&created).unwrap(), b"keep-me\n");
    }

    #[test]
    fn revert_hunk_rejects_dotdot_absolute_and_vcs() {
        let (dir, r) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("pwned.txt");
        let abs = target.to_string_lossy().to_string();
        for path in [
            "../pwned.txt",
            abs.as_str(),
            ".GIT/hooks/pwn",
            "a/../../pwned.txt",
        ] {
            assert!(
                r.revert_hunk(path, "h", "HEAD").is_err(),
                "{path} was accepted"
            );
        }
        assert!(!target.exists());
        assert!(!dir.path().join(".git/hooks/pwn").exists());
    }
}
