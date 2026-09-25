//! Full-text scan engine (B2a): regex::bytes over the FileIndex snapshot,
//! rayon parallelism, mmap above 1 MiB, binary skip, glob include/exclude,
//! UTF-16 snippets and ranges, per-file and global caps with early stop.

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
            Case::Smart => !has_literal_uppercase(&self.pattern, self.mode == Mode::Regex),
        }
    }

    pub fn compile(&self) -> Result<regex::bytes::Regex, String> {
        compile_query(self).map_err(|(msg, _)| msg)
    }
}

/// Smart case (rg parity): uppercase counts only when it is literal text. In a
/// regex, escapes (`\S`, `\W`, `\p{Lu}`, `\xFF`) and group names (`(?P<Name>`)
/// are syntax, not text.
fn has_literal_uppercase(pattern: &str, regex: bool) -> bool {
    if !regex {
        return pattern.chars().any(char::is_uppercase);
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                let esc = chars.get(i + 1).copied();
                i += 2;
                if matches!(esc, Some('p' | 'P' | 'x' | 'u' | 'U')) {
                    if chars.get(i) == Some(&'{') {
                        while i < chars.len() && chars[i] != '}' {
                            i += 1;
                        }
                        i += 1;
                    } else if matches!(esc, Some('x')) {
                        i += 2;
                    } else if matches!(esc, Some('p' | 'P')) {
                        i += 1;
                    }
                }
                continue;
            }
            '(' if chars.get(i + 1) == Some(&'?') => {
                // (?P<name>…) / (?<name>…): skip the group name.
                let mut j = i + 2;
                if chars.get(j) == Some(&'P') {
                    j += 1;
                }
                if chars.get(j) == Some(&'<') {
                    while j < chars.len() && chars[j] != '>' {
                        j += 1;
                    }
                    i = j + 1;
                    continue;
                }
                i += 2;
                continue;
            }
            c if c.is_uppercase() => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Compile a query, returning the message plus a best-effort error position
/// for `detail.position` (API.md § 5.2).
pub fn compile_query(q: &Query) -> Result<regex::bytes::Regex, (String, usize)> {
    let mut pat = if q.mode == Mode::Literal {
        regex::escape(&q.pattern)
    } else {
        q.pattern.clone()
    };
    if q.word {
        pat = format!(r"\b(?:{pat})\b");
    }
    let is_regex = q.mode == Mode::Regex;
    regex::bytes::RegexBuilder::new(&pat)
        .case_insensitive(q.effective_case())
        // ^/$ anchor at line boundaries (rg parity: one match per line).
        .multi_line(true)
        // `$` also matches before `\r\n` (CRLF files).
        .crlf(true)
        .build()
        .map_err(|e| {
            let pos = if is_regex {
                q.pattern
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
            (format!("{e}"), pos)
        })
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

/// UTF-16 ranges for byte ranges inside a raw line. Each invalid UTF-8
/// sequence counts as one U+FFFD, which is what `from_utf8_lossy` renders, so
/// the ranges line up with the lossy text sent to the client.
fn raw_ranges_to_utf16(raw: &[u8], ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut units_at = vec![0usize; raw.len() + 1];
    let mut u = 0usize;
    let mut base = 0usize;
    for chunk in raw.utf8_chunks() {
        for (i, c) in chunk.valid().char_indices() {
            // Offsets inside a multi-byte char snap to its start.
            units_at[base + i..base + i + c.len_utf8()].fill(u);
            u += c.len_utf16();
        }
        base += chunk.valid().len();
        let bad = chunk.invalid().len();
        if bad > 0 {
            units_at[base..base + bad].fill(u);
            u += 1;
            base += bad;
        }
    }
    units_at[raw.len()] = u;
    ranges
        .iter()
        .map(|&(a, b)| {
            let a = units_at[a.min(raw.len())];
            (a, units_at[b.min(raw.len())].max(a))
        })
        .collect()
}

/// One matching line: its byte span (terminator excluded) and the match byte
/// ranges relative to the line start.
struct LineMatch {
    line: usize,
    start: usize,
    end: usize,
    ranges: Vec<(usize, usize)>,
}

/// Line-oriented matching (rg parity): every match lies inside one line and
/// all matches on a line are grouped into one [`LineMatch`]. A whole-buffer
/// search finds the next candidate line quickly; the pattern is then re-run on
/// that line alone, so matches never span a line terminator (`\s+` stops at
/// `\n`) and `$` sees the line end. `on_line` returns false to stop.
fn for_each_matching_line(
    re: &regex::bytes::Regex,
    bytes: &[u8],
    mut on_line: impl FnMut(LineMatch) -> bool,
) {
    let mut pos = 0usize;
    let mut line_no = 1usize;
    let mut counted_to = 0usize;
    while pos < bytes.len() {
        let Some(m) = re.find_at(bytes, pos) else {
            return;
        };
        let ls = memchr::memrchr(b'\n', &bytes[pos..m.start()]).map_or(pos, |p| pos + p + 1);
        if ls >= bytes.len() {
            return;
        }
        let nl = memchr::memchr(b'\n', &bytes[m.start()..]).map(|p| m.start() + p);
        let mut le = nl.unwrap_or(bytes.len());
        if le > ls && bytes[le - 1] == b'\r' {
            le -= 1;
        }
        line_no += memchr::memchr_iter(b'\n', &bytes[counted_to..ls]).count();
        counted_to = ls;
        let ranges: Vec<(usize, usize)> = re
            .find_iter(&bytes[ls..le])
            .map(|m| (m.start(), m.end()))
            .collect();
        if !ranges.is_empty()
            && !on_line(LineMatch {
                line: line_no,
                start: ls,
                end: le,
                ranges,
            })
        {
            return;
        }
        match nl {
            Some(p) => pos = p + 1,
            None => return,
        }
    }
}

/// Snippet: the line, or a ≤400-unit window around the first match. Ranges
/// are shifted into the window; ranges outside it are dropped.
fn snippet(line: &str, ranges: Vec<(usize, usize)>) -> (String, Vec<(usize, usize)>, bool, bool) {
    const MAX: usize = 400;
    let total = utf16_len(line);
    if total <= MAX {
        return (line.to_string(), ranges, false, false);
    }
    let first = ranges.first().copied().unwrap_or((0, 0));
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
    let shift = utf16_len(&line[..start_b]);
    let width = utf16_len(&line[start_b..end_b]);
    let ranges = ranges
        .into_iter()
        .filter(|&(a, b)| {
            if a == b {
                a >= shift && a <= shift + width
            } else {
                a < shift + width && b > shift
            }
        })
        .map(|(a, b)| (a.max(shift) - shift, b.min(shift + width) - shift))
        .collect();
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
    let (cands, exclude_globs, excluded_files) = candidates(snap, q);
    let (files, files_scanned, files_matched, truncated) =
        search_candidates(snap, root, q, &re, stop, &cands);
    Ok(SearchResponse {
        q: q.pattern.clone(),
        engine: "scan",
        ms: t0.elapsed().as_millis(),
        files_scanned,
        files_matched,
        truncated,
        excluded: Excluded {
            globs: exclude_globs,
            files: excluded_files,
        },
        files,
    })
}

/// Candidate file indices + exclusion accounting, shared by full and sharded scans.
pub fn candidates(snap: &FileSnapshot, q: &Query) -> (Vec<usize>, Vec<String>, usize) {
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
    (cands, exclude_globs, excluded_files)
}

/// Scan an explicit candidate list. Returns (files path-sorted, scanned, matched, truncated).
pub fn search_candidates(
    snap: &FileSnapshot,
    root: &Path,
    q: &Query,
    re: &regex::bytes::Regex,
    stop: &AtomicBool,
    cands: &[usize],
) -> (Vec<FileHits>, usize, usize, bool) {
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
        // mmap above 1 MiB, scanned in place; small files are read.
        let mapped;
        let read;
        let bytes: &[u8] = if snap.sizes.get(i).copied().unwrap_or(0) > 1024 * 1024 {
            let f = std::fs::File::open(&full).ok()?;
            mapped = unsafe { memmap2::Mmap::map(&f).ok()? };
            &mapped
        } else {
            read = std::fs::read(&full).ok()?;
            &read
        };
        if bytes.len() as u64 > max_bytes || bytes[..bytes.len().min(8192)].contains(&0) {
            return None;
        }
        let mut hits = Vec::new();
        let mut more = false;
        for_each_matching_line(re, bytes, |lm| {
            if hits.len() >= per_file {
                more = true;
                return false;
            }
            let raw = &bytes[lm.start..lm.end];
            let line = String::from_utf8_lossy(raw);
            let ranges16 = raw_ranges_to_utf16(raw, &lm.ranges);
            let (text, ranges, cut_start, cut_end) = snippet(&line, ranges16);
            hits.push(Hit {
                line: lm.line,
                text,
                ranges,
                cut_start,
                cut_end,
                def: is_ident_query && looks_like_def(&line, &q.pattern),
            });
            true
        });
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
    (
        files,
        scanned.load(Ordering::Relaxed),
        matched.load(Ordering::Relaxed),
        truncated,
    )
}

/// Find in one file (Mod+F). `full` is an already-resolved path; files over
/// `max_bytes` fail with `FileTooLarge`. `limit` caps the returned ranges;
/// `total` counts every match.
pub fn find_in_file(
    full: &Path,
    re: &regex::bytes::Regex,
    limit: usize,
    max_bytes: u64,
) -> std::io::Result<FindResponse> {
    if std::fs::metadata(full)?.len() > max_bytes {
        return Err(std::io::ErrorKind::FileTooLarge.into());
    }
    let bytes = std::fs::read(full)?;
    let mut matches = Vec::new();
    let mut total = 0usize;
    let mut kept = 0usize;
    let mut truncated = false;
    for_each_matching_line(re, &bytes, |lm| {
        total += lm.ranges.len();
        let room = limit.saturating_sub(kept);
        if room < lm.ranges.len() {
            truncated = true;
        }
        if room > 0 {
            let ranges = &lm.ranges[..room.min(lm.ranges.len())];
            kept += ranges.len();
            matches.push(FindMatch {
                line: lm.line,
                ranges: raw_ranges_to_utf16(&bytes[lm.start..lm.end], ranges),
            });
        }
        true
    });
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

    fn regex(p: &str) -> Query {
        let mut q = Query::literal(p);
        q.mode = Mode::Regex;
        q
    }

    #[test]
    fn same_line_matches_grouped() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.rs", "foo(foo)\nbar\nfoo\n")]);
        let stop = AtomicBool::new(false);
        let q = Query::literal("foo");
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        let hits = &r.files[0].hits;
        assert_eq!(hits.len(), 2);
        assert_eq!(
            (hits[0].line, hits[0].ranges.clone()),
            (1, vec![(0, 3), (4, 7)])
        );
        assert_eq!((hits[1].line, hits[1].ranges.clone()), (3, vec![(0, 3)]));

        let f = find_in_file(
            &dir.path().join("a.rs"),
            &q.compile().unwrap(),
            100,
            1 << 20,
        )
        .unwrap();
        assert_eq!(f.total, 3);
        assert_eq!(f.matches.len(), 2);
        assert_eq!(f.matches[0].ranges, vec![(0, 3), (4, 7)]);
        // The range cap applies inside a line too.
        let f = find_in_file(&dir.path().join("a.rs"), &q.compile().unwrap(), 1, 1 << 20).unwrap();
        assert_eq!((f.total, f.truncated), (3, true));
        assert_eq!(f.matches.len(), 1);
        assert_eq!(f.matches[0].ranges, vec![(0, 3)]);
    }

    #[test]
    fn matches_never_span_lines() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.txt", "foo\nbar\nfoo  \nx\n")]);
        let stop = AtomicBool::new(false);
        let r = search(&snap, dir.path(), &regex(r"foo\s+bar"), &stop).unwrap();
        assert!(r.files.is_empty());
        // `\s+` stops at the line end instead of eating the newline.
        let r = search(&snap, dir.path(), &regex(r"foo\s+"), &stop).unwrap();
        let hits = &r.files[0].hits;
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].line, hits[0].ranges.clone()), (3, vec![(0, 5)]));
    }

    #[test]
    fn crlf_lines() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.txt", "foo\r\nbar foo\r\n")]);
        let stop = AtomicBool::new(false);
        let q = regex("foo$");
        let r = search(&snap, dir.path(), &q, &stop).unwrap();
        let hits = &r.files[0].hits;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[1].text, "bar foo");
        let f = find_in_file(
            &dir.path().join("a.txt"),
            &q.compile().unwrap(),
            100,
            1 << 20,
        )
        .unwrap();
        assert_eq!(f.total, 2);
        assert_eq!(f.matches[1].ranges, vec![(4, 7)]);
    }

    #[test]
    fn invalid_utf8_ranges_follow_lossy_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latin1.txt");
        std::fs::write(&path, b"caf\xe9 foo\n\xff\xfe\n").unwrap();
        let q = Query::literal("foo");
        let f = find_in_file(&path, &q.compile().unwrap(), 100, 1 << 20).unwrap();
        // "caf\u{FFFD} foo": U+FFFD is one UTF-16 unit.
        assert_eq!(f.matches[0].ranges, vec![(5, 8)]);

        let mut snap = snap_for(dir.path(), &[]);
        snap.paths = vec!["latin1.txt".into()];
        snap.lower = snap.paths.clone();
        snap.base_off = vec![0];
        snap.sizes = vec![0];
        snap.mtimes = vec![0];
        let r = search(&snap, dir.path(), &q, &AtomicBool::new(false)).unwrap();
        let h = &r.files[0].hits[0];
        assert_eq!(h.text, "caf\u{FFFD} foo");
        assert_eq!(h.ranges, vec![(5, 8)]);
    }

    #[test]
    fn find_rejects_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        std::fs::write(&path, "foo\n".repeat(100)).unwrap();
        let re = Query::literal("foo").compile().unwrap();
        let e = find_in_file(&path, &re, 10, 100).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::FileTooLarge);
    }

    #[test]
    fn mmap_path_scans_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let mut body = "x\n".repeat(700_000);
        body.push_str("needle here\n");
        let mut snap = snap_for(dir.path(), &[("big.txt", &body)]);
        snap.sizes = vec![body.len() as u64];
        let r = search(
            &snap,
            dir.path(),
            &Query::literal("needle"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(r.files[0].hits[0].line, 700_001);
    }

    #[test]
    fn smart_case_ignores_regex_syntax() {
        assert!(!has_literal_uppercase(r"foo\S+\W\D\B", true));
        assert!(!has_literal_uppercase(r"\p{Lu}x\pL\x{FF}\xAB", true));
        assert!(!has_literal_uppercase(r"(?P<Name>a)(?<Other>b)", true));
        assert!(has_literal_uppercase(r"Foo\s", true));
        assert!(has_literal_uppercase(r"\SFoo", true));
        // Literal mode: a backslash is text, so `\S` has an uppercase letter.
        assert!(has_literal_uppercase(r"\S", false));
    }

    #[test]
    fn snippet_keeps_ranges_inside_window() {
        let line = format!(
            "{}foo{}foo{}",
            "a".repeat(100),
            "b".repeat(50),
            "c".repeat(600)
        );
        let (text, ranges, cut_start, cut_end) =
            snippet(&line, vec![(100, 103), (153, 156), (700, 703)]);
        assert!(cut_start && cut_end);
        assert_eq!(&text[ranges[0].0..ranges[0].1], "foo");
        assert_eq!(&text[ranges[1].0..ranges[1].1], "foo");
        assert_eq!(ranges.len(), 2);
    }
}
