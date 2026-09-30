//! Identifier occurrences for `GET /api/v1/nav/references`.
//!
//! The request path used to word-scan the workspace and tree-sitter-parse
//! every hit file. On Kubernetes that was seconds for a common name. The
//! symbol-index build records which files contain each code identifier
//! (comments and strings skipped, vendor trees excluded). A query re-lexes
//! only those files, in path order, and stops at the API cap.
//!
//! Full posting lists of every line and column do not fit the 40 MiB symbol
//! budget next to the definition table, so positions are computed at query
//! time from the file list.

use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;

use crate::symbols::supported;

/// One code identifier outside comments and strings.
#[derive(Debug, Clone, Copy)]
pub struct Occ<'a> {
    pub line: u32,
    pub col: u32,
    pub name: &'a str,
}

#[derive(Clone, Copy)]
enum Family {
    Go,
    Rust,
    /// Java, JS, TS, C, and (with `hash`) PHP.
    C,
    Python,
    Ruby,
}

/// Lex `text` when `ext` is a supported language. `None` for the rest.
pub fn occurrences<'a>(ext: &str, text: &'a str) -> Option<Vec<Occ<'a>>> {
    let family = family_of(ext)?;
    Some(scan(family, text, ext == "php"))
}

fn family_of(ext: &str) -> Option<Family> {
    if !supported(ext) {
        return None;
    }
    Some(match ext {
        "go" => Family::Go,
        "rs" => Family::Rust,
        "py" => Family::Python,
        "rb" => Family::Ruby,
        "php" => Family::C,
        _ => Family::C,
    })
}

/// True when a path sits under a `vendor` directory. Reference search
/// excludes those by default; the index does too.
pub fn is_vendor_path(path: &str) -> bool {
    path.split(['/', '\\']).any(|s| s == "vendor")
}

fn scan(family: Family, text: &str, hash_line: bool) -> Vec<Occ<'_>> {
    let mut out = Vec::new();
    for_each_ident(family, text, hash_line, |occ| {
        out.push(occ);
        true
    });
    out
}

/// `f` returns false to stop the scan.
fn for_each_ident<'a>(
    family: Family,
    text: &'a str,
    hash_line: bool,
    mut f: impl FnMut(Occ<'a>) -> bool,
) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut line: u32 = 1;
    let mut line_start = 0usize;
    while i < bytes.len() {
        if let Some(end) = skip_comment_or_string(family, bytes, i, hash_line) {
            let end = end.max(i + 1).min(bytes.len());
            advance_line(&mut line, &mut line_start, bytes, i, end);
            i = end;
            continue;
        }
        let b = bytes[i];
        if b == b'\n' {
            line += 1;
            i += 1;
            line_start = i;
            continue;
        }
        if b == b' ' || b == b'\t' || b == b'\r' {
            i += 1;
            continue;
        }
        if is_ident_start(bytes, i) {
            let start = i;
            i += utf8_len(bytes[i]);
            while i < bytes.len() && is_ident_continue(bytes, i) {
                i += utf8_len(bytes[i]);
            }
            if i - start <= 128 {
                if let Some(name) = text.get(start..i) {
                    let col = col_on_line(text, line_start, start);
                    if !f(Occ { line, col, name }) {
                        return;
                    }
                }
            }
            continue;
        }
        let w = utf8_len(b);
        i += w;
    }
}

fn advance_line(line: &mut u32, line_start: &mut usize, bytes: &[u8], from: usize, to: usize) {
    for (off, &c) in bytes[from..to].iter().enumerate() {
        if c == b'\n' {
            *line += 1;
            *line_start = from + off + 1;
        }
    }
}

fn col_on_line(text: &str, line_start: usize, byte: usize) -> u32 {
    let line = text.get(line_start..byte).unwrap_or("");
    if line.is_ascii() {
        return (byte - line_start) as u32 + 1;
    }
    crate::text::utf8_to_utf16_offset(line, line.len()) as u32 + 1
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b & 0xE0 == 0xC0 {
        2
    } else if b & 0xF0 == 0xE0 {
        3
    } else if b & 0xF8 == 0xF0 {
        4
    } else {
        1
    }
}

fn is_ident_start(bytes: &[u8], i: usize) -> bool {
    let b = bytes[i];
    if b.is_ascii_digit() {
        return false;
    }
    if b.is_ascii_alphabetic() || b == b'_' {
        return true;
    }
    if b < 0x80 {
        return false;
    }
    let ch = decode_char(bytes, i);
    ch.is_alphanumeric()
}

fn is_ident_continue(bytes: &[u8], i: usize) -> bool {
    let b = bytes[i];
    if b.is_ascii_alphanumeric() || b == b'_' {
        return true;
    }
    if b < 0x80 {
        return false;
    }
    decode_char(bytes, i).is_alphanumeric()
}

fn decode_char(bytes: &[u8], i: usize) -> char {
    let w = utf8_len(bytes[i]).min(bytes.len() - i);
    let s = std::str::from_utf8(&bytes[i..i + w]).unwrap_or("\u{FFFD}");
    s.chars().next().unwrap_or('\u{FFFD}')
}

/// Byte index just past a comment or string starting at `i`, when one starts there.
fn skip_comment_or_string(
    family: Family,
    bytes: &[u8],
    i: usize,
    hash_line: bool,
) -> Option<usize> {
    let b = bytes[i];
    match family {
        Family::Go => skip_go(bytes, i, b),
        Family::Rust => skip_rust(bytes, i, b),
        Family::C => skip_c(bytes, i, b, hash_line),
        Family::Python => skip_python(bytes, i),
        Family::Ruby => skip_ruby(bytes, i, b),
    }
}

fn skip_go(bytes: &[u8], i: usize, b: u8) -> Option<usize> {
    if b == b'/' {
        return skip_slash_comment(bytes, i, false);
    }
    if b == b'"' || b == b'\'' {
        return Some(skip_quoted(bytes, i, b, true));
    }
    if b == b'`' {
        return Some(skip_until(bytes, i + 1, b'`'));
    }
    None
}

fn skip_rust(bytes: &[u8], i: usize, b: u8) -> Option<usize> {
    if b == b'/' {
        return skip_slash_comment(bytes, i, true);
    }
    if let Some(n) = skip_rust_raw_at(bytes, i) {
        return Some(n);
    }
    if b == b'"' {
        return Some(skip_quoted(bytes, i, b'"', true));
    }
    // `'` is a lifetime tick or a char literal. Char literals are not
    // string nodes in tree-sitter, so the character text stays searchable
    // the same way the old word scan kept it. Don't swallow it here.
    if b == b'b' || b == b'c' {
        if let Some(n) = skip_rust_raw_at(bytes, i) {
            return Some(n);
        }
        let n = bytes.get(i + 1).copied()?;
        if n == b'"' || n == b'\'' {
            return Some(skip_quoted(bytes, i + 1, n, true));
        }
    }
    None
}

fn skip_c(bytes: &[u8], i: usize, b: u8, php_hash: bool) -> Option<usize> {
    if php_hash && b == b'#' {
        return Some(skip_until_newline(bytes, i + 1));
    }
    if b == b'/' {
        return skip_slash_comment(bytes, i, false);
    }
    if b == b'"' || b == b'\'' {
        // Java text block """ ... """
        if b == b'"' && bytes.get(i + 1) == Some(&b'"') && bytes.get(i + 2) == Some(&b'"') {
            return Some(skip_java_text(bytes, i + 3));
        }
        return Some(skip_quoted(bytes, i, b, true));
    }
    None
}

fn skip_python(bytes: &[u8], i: usize) -> Option<usize> {
    let b = bytes[i];
    if b == b'#' {
        return Some(skip_until_newline(bytes, i + 1));
    }
    // Optional string prefix (r, u, b, f and combinations) then a quote.
    let mut j = i;
    let mut prefixed = false;
    while j < bytes.len() && matches!(bytes[j].to_ascii_lowercase(), b'r' | b'u' | b'b' | b'f') {
        // A real identifier may start with those letters. Only treat this as
        // a prefix when a quote follows the run.
        j += 1;
        prefixed = true;
        if j - i > 3 {
            break;
        }
    }
    if prefixed {
        if matches!(bytes.get(j), Some(&b'"' | &b'\'')) {
            return Some(skip_py_quotes(bytes, j));
        }
        return None;
    }
    if b == b'"' || b == b'\'' {
        return Some(skip_py_quotes(bytes, i));
    }
    None
}

fn skip_py_quotes(bytes: &[u8], i: usize) -> usize {
    let q = bytes[i];
    let triple = bytes.get(i + 1) == Some(&q) && bytes.get(i + 2) == Some(&q);
    if triple {
        let mut j = i + 3;
        while j + 2 < bytes.len() {
            if bytes[j] == b'\\' {
                j += 2;
                continue;
            }
            if bytes[j] == q && bytes[j + 1] == q && bytes[j + 2] == q {
                return j + 3;
            }
            j += 1;
        }
        return bytes.len();
    }
    skip_quoted(bytes, i, q, true)
}

fn skip_ruby(bytes: &[u8], i: usize, b: u8) -> Option<usize> {
    if b == b'#' {
        return Some(skip_until_newline(bytes, i + 1));
    }
    if b == b'"' || b == b'\'' {
        return Some(skip_quoted(bytes, i, b, true));
    }
    None
}

fn skip_slash_comment(bytes: &[u8], i: usize, nested: bool) -> Option<usize> {
    if bytes.get(i) != Some(&b'/') {
        return None;
    }
    match bytes.get(i + 1).copied() {
        Some(b'/') => Some(skip_until_newline(bytes, i + 2)),
        Some(b'*') => Some(skip_block_comment(bytes, i + 2, nested)),
        _ => None,
    }
}

fn skip_block_comment(bytes: &[u8], mut j: usize, nested: bool) -> usize {
    let mut depth = 1i32;
    while j + 1 < bytes.len() {
        if nested && bytes[j] == b'/' && bytes[j + 1] == b'*' {
            depth += 1;
            j += 2;
            continue;
        }
        if bytes[j] == b'*' && bytes[j + 1] == b'/' {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return j;
            }
            continue;
        }
        j += 1;
    }
    bytes.len()
}

fn skip_quoted(bytes: &[u8], i: usize, quote: u8, escapes: bool) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        if escapes && bytes[j] == b'\\' {
            j += 2;
            continue;
        }
        if bytes[j] == quote {
            return j + 1;
        }
        if bytes[j] == b'\n' && quote == b'\'' {
            // Unclosed char: don't swallow the rest of the file.
            return j;
        }
        j += 1;
    }
    bytes.len()
}

fn skip_until(bytes: &[u8], mut j: usize, end: u8) -> usize {
    while j < bytes.len() {
        if bytes[j] == end {
            return j + 1;
        }
        j += 1;
    }
    bytes.len()
}

fn skip_until_newline(bytes: &[u8], mut j: usize) -> usize {
    while j < bytes.len() && bytes[j] != b'\n' {
        j += 1;
    }
    j
}

fn skip_java_text(bytes: &[u8], mut j: usize) -> usize {
    while j + 2 < bytes.len() {
        if bytes[j] == b'"' && bytes[j + 1] == b'"' && bytes[j + 2] == b'"' {
            return j + 3;
        }
        j += 1;
    }
    bytes.len()
}

fn skip_rust_raw_at(bytes: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    if matches!(bytes.get(j), Some(&b'b' | &b'c')) {
        j += 1;
    }
    if bytes.get(j) != Some(&b'r') {
        return None;
    }
    j += 1;
    let mut hashes = 0usize;
    while bytes.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
        if hashes > 16 {
            return None;
        }
    }
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < bytes.len() {
        if bytes[j] == b'"' && hash_run(bytes, j + 1, hashes) {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(bytes.len())
}

fn hash_run(bytes: &[u8], mut j: usize, n: usize) -> bool {
    for _ in 0..n {
        if bytes.get(j) != Some(&b'#') {
            return false;
        }
        j += 1;
    }
    true
}

// -- file lists --------------------------------------------------------------

/// `(workspace path, 1-based line, 1-based column)`.
pub type RefHit = (String, u32, u32);

/// Inverted identifier index swapped with the symbol table.
///
/// Each name records the path indexes that contain it, not every line.
/// [`RefIndex::lookup`] re-lexes those files in path order.
#[derive(Debug, Clone)]
pub struct RefIndex {
    active: bool,
    /// Sorted names. Parallel with `spans`.
    names: Vec<String>,
    /// `(start, len)` into `files` for each name.
    spans: Vec<(u32, u32)>,
    files: Vec<u32>,
    /// Packed positions for names that span many files, sorted by name id.
    /// `(name_id, start, len)` into `locs`. Absent names are re-lexed.
    wide: Vec<(u32, u32, u32)>,
    locs: Vec<u64>,
}

impl RefIndex {
    pub fn inactive() -> Self {
        Self {
            active: false,
            names: Vec::new(),
            spans: Vec::new(),
            files: Vec::new(),
            wide: Vec::new(),
            locs: Vec::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Build from per-file unique names, in path-table order.
    pub fn from_names(per_file: Vec<Vec<String>>) -> Self {
        let mut map: std::collections::HashMap<String, Vec<u32>> = std::collections::HashMap::new();
        for (path_idx, names) in per_file.into_iter().enumerate() {
            let path_idx = path_idx as u32;
            let mut seen = HashSet::new();
            for name in names {
                if name.is_empty() || !seen.insert(name.clone()) {
                    continue;
                }
                map.entry(name).or_default().push(path_idx);
            }
        }
        let mut acc: Vec<(String, Vec<u32>)> = map.into_iter().collect();
        acc.sort_by(|a, b| a.0.cmp(&b.0));
        let mut names = Vec::with_capacity(acc.len());
        let mut spans = Vec::with_capacity(acc.len());
        let mut files = Vec::new();
        for (name, ids) in acc {
            let start = files.len() as u32;
            let len = ids.len() as u32;
            files.extend(ids);
            names.push(name);
            spans.push((start, len));
        }
        names.shrink_to_fit();
        spans.shrink_to_fit();
        files.shrink_to_fit();
        Self {
            active: true,
            names,
            spans,
            files,
            wide: Vec::new(),
            locs: Vec::new(),
        }
    }

    pub fn lookup(
        &self,
        name: &str,
        root: &Path,
        paths: &[String],
        limit: usize,
    ) -> Option<(Vec<RefHit>, bool)> {
        if !self.active {
            return None;
        }
        let Ok(id) = self.names.binary_search_by(|n| n.as_str().cmp(name)) else {
            return Some((Vec::new(), false));
        };
        if let Ok(pos) = self.wide.binary_search_by_key(&(id as u32), |w| w.0) {
            let (_, start, len) = self.wide[pos];
            let rows = &self.locs[start as usize..(start + len) as usize];
            let truncated = rows.len() > limit;
            let hits = rows
                .iter()
                .take(limit)
                .map(|packed| {
                    let (path_i, line, col) = unpack_loc(*packed);
                    let path = paths.get(path_i as usize).cloned().unwrap_or_default();
                    (path, line, col)
                })
                .collect();
            return Some((hits, truncated));
        }
        let (start, len) = self.spans[id];
        let ids = &self.files[start as usize..(start + len) as usize];
        let mut order: Vec<u32> = ids.to_vec();
        order.sort_by(|&a, &b| {
            paths
                .get(a as usize)
                .map(String::as_str)
                .unwrap_or("")
                .cmp(paths.get(b as usize).map(String::as_str).unwrap_or(""))
                .then(a.cmp(&b))
        });
        order.dedup();
        let mut hits: Vec<(String, u32, u32)> = Vec::new();
        let mut truncated = false;
        for path_idx in order {
            if hits.len() > limit {
                truncated = true;
                break;
            }
            let Some(rel) = paths.get(path_idx as usize) else {
                continue;
            };
            let room = limit + 1 - hits.len();
            let found = read_positions(root, rel, name, room);
            if hits.len() + found.len() > limit {
                truncated = true;
                let keep = limit - hits.len();
                for (line, col) in found.into_iter().take(keep) {
                    hits.push((rel.clone(), line, col));
                }
                break;
            }
            for (line, col) in found {
                hits.push((rel.clone(), line, col));
            }
        }
        if hits.len() > limit {
            truncated = true;
            hits.truncate(limit);
        }
        Some((hits, truncated))
    }

    /// Drop path slots `remap` does not mention and renumber the rest.
    pub fn remap(&mut self, remap: &std::collections::HashMap<u32, u32>) {
        if !self.active {
            return;
        }
        let mut files = Vec::with_capacity(self.files.len());
        for (start, len) in &mut self.spans {
            let old = &self.files[*start as usize..(*start + *len) as usize];
            let new_start = files.len() as u32;
            for id in old {
                if let Some(&neu) = remap.get(id) {
                    files.push(neu);
                }
            }
            *start = new_start;
            *len = (files.len() as u32) - new_start;
        }
        self.files = files;
        // Stored positions carry pre-remap path indexes. Edits re-lex.
        self.wide.clear();
        self.locs.clear();
    }

    /// Record that `path_idx` contains each of `names`.
    pub fn add_file(&mut self, path_idx: u32, names_in: &[String]) {
        if !self.active {
            return;
        }
        let mut seen = HashSet::new();
        for name in names_in {
            if name.is_empty() || !seen.insert(name.as_str()) {
                continue;
            }
            match self.names.binary_search_by(|n| n.as_str().cmp(name)) {
                Ok(id) => self.push_file(id, path_idx),
                Err(at) => self.insert_name(at, name.clone(), path_idx),
            }
        }
    }

    fn push_file(&mut self, id: usize, path_idx: u32) {
        let (start, len) = self.spans[id];
        let range = start as usize..(start + len) as usize;
        if self.files[range].contains(&path_idx) {
            return;
        }
        self.files.insert((start + len) as usize, path_idx);
        self.spans[id].1 += 1;
        for (s, _) in self.spans.iter_mut().skip(id + 1) {
            *s += 1;
        }
    }

    fn insert_name(&mut self, at: usize, name: String, path_idx: u32) {
        let start = if at == 0 {
            0
        } else {
            let (s, l) = self.spans[at - 1];
            s + l
        };
        self.files.insert(start as usize, path_idx);
        for (s, _) in self.spans.iter_mut().skip(at) {
            *s += 1;
        }
        self.names.insert(at, name);
        self.spans.insert(at, (start, 1));
    }

    pub fn explain_heap(&self) {
        let mut name_bytes = 0usize;
        for name in &self.names {
            name_bytes += name.capacity() + std::mem::size_of::<String>();
        }
        let spans = self.spans.capacity() * std::mem::size_of::<(u32, u32)>();
        let files = self.files.capacity() * 4;
        eprintln!(
            "ref parts names={name_bytes} n={} spans={spans} files={files} wide={} locs={}",
            self.names.len(),
            self.wide.len(),
            self.locs.len()
        );
    }

    pub fn heap_bytes(&self) -> usize {
        let mut n = 0usize;
        for name in &self.names {
            n += name.capacity() + std::mem::size_of::<String>();
        }
        n += self.spans.capacity() * std::mem::size_of::<(u32, u32)>();
        n += self.files.capacity() * 4;
        n += self.wide.capacity() * std::mem::size_of::<(u32, u32, u32)>();
        n += self.locs.capacity() * 8;
        n
    }

    pub fn name_count(&self) -> usize {
        self.names.len()
    }
}

impl Default for RefIndex {
    fn default() -> Self {
        Self::inactive()
    }
}

#[derive(Clone, Default)]
struct FxBuild;

impl BuildHasher for FxBuild {
    type Hasher = FxHasher;
    fn build_hasher(&self) -> FxHasher {
        FxHasher(0)
    }
}

struct FxHasher(u64);

impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        let mut i = 0;
        while i + 8 <= bytes.len() {
            let n = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
            hash = (hash.rotate_left(5) ^ n).wrapping_mul(0x517cc1b727220a95);
            i += 8;
        }
        while i < bytes.len() {
            hash = (hash.rotate_left(5) ^ u64::from(bytes[i])).wrapping_mul(0x517cc1b727220a95);
            i += 1;
        }
        self.0 = hash;
    }
}

/// Positions kept for names that show up in many files. Narrow names are
/// re-lexed from their file list, which is cheap. This cap is what keeps
/// the symbol table inside the 40 MiB budget.
const WIDE_FILE_MIN: usize = 12;
const LOC_BUDGET: usize = 600_000;
const LOC_CAP: usize = 1001;

fn pack_loc(path: u32, line: u32, col: u32) -> u64 {
    let col = u64::from(col.min(u16::MAX as u32));
    let line = u64::from(line) & 0x00ff_ffff;
    let path = u64::from(path) & 0x00ff_ffff;
    (path << 40) | (line << 16) | col
}

fn unpack_loc(v: u64) -> (u32, u32, u32) {
    let path = (v >> 40) as u32;
    let line = ((v >> 16) & 0x00ff_ffff) as u32;
    let col = (v & 0xffff) as u32;
    (path, line, col)
}

struct ShardAcc {
    files: Vec<u32>,
    locs: Vec<u64>,
}

/// Filled from the parallel parse. Provisional path indexes are remapped in
/// [`RefShards::finish`].
pub struct RefShards {
    shards: Vec<parking_lot::Mutex<std::collections::HashMap<String, ShardAcc, FxBuild>>>,
}

impl Default for RefShards {
    fn default() -> Self {
        Self::new()
    }
}

impl RefShards {
    pub fn new() -> Self {
        let shards = (0..64)
            .map(|_| parking_lot::Mutex::new(std::collections::HashMap::with_hasher(FxBuild)))
            .collect();
        Self { shards }
    }

    fn shard(name: &str) -> usize {
        let mut hasher = FxHasher(0);
        hasher.write(name.as_bytes());
        (hasher.finish() as usize) & 63
    }

    pub fn note(&self, path_idx: u32, ext: &str, text: &str) {
        if ext == "go" && text.is_ascii() {
            self.note_go_ascii(path_idx, text);
            return;
        }
        let Some(family) = family_of(ext) else {
            return;
        };
        let mut local: std::collections::HashMap<&str, Vec<u64>, FxBuild> =
            std::collections::HashMap::with_hasher(FxBuild);
        for_each_ident(family, text, ext == "php", |occ| {
            local
                .entry(occ.name)
                .or_default()
                .push(pack_loc(path_idx, occ.line, occ.col));
            true
        });
        self.apply(path_idx, local.into_iter().collect());
    }

    /// ASCII Go scan. Kubernetes is almost entirely this shape; the generic
    /// lexer spends the build budget on per-identifier hashing.
    fn note_go_ascii(&self, path_idx: u32, text: &str) {
        let bytes = text.as_bytes();
        let mut raw: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(256);
        let mut i = 0usize;
        let mut line = 1u32;
        let mut line_start = 0usize;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'\n' {
                line += 1;
                i += 1;
                line_start = i;
                continue;
            }
            if c <= b' ' {
                i += 1;
                continue;
            }
            if c == b'/' {
                match bytes.get(i + 1) {
                    Some(b'/') => {
                        i += 2;
                        while i < bytes.len() && bytes[i] != b'\n' {
                            i += 1;
                        }
                        continue;
                    }
                    Some(b'*') => {
                        i += 2;
                        while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                            if bytes[i] == b'\n' {
                                line += 1;
                                line_start = i + 1;
                            }
                            i += 1;
                        }
                        i = (i + 2).min(bytes.len());
                        continue;
                    }
                    _ => {}
                }
            }
            if c == b'"' || c == b'\'' {
                let quote = c;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote || bytes[i] == b'\n' && quote == b'\'' {
                        i += 1;
                        break;
                    }
                    if bytes[i] == b'\n' {
                        line += 1;
                        line_start = i + 1;
                    }
                    i += 1;
                }
                continue;
            }
            if c == b'`' {
                i += 1;
                while i < bytes.len() && bytes[i] != b'`' {
                    if bytes[i] == b'\n' {
                        line += 1;
                        line_start = i + 1;
                    }
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                continue;
            }
            if c.is_ascii_alphabetic() || c == b'_' {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if i - start <= 128 {
                    let col = (start - line_start) as u32 + 1;
                    raw.push((start as u32, (i - start) as u32, line, col));
                }
                continue;
            }
            i += 1;
        }
        raw.sort_unstable_by(|a, b| {
            bytes[a.0 as usize..(a.0 + a.1) as usize]
                .cmp(&bytes[b.0 as usize..(b.0 + b.1) as usize])
        });
        let mut groups: Vec<(&str, Vec<u64>)> = Vec::new();
        let mut at = 0;
        while at < raw.len() {
            let name = &text[raw[at].0 as usize..(raw[at].0 + raw[at].1) as usize];
            let mut batch = Vec::new();
            while at < raw.len() {
                let n = &text[raw[at].0 as usize..(raw[at].0 + raw[at].1) as usize];
                if n != name {
                    break;
                }
                batch.push(pack_loc(path_idx, raw[at].2, raw[at].3));
                at += 1;
            }
            groups.push((name, batch));
        }
        self.apply(path_idx, groups);
    }

    fn apply(&self, path_idx: u32, groups: Vec<(&str, Vec<u64>)>) {
        let mut buckets: Vec<Vec<(&str, Vec<u64>)>> = (0..64).map(|_| Vec::new()).collect();
        for group in groups {
            buckets[Self::shard(group.0)].push(group);
        }
        for (si, groups) in buckets.into_iter().enumerate() {
            if groups.is_empty() {
                continue;
            }
            let mut map = self.shards[si].lock();
            for (name, batch) in groups {
                let acc = if let Some(acc) = map.get_mut(name) {
                    acc
                } else {
                    map.insert(
                        name.to_string(),
                        ShardAcc {
                            files: Vec::new(),
                            locs: Vec::new(),
                        },
                    );
                    map.get_mut(name).unwrap()
                };
                acc.files.push(path_idx);
                push_path_prefix(&mut acc.locs, &batch);
            }
        }
    }

    pub fn finish(self, remap: &[u32]) -> RefIndex {
        let mut rows: Vec<(String, Vec<u32>, Vec<u64>)> = Vec::new();
        for shard in self.shards {
            for (name, acc) in shard.into_inner() {
                let mut files = Vec::new();
                for id in acc.files {
                    if let Some(&neu) = remap.get(id as usize) {
                        if neu != u32::MAX {
                            files.push(neu);
                        }
                    }
                }
                let mut locs = Vec::new();
                for packed in acc.locs {
                    let (path, line, col) = unpack_loc(packed);
                    let Some(&neu) = remap.get(path as usize) else {
                        continue;
                    };
                    if neu == u32::MAX {
                        continue;
                    }
                    locs.push(pack_loc(neu, line, col));
                }
                locs.sort_unstable();
                locs.truncate(LOC_CAP);
                if !files.is_empty() {
                    rows.push((name, files, locs));
                }
            }
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let mut keep = vec![false; rows.len()];
        let mut ranked: Vec<usize> = (0..rows.len())
            .filter(|&i| rows[i].1.len() >= WIDE_FILE_MIN)
            .collect();
        ranked.sort_by_key(|&i| std::cmp::Reverse(rows[i].1.len()));
        let mut budget = LOC_BUDGET;
        for i in ranked {
            let n = rows[i].2.len();
            if n > budget {
                break;
            }
            keep[i] = true;
            budget -= n;
        }
        let mut names = Vec::with_capacity(rows.len());
        let mut spans = Vec::with_capacity(rows.len());
        let mut file_ids = Vec::new();
        let mut wide = Vec::new();
        let mut out_locs = Vec::new();
        for (i, (name, ids, packed)) in rows.into_iter().enumerate() {
            let start = file_ids.len() as u32;
            file_ids.extend(ids);
            let len = (file_ids.len() as u32) - start;
            if keep[i] && !packed.is_empty() {
                let ls = out_locs.len() as u32;
                wide.push((i as u32, ls, packed.len() as u32));
                out_locs.extend(packed);
            }
            names.push(name);
            spans.push((start, len));
        }
        names.shrink_to_fit();
        spans.shrink_to_fit();
        file_ids.shrink_to_fit();
        wide.shrink_to_fit();
        out_locs.shrink_to_fit();
        RefIndex {
            active: true,
            names,
            spans,
            files: file_ids,
            wide,
            locs: out_locs,
        }
    }
}

fn push_path_prefix(locs: &mut Vec<u64>, batch: &[u64]) {
    if batch.is_empty() {
        return;
    }
    if locs.len() >= LOC_CAP {
        let last = unpack_loc(*locs.last().unwrap()).0;
        let path = unpack_loc(batch[0]).0;
        if path >= last {
            return;
        }
    }
    let at = locs
        .iter()
        .position(|v| *v > batch[0])
        .unwrap_or(locs.len());
    locs.splice(at..at, batch.iter().copied());
    if locs.len() > LOC_CAP {
        locs.truncate(LOC_CAP);
    }
}

pub(crate) fn unique_names(ext: &str, text: &str) -> Vec<String> {
    let Some(family) = family_of(ext) else {
        return Vec::new();
    };
    let mut set = HashSet::new();
    for_each_ident(family, text, ext == "php", |occ| {
        set.insert(occ.name);
        true
    });
    let mut names: Vec<String> = set.into_iter().map(str::to_string).collect();
    names.shrink_to_fit();
    names
}

fn read_positions(root: &Path, rel: &str, name: &str, max: usize) -> Vec<(u32, u32)> {
    if max == 0 {
        return Vec::new();
    }
    let ext = rel.rsplit('.').next().unwrap_or("");
    let abs = root.join(rel.trim_start_matches('/'));
    let Ok(bytes) = std::fs::read(&abs) else {
        return Vec::new();
    };
    if bytes.len() as u64 > 2 * 1024 * 1024 || bytes[..bytes.len().min(8192)].contains(&0) {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    positions_of(ext, &text, name, max)
}

pub(crate) fn positions_of(ext: &str, text: &str, name: &str, max: usize) -> Vec<(u32, u32)> {
    let Some(family) = family_of(ext) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if max == 0 {
        return out;
    }
    for_each_ident(family, text, ext == "php", |occ| {
        if occ.name == name {
            out.push((occ.line, occ.col));
            if out.len() >= max {
                return false;
            }
        }
        true
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ext: &str, text: &str) -> Vec<(u32, u32, String)> {
        occurrences(ext, text)
            .unwrap()
            .into_iter()
            .map(|o| (o.line, o.col, o.name.to_string()))
            .collect()
    }

    #[test]
    fn go_skips_comments_and_strings() {
        let text = "package p\n// Run\nfunc Run() {\n\ts := \"Run\"\n\tRun()\n}\n";
        let got = names("go", text);
        let runs: Vec<_> = got.iter().filter(|(_, _, n)| n == "Run").cloned().collect();
        assert_eq!(
            runs,
            vec![(3, 6, "Run".into()), (5, 2, "Run".into())],
            "{got:?}"
        );
    }

    #[test]
    fn rust_skips_raw_string_nested_comment_and_lifetime() {
        let text = "fn foo() {\n    // foo\n    /* /* foo */ */\n    let _ = r#\"foo\"#;\n    let _ = 'f';\n    foo();\n}\n";
        let got = names("rs", text);
        let foos: Vec<_> = got.iter().filter(|(_, _, n)| n == "foo").cloned().collect();
        assert_eq!(
            foos,
            vec![(1, 4, "foo".into()), (6, 5, "foo".into())],
            "{got:?}"
        );
    }

    #[test]
    fn word_inside_longer_ident_is_not_split() {
        let text = "fn foo() { foobar(); }\n";
        let got = names("go", text);
        assert!(got.iter().any(|(_, _, n)| n == "foo"));
        assert!(got.iter().any(|(_, _, n)| n == "foobar"));
        assert_eq!(got.iter().filter(|(_, _, n)| n == "foo").count(), 1);
    }

    #[cfg(feature = "ts-go")]
    #[test]
    fn go_lexer_matches_treesitter_exclusions() {
        let text = concat!(
            "package p\n",
            "// Run hides\n",
            "func Run() {\n",
            "\ts := \"Run\"\n",
            "\traw := `Run`\n",
            "\t/* Run */\n",
            "\tRun()\n",
            "}\n",
        );
        let got = names("go", text);
        let expect = treesitter_words(tree_sitter_go::LANGUAGE.into(), text);
        assert_eq!(got, expect, "lexer drifted from tree-sitter");
    }

    #[cfg(feature = "ts-rust")]
    #[test]
    fn rust_lexer_matches_treesitter_exclusions() {
        let text = "fn foo() {\n    // foo\n    /* /* foo */ */\n    let _ = r#\"foo\"#;\n    let _ = 'f';\n    foo();\n}\n";
        let got = names("rs", text);
        let expect = treesitter_words(tree_sitter_rust::LANGUAGE.into(), text);
        assert_eq!(got, expect, "lexer drifted from tree-sitter");
    }

    #[cfg(feature = "tree-sitter")]
    fn treesitter_words(lang: tree_sitter::Language, text: &str) -> Vec<(u32, u32, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse(text, None).unwrap();
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0usize;
        let mut line = 1u32;
        let mut line_start = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'\n' {
                line += 1;
                i += 1;
                line_start = i;
                continue;
            }
            if super::is_ident_start(bytes, i) {
                let start = i;
                i += super::utf8_len(bytes[i]);
                while i < bytes.len() && super::is_ident_continue(bytes, i) {
                    i += super::utf8_len(bytes[i]);
                }
                if i - start <= 128 && in_code(tree.root_node(), start) {
                    let col = (start - line_start) as u32 + 1;
                    out.push((line, col, text[start..i].to_string()));
                }
                continue;
            }
            i += super::utf8_len(bytes[i]);
        }
        out
    }

    #[cfg(feature = "tree-sitter")]
    fn in_code(root: tree_sitter::Node<'_>, byte_off: usize) -> bool {
        let Some(mut node) = root.descendant_for_byte_range(byte_off, byte_off + 1) else {
            return true;
        };
        loop {
            let mut advanced = false;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named()
                    && child.start_byte() <= byte_off
                    && byte_off < child.end_byte()
                    && (child.end_byte() - child.start_byte())
                        < (node.end_byte() - node.start_byte())
                {
                    node = child;
                    advanced = true;
                    break;
                }
            }
            if !advanced {
                break;
            }
        }
        let mut cur = Some(node);
        while let Some(n) = cur {
            let k = n.kind();
            if k.contains("comment") || k.contains("string") {
                return false;
            }
            cur = n.parent();
        }
        true
    }

    #[test]
    fn window_truncates_in_path_order() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec!["a.go".to_string(), "b.go".to_string(), "c.go".to_string()];
        let mut per = Vec::new();
        for p in &paths {
            let mut body = String::from("package p\n");
            for _ in 0..400 {
                body.push_str("Run()\n");
            }
            std::fs::write(dir.path().join(p), &body).unwrap();
            per.push(vec!["Run".into()]);
        }
        let idx = RefIndex::from_names(per);
        let (hits, truncated) = idx.lookup("Run", dir.path(), &paths, 1000).unwrap();
        assert!(truncated);
        assert_eq!(hits.len(), 1000);
        assert_eq!(hits[0].0, "a.go");
        assert_eq!(hits[999].0, "c.go");
    }
}
