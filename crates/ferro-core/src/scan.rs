//! Full-text scan engine (B2a): regex::bytes over the FileIndex snapshot,
//! rayon parallelism, mmap above 1 MiB, binary skip, glob include/exclude,
//! UTF-16 snippets and ranges, per-file and global caps with early stop.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::fileindex::FileSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Literal,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    Smart,
    Insensitive,
    Sensitive,
}

#[derive(Debug, Clone)]
pub struct Query {
    pub pattern: String,
    pub mode: Mode,
    pub case: Case,
    pub word: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Extra default excludes (search.exclude); skipped when `exclude` has `!default`.
    pub default_exclude: Vec<String>,
    pub max_files: usize,
    pub max_per_file: usize,
    pub max_file_bytes: u64,
}

impl Query {
    pub fn literal(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            mode: Mode::Literal,
            case: Case::Smart,
            word: false,
            include: vec![],
            exclude: vec![],
            default_exclude: vec!["**/vendor/**".into()],
            max_files: 200,
            max_per_file: 20,
            max_file_bytes: 8 * 1024 * 1024,
        }
    }

    fn effective_case(&self) -> bool {
        // Returns case_insensitive flag.
        match self.case {
            Case::Sensitive => false,
            Case::Insensitive => true,
            Case::Smart => !self.pattern.chars().any(|c| c.is_uppercase()),
        }
    }

    pub fn compile(&self) -> Result<regex::bytes::Regex, String> {
        let mut pat = if self.mode == Mode::Literal {
            regex::escape(&self.pattern)
        } else {
            self.pattern.clone()
        };
        if self.word {
            pat = format!(r"\b(?:{pat})\b");
        }
        let is_regex = self.mode == Mode::Regex;
        regex::bytes::RegexBuilder::new(&pat)
            .case_insensitive(self.effective_case())
            .build()
            .map_err(|e| {
                // Best-effort error position: first regex metacharacter, else 0.
                let pos = if is_regex {
                    self.pattern
                        .char_indices()
                        .find(|(_, c)| {
                            matches!(
                                c,
                                '(' | ')'
                                    | '['
                                    | ']'
                                    | '{'
                                    | '}'
                                    | '*'
                                    | '+'
                                    | '?'
                                    | '|'
                                    | '^'
                                    | '$'
                                    | '.'
                                    | '\\'
                            )
                        })
                        .map(|(i, _)| i)
                        .unwrap_or(0)
                } else {
                    0
                };
                format!("{e} at {pos}")
            })
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub line: usize,
    pub text: String,
    pub ranges: Vec<(usize, usize)>,
    pub cut_start: bool,
    pub cut_end: bool,
    pub def: bool,
}

#[derive(Debug, Clone)]
pub struct FileHits {
    pub path: String,
    pub hits: Vec<Hit>,
    pub more: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Excluded {
    pub globs: Vec<String>,
    pub files: usize,
}

#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub q: String,
    pub engine: &'static str,
    pub ms: u128,
    pub files_scanned: usize,
    pub files_matched: usize,
    pub truncated: bool,
    pub excluded: Excluded,
    pub files: Vec<FileHits>,
}

fn build_globs(patterns: &[String]) -> globset::GlobSet {
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        if p == "!default" {
            continue;
        }
        if let Ok(g) = globset::GlobBuilder::new(p).literal_separator(true).build() {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| globset::GlobSet::empty())
}

/// Identifier-declaration heuristic for the `def` flag.
fn looks_like_def(line: &str, query: &str) -> bool {
    let ql = query.to_lowercase();
    if ql.is_empty() || ql.len() > line.len() {
        return false;
    }
    let ll = line.to_lowercase();
    // `<q> (` `=` `:` `{` `[` or `keyword <q>`.
    let mut i = 0;
    while let Some(pos) = ll[i..].find(&ql) {
        let s = i + pos;
        let e = s + ql.len();
        let before_ok = s == 0 || !is_ident(ll.as_bytes()[s - 1]);
        let after = ll.as_bytes().get(e).copied().unwrap_or(b' ');
        if before_ok && (after == b'=' || after == b':' || after == b'{' || after == b'[') {
            return true;
        }
        if before_ok {
            // Declaration keyword immediately before the name.
            let head = ll[..s].trim_end();
            if head
                .split_whitespace()
                .last()
                .map(|w| {
                    matches!(
                        w,
                        "fn" | "def"
                            | "class"
                            | "func"
                            | "type"
                            | "let"
                            | "const"
                            | "var"
                            | "struct"
                            | "enum"
                            | "interface"
                            | "pub"
                            | "export"
                            | "static"
                    )
                })
                .unwrap_or(false)
            {
                return true;
            }
        }
        i = e.max(s + 1);
    }
    false
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// UTF-16 length of a str prefix.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Convert byte ranges of a match inside `line` (valid UTF-8, lossy) to UTF-16 ranges.
fn byte_ranges_to_utf16(line: &str, ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    // Prefix unit counts at every byte index (ASCII fast path inline).
    let mut units_at = vec![0usize; line.len() + 1];
    let mut u = 0usize;
    for (i, c) in line.char_indices() {
        while units_at.len() <= i {
            units_at.push(u);
        }
        units_at[i] = u;
        u += c.len_utf16();
    }
    units_at[line.len()] = u;
    ranges
        .iter()
        .map(|&(a, b)| {
            let a = a.min(line.len());
            let b = b.min(line.len());
            // Snap to char boundaries.
            let snap = |mut x: usize| {
                while x > 0 && !line.is_char_boundary(x) {
                    x -= 1;
                }
                x
            };
            (units_at[snap(a)], units_at[snap(b).max(snap(a))])
        })
        .collect()
}

/// Snippet: the line, or a ≤400-unit window around the first match.
fn snippet(line: &str, first: (usize, usize)) -> (String, Vec<(usize, usize)>, bool, bool) {
    const MAX: usize = 400;
    let total = utf16_len(line);
    if total <= MAX {
        return (line.to_string(), vec![first], false, false);
    }
    // Window around the first match start, snapped to char boundaries.
    let target = first.0.saturating_sub(80);
    let mut start_b = 0usize;
    let mut u = 0usize;
    for (i, c) in line.char_indices() {
        if u >= target {
            start_b = i;
            break;
        }
        u += c.len_utf16();
    }
    // Expand to MAX units.
    let mut end_b = line.len();
    let mut u2 = 0usize;
    for (i, c) in line[start_b..].char_indices() {
        if u2 >= MAX {
            end_b = start_b + i;
            break;
        }
        u2 += c.len_utf16();
    }
    let mut units = 0usize;
    for c in line[start_b..end_b].chars() {
        units += c.len_utf16();
    }
    let shift = {
        let mut s = 0usize;
        let mut uu = 0usize;
        for c in line[..start_b].chars() {
            uu += c.len_utf16();
        }
        s = uu;
        s
    };
    let _ = units;
    let ranges = vec![(first.0.saturating_sub(shift), first.1.saturating_sub(shift))];
    (
        line[start_b..end_b].to_string(),
        ranges,
        start_b > 0,
        end_b < line.len(),
    )
}

pub fn search(
    snap: &FileSnapshot,
    root: &Path,
    q: &Query,
    stop: &AtomicBool,
) -> Result<SearchResponse, String> {
    let t0 = std::time::Instant::now();
    let re = q.compile()?;
    let use_defaults = !q.exclude.iter().any(|g| g == "!default");
    let mut exclude_globs: Vec<String> = q
        .exclude
        .iter()
        .filter(|g| *g != "!default")
        .cloned()
        .collect();
    if use_defaults {
        exclude_globs.extend(q.default_exclude.clone());
    }
    let include_set = build_globs(&q.include);
    let exclude_set = build_globs(&exclude_globs);
    let has_include = !q.include.is_empty();

    // Candidate selection (no IO yet).
    let mut excluded_files = 0usize;
    let mut cands: Vec<usize> = Vec::new();
    for i in 0..snap.len() {
        let p = &snap.paths[i];
        if exclude_set.is_match(p) {
            excluded_files += 1;
            continue;
        }
        if has_include && !include_set.is_match(p) {
            continue;
        }
        cands.push(i);
    }

    let scanned = AtomicUsize::new(0);
    let matched = AtomicUsize::new(0);
    let hit_cap = AtomicBool::new(false);
    let per_file = q.max_per_file.clamp(1, 1000);
    let max_files = q.max_files.max(1);
    let max_bytes = q.max_file_bytes;
    let root = root.to_path_buf();
    let is_ident_query =
        q.pattern.chars().all(|c| c.is_alphanumeric() || c == '_') && !q.pattern.is_empty();

    let scan_one = |i: usize| -> Option<FileHits> {
        if stop.load(Ordering::Relaxed) || hit_cap.load(Ordering::Relaxed) {
            return None;
        }
        scanned.fetch_add(1, Ordering::Relaxed);
        let rel = &snap.paths[i];
        let full = root.join(rel);
        let bytes: Vec<u8> = if snap.sizes.get(i).copied().unwrap_or(0) > 1024 * 1024 {
            // mmap above 1 MiB.
            let f = std::fs::File::open(&full).ok()?;
            let m = unsafe { memmap2::Mmap::map(&f).ok()? };
            if m.len() as u64 > max_bytes {
                return None;
            }
            if m[..m.len().min(8192)].contains(&0) {
                return None;
            }
            m.to_vec()
        } else {
            let b = std::fs::read(&full).ok()?;
            if b.len() as u64 > max_bytes {
                return None;
            }
            if b[..b.len().min(8192)].contains(&0) {
                return None;
            }
            b
        };
        // Line starts via memchr (no per-line splitting).
        let mut starts = vec![0usize];
        starts.extend(memchr::memchr_iter(b'\n', &bytes).map(|p| p + 1));
        let mut hits = Vec::new();
        let mut more = false;
        for m in re.find_iter(&bytes) {
            let (ms, me) = (m.start(), m.end());
            let line_no = starts.partition_point(|&s| s <= ms);
            let ls = starts[line_no - 1];
            let mut line_end = starts.get(line_no).copied().unwrap_or(bytes.len());
            if line_end > ls && bytes[line_end - 1] == b'\n' {
                line_end -= 1;
            }
            if line_end > ls && bytes[line_end - 1] == b'\r' {
                line_end -= 1;
            }
            if hits.len() >= per_file {
                more = true;
                break;
            }
            let line = String::from_utf8_lossy(&bytes[ls..line_end]).into_owned();
            let ranges16 = byte_ranges_to_utf16(&line, &[(ms - ls, me - ls)]);
            let first = ranges16.first().copied().unwrap_or((0, 0));
            let (text, ranges, cut_start, cut_end) = snippet(&line, first);
            // Recompute ranges against the snippet when it was cut.
            let ranges = if cut_start || cut_end {
                ranges
            } else {
                ranges16
            };
            hits.push(Hit {
                line: line_no,
                text,
                ranges,
                cut_start,
                cut_end,
                def: is_ident_query && looks_like_def(&line, &q.pattern),
            });
        }
        if hits.is_empty() {
            return None;
        }
        let n = matched.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= max_files {
            hit_cap.store(true, Ordering::Relaxed);
        }
        Some(FileHits {
            path: rel.clone(),
            hits,
            more,
        })
    };

    let mut files: Vec<FileHits> = if cands.len() > 64 {
        use rayon::prelude::*;
        cands.par_iter().filter_map(|&i| scan_one(i)).collect()
    } else {
        cands.iter().filter_map(|&i| scan_one(i)).collect()
    };
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let truncated = hit_cap.load(Ordering::Relaxed);
    if files.len() > max_files {
        files.truncate(max_files);
    }
    Ok(SearchResponse {
        q: q.pattern.clone(),
        engine: "scan",
        ms: t0.elapsed().as_millis(),
        files_scanned: scanned.load(Ordering::Relaxed),
        files_matched: matched.load(Ordering::Relaxed),
        truncated,
        excluded: Excluded {
            globs: exclude_globs,
            files: excluded_files,
        },
        files,
    })
}

pub fn find_in_file(
    root: &Path,
    rel: &str,
    q: &Query,
    limit: usize,
) -> Result<FindResponse, String> {
    let re = q.compile()?;
    let full = root.join(rel.trim_start_matches('/'));
    let bytes = std::fs::read(&full).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let mut matches = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    // Byte offsets → line numbers via memchr.
    let mut starts = vec![0usize];
    {
        use memchr::memchr_iter;
        for pos in memchr_iter(b'\n', &bytes) {
            starts.push(pos + 1);
        }
    }
    for m in re.find_iter(bytes.as_slice()) {
        total += 1;
        if matches.len() >= limit {
            truncated = true;
            continue;
        }
        let line_no = starts.partition_point(|&s| s <= m.start());
        let ls = starts[line_no - 1];
        let mut le = starts.get(line_no).copied().unwrap_or(bytes.len());
        if le > ls && bytes[le - 1] == b'\n' {
            le -= 1;
        }
        let line = &text[ls.min(text.len())..le.min(text.len())];
        matches.push(FindMatch {
            line: line_no,
            ranges: byte_ranges_to_utf16(line, &[(m.start() - ls, m.end() - ls)]),
        });
    }
    Ok(FindResponse {
        total,
        truncated,
        matches,
    })
}

#[derive(Debug, Clone)]
pub struct FindResponse {
    pub total: usize,
    pub truncated: bool,
    pub matches: Vec<FindMatch>,
}

#[derive(Debug, Clone)]
pub struct FindMatch {
    pub line: usize,
    pub ranges: Vec<(usize, usize)>,
}

/// Resolve `path`, `path:line`, `path:line:col`, `path#L10[-L20]`, bare basename.
pub fn resolve_candidates(
    snap: &FileSnapshot,
    candidates: &[String],
) -> Vec<(String, Option<ResolvedRef>)> {
    candidates
        .iter()
        .map(|c| (c.clone(), resolve_one(snap, c)))
        .collect()
}

#[derive(Debug, Clone)]
pub struct ResolvedRef {
    pub path: String,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub col: Option<usize>,
}

fn resolve_one(snap: &FileSnapshot, c: &str) -> Option<ResolvedRef> {
    let c = c.trim();
    if c.is_empty() || c.len() > 512 {
        return None;
    }
    // path#L10 / path#L10-L20
    if let Some((p, frag)) = c.split_once('#') {
        if p.is_empty() {
            return None;
        }
        let (line, end_line) = parse_line_frag(frag)?;
        let path = match_exact_or_unique(snap, p)?;
        return Some(ResolvedRef {
            path,
            line: Some(line),
            end_line,
            col: None,
        });
    }
    // path:line[:col] — line is second-to-last when both ends are numeric.
    let segs: Vec<&str> = c.split(':').collect();
    if segs.len() >= 2 {
        if let Ok(line) = segs[segs.len() - 1].parse::<usize>() {
            if line > 0 {
                // Three-part numeric tail means path:line:col.
                if segs.len() >= 3 {
                    if let (Ok(ln), Ok(col)) = (
                        segs[segs.len() - 2].parse::<usize>(),
                        segs[segs.len() - 1].parse::<usize>(),
                    ) {
                        if ln > 0 {
                            let p = segs[..segs.len() - 2].join(":");
                            if !p.is_empty() {
                                if let Some(path) = match_exact_or_unique(snap, &p) {
                                    return Some(ResolvedRef {
                                        path,
                                        line: Some(ln),
                                        end_line: None,
                                        col: Some(col),
                                    });
                                }
                            }
                        }
                    }
                }
                let p = segs[..segs.len() - 1].join(":");
                if !p.is_empty() {
                    if let Some(path) = match_exact_or_unique(snap, &p) {
                        return Some(ResolvedRef {
                            path,
                            line: Some(line),
                            end_line: None,
                            col: None,
                        });
                    }
                }
            }
        }
    }
    match_exact_or_unique(snap, c).map(|path| ResolvedRef {
        path,
        line: None,
        end_line: None,
        col: None,
    })
}

fn parse_line_frag(frag: &str) -> Option<(usize, Option<usize>)> {
    let f = frag.strip_prefix('L').unwrap_or(frag);
    if let Some((a, b)) = f.split_once("-L").or_else(|| f.split_once('-')) {
        let a = a.parse::<usize>().ok()?;
        let b = b.trim_start_matches('L').parse::<usize>().ok()?;
        if a > 0 && b >= a {
            return Some((a, Some(b)));
        }
        return None;
    }
    let n: usize = f.parse().ok()?;
    (n > 0).then_some((n, None))
}

fn match_exact_or_unique(snap: &FileSnapshot, p: &str) -> Option<String> {
    if snap.paths.iter().any(|x| x == p) {
        return Some(p.to_string());
    }
    // Suffix match (foo/bar.rs) unique only.
    let mut hits: Vec<&String> = snap
        .paths
        .iter()
        .filter(|x| x.as_str() == p || x.ends_with(&format!("/{p}")))
        .collect();
    // Bare basename: unique across all basenames.
    if hits.is_empty() && !p.contains('/') {
        hits = snap
            .paths
            .iter()
            .filter(|x| x.rsplit('/').next() == Some(p))
            .collect();
    }
    if hits.len() == 1 {
        Some(hits[0].clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_for(dir: &std::path::Path, files: &[(&str, &str)]) -> FileSnapshot {
        for (p, content) in files {
            let full = dir.join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, content).unwrap();
        }
        let mut paths: Vec<String> = files.iter().map(|(p, _)| p.to_string()).collect();
        paths.sort();
        let sizes = vec![0u64; paths.len()];
        let mtimes = vec![0i64; paths.len()];
        FileSnapshot {
            lower: paths.iter().map(|p| p.to_lowercase()).collect(),
            base_off: paths
                .iter()
                .map(|p| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
                .collect(),
            sizes,
            mtimes,
            generation: 0,
            paths,
        }
    }

    #[test]
    fn literal_case_modes_and_word() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.rs", "Foo bar\nfoo BAR\nfoobar\n")]);
        let stop = AtomicBool::new(false);
        let mut q = Query::literal("foo");
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        assert_eq!(r.files[0].hits.len(), 3); // smart: lowercase → insensitive
        q.case = Case::Sensitive;
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        assert_eq!(r.files[0].hits.len(), 2);
        q.case = Case::Smart;
        q.word = true;
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        // "foo" whole-word: lines 1 ("Foo") and 2 ("foo"), not "foobar"
        assert_eq!(r.files[0].hits.len(), 2);
        q.pattern = "Foo".into();
        q.word = false;
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        assert_eq!(r.files[0].hits.len(), 1); // smart: uppercase → sensitive
    }

    #[test]
    fn invalid_regex_position() {
        let q = Query {
            pattern: "a(b".into(),
            mode: Mode::Regex,
            ..Query::literal("")
        };
        assert!(q.compile().is_err());
    }

    #[test]
    fn globs_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(
            dir.path(),
            &[("v/a.rs", "x = 1\n"), ("src/b.rs", "x = 1\n")],
        );
        let stop = AtomicBool::new(false);
        let mut q = Query::literal("x = 1");
        q.exclude = vec!["v/**".into()];
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].path, "src/b.rs");
        assert_eq!(r.excluded.files, 1);
        let mut q = Query::literal("x = 1");
        q.max_per_file = 1;
        let _ = q;
        // per-file cap flag
        let dir2 = tempfile::tempdir().unwrap();
        let snap2 = snap_for(dir2.path(), &[("c.rs", "y\ny\ny\n")]);
        let mut q2 = Query::literal("y");
        q2.max_per_file = 2;
        let r2 = search(&snap2, dir2.path(), &q2, &stop).unwrap();
        assert!(r2.files[0].more);
        assert_eq!(r2.files[0].hits.len(), 2);
    }

    #[test]
    fn binary_and_size_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.bin"), [0u8, 1, 2, 3]).unwrap();
        let snap = snap_for(dir.path(), &[("b.bin", "x")]);
        // rewrite with real binary content
        std::fs::write(dir.path().join("b.bin"), [0u8, 1, 2, 3]).unwrap();
        let stop = AtomicBool::new(false);
        let q = Query::literal("x");
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        assert!(r.files.is_empty());
        assert_eq!(r.files_scanned, 1);
    }

    #[test]
    fn resolve_forms() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("src/main.rs", "x"), ("src/other.rs", "y")]);
        let out = resolve_candidates(
            &snap,
            &[
                "src/main.rs:10".into(),
                "main.rs".into(),
                "src/main.rs#L3-L5".into(),
                "main".into(),
                "other.rs:2:7".into(),
            ],
        );
        assert_eq!(out[0].1.as_ref().unwrap().line, Some(10));
        assert_eq!(out[1].1.as_ref().unwrap().path, "src/main.rs");
        assert_eq!(out[2].1.as_ref().unwrap().end_line, Some(5));
        assert!(out[3].1.is_none());
        assert_eq!(out[4].1.as_ref().unwrap().col, Some(7));
    }

    #[test]
    fn def_flag() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.rs", "fn foo() {}\nfoo();\n")]);
        let stop = AtomicBool::new(false);
        let q = Query::literal("foo");
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        assert!(r.files[0].hits[0].def);
        assert!(!r.files[0].hits[1].def);
    }
}
