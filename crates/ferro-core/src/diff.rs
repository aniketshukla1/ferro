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
    pub fn diff_raw(
        &self,
        path: &str,
        base: &str,
        target: &str,
        context: usize,
        ignore_ws: bool,
    ) -> Result<FileDiffRaw, GitError> {
        // Resolve through § 5.1 first (read access is enough; git reads it).
        let root_canon = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let rel = crate::paths::resolve(&root_canon, path, crate::paths::Access::Read)
            .map_err(|_| GitError::Forbidden("path escapes root".into()))?;
        let rel = rel
            .strip_prefix(&root_canon)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string());
        let base_sha = self.resolve_base(base)?;
        // Rename pairing needs both sides visible: look up the rename source
        // in the relevant letter maps BEFORE running the pathspec-limited diff.
        let (staged_letters, unstaged_letters, range_letters) = if target == "worktree" {
            (
                self.name_status(&[&base_sha, "--cached"])
                    .unwrap_or_default(),
                self.name_status(&[&base_sha]).unwrap_or_default(),
                None,
            )
        } else if target == "index" {
            (
                self.name_status(&[&base_sha, "--cached"])
                    .unwrap_or_default(),
                std::collections::BTreeMap::new(),
                None,
            )
        } else {
            let rev = self
                .run(&["rev-parse", target])
                .map(|s| s.trim().to_string())?;
            let range = format!("{base_sha}..{rev}");
            (
                std::collections::BTreeMap::new(),
                std::collections::BTreeMap::new(),
                Some(self.name_status(&[&range]).unwrap_or_default()),
            )
        };
        let rename_orig = staged_letters
            .get(&rel)
            .or_else(|| unstaged_letters.get(&rel))
            .or_else(|| range_letters.as_ref().and_then(|m| m.get(&rel)))
            .filter(|(st, _)| *st == ChangeStatus::Renamed)
            .and_then(|(_, o)| o.clone());
        let is_untracked = if target == "worktree" {
            self.status_v2()
                .map(|st| st.files.iter().any(|f| f.path == rel && f.untracked))
                .unwrap_or(false)
        } else {
            false
        };
        let ctx = context.clamp(0, 50).to_string();
        let mut args: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff", "-M"];
        let ctx_arg = format!("-U{ctx}");
        args.push(&ctx_arg);
        if ignore_ws {
            args.push("-w");
        }
        let rev_target;
        if is_untracked {
            // Untracked files diff against an empty base.
            let abs = self.root.join(&rel);
            let out = self.run_diff_bytes(&[
                "diff",
                "--no-index",
                "--no-color",
                "--no-ext-diff",
                &ctx_arg,
                "/dev/null",
                &abs.to_string_lossy(),
            ])?;
            return Ok(parse_diff(&out, ChangeStatus::Added));
        } else if target == "worktree" {
            args.push(&base_sha);
        } else if target == "index" {
            args.push("--cached");
            args.push(&base_sha);
        } else {
            rev_target = self
                .run(&["rev-parse", target])
                .map(|s| s.trim().to_string())?;
            args.push(&base_sha);
            args.push(&rev_target);
        }
        args.push("--");
        if let Some(orig) = &rename_orig {
            args.push(orig);
        }
        args.push(&rel);
        // borrowck: base_sha/rev_target/rename_orig outlive args.
        let out = self.run_diff_bytes(&args)?;
        let mut d = parse_diff(&out, ChangeStatus::Modified);
        // Authoritative letters + rename source from the precomputed maps.
        if let Some((st, old)) = staged_letters
            .get(&rel)
            .or_else(|| unstaged_letters.get(&rel))
            .or_else(|| range_letters.as_ref().and_then(|m| m.get(&rel)))
        {
            d.status = *st;
            if d.old_path.is_none() {
                d.old_path = old.clone();
            }
        } else if d.hunks.is_empty() && !d.binary {
            // No diff output for this path: unchanged vs the base.
            d.status = ChangeStatus::Modified;
        }
        Ok(d)
    }

    /// Full side content for highlighting: worktree file, index blob, rev
    /// blob, or empty. Callers cap highlighting at 2 MiB per side.
    pub fn side_bytes(&self, path: &str, side: &DiffSide) -> Vec<u8> {
        match side {
            DiffSide::Empty => Vec::new(),
            DiffSide::Worktree => std::fs::read(self.root.join(path)).unwrap_or_default(),
            DiffSide::Index => self
                .run_bytes(&["show", &format!(":{path}")])
                .unwrap_or_default(),
            DiffSide::Rev(rev) => self
                .run_bytes(&["show", &format!("{rev}:{path}")])
                .unwrap_or_default(),
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
}
