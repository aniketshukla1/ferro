//! Code navigation (B6): goto-definition ranking, references with
//! identifier filtering, and hover signatures over the workspace.
//! All queries resolve through the [`SymbolIndex`](crate::symindex::SymbolIndex)
//! (tree-sitter definitions) plus the file snapshot (word search).
//! Columns are 1-based UTF-16 code units, matching the highlight contract.

use std::collections::HashMap;
use std::path::Path;

use crate::fileindex::FileSnapshot;
use crate::symindex::{ResolvedSymbol, SymbolIndex};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_REFERENCES: usize = 1000;

#[derive(Debug, Clone)]
pub struct Definition {
    pub path: String,
    pub line: u32,
    pub end_line: u32,
    pub kind: &'static str,
    pub name: String,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub path: String,
    pub line: usize,
    pub col: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Hover {
    pub name: String,
    pub kind: &'static str,
    pub path: String,
    pub line: u32,
    pub signature: String,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavError {
    BadPath(String),
    BadPosition(String),
    NoIdentifier,
    TooLarge,
}

impl std::fmt::Display for NavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NavError::BadPath(p) => write!(f, "cannot read: {p}"),
            NavError::BadPosition(m) => write!(f, "{m}"),
            NavError::NoIdentifier => write!(f, "no identifier at position"),
            NavError::TooLarge => write!(f, "file too large"),
        }
    }
}

pub struct Nav<'a> {
    pub root: &'a Path,
    pub snap: &'a FileSnapshot,
    pub syms: &'a SymbolIndex,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Identifier under (1-based) char column on a line + its start column.
pub fn identifier_at(line_text: &str, col: usize) -> Option<(String, usize)> {
    if col == 0 {
        return None;
    }
    let chars: Vec<char> = line_text.chars().collect();
    let i = col.saturating_sub(1);
    if i >= chars.len() || !is_ident_char(chars[i]) {
        return None;
    }
    let mut s = i;
    while s > 0 && is_ident_char(chars[s - 1]) {
        s -= 1;
    }
    let mut e = i;
    while e + 1 < chars.len() && is_ident_char(chars[e + 1]) {
        e += 1;
    }
    Some((chars[s..=e].iter().collect(), s + 1))
}

fn read_lines(root: &Path, rel: &str) -> Result<(Vec<u8>, Vec<String>), NavError> {
    if rel.len() > 512 {
        return Err(NavError::BadPath("path too long".into()));
    }
    let abs = root.join(rel.trim_start_matches('/'));
    let bytes =
        std::fs::read(&abs).map_err(|_| NavError::BadPath(format!("cannot read: {rel}")))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(NavError::TooLarge);
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(NavError::BadPath(format!("cannot read: {rel}")));
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok((
        bytes.to_vec(),
        text.lines().map(|l| l.to_string()).collect(),
    ))
}

/// Identifier at a 1-based (line, UTF-16 col) position.
pub fn identifier_at_pos(
    root: &Path,
    rel: &str,
    line: usize,
    col: usize,
) -> Result<String, NavError> {
    if line == 0 || col == 0 {
        return Err(NavError::BadPosition("line and col start at 1".into()));
    }
    let (_, lines) = read_lines(root, rel)?;
    let text = lines
        .get(line - 1)
        .ok_or(NavError::BadPosition("line past end".into()))?;
    // UTF-16 col → char index.
    let mut units = 0usize;
    let mut char_idx = None;
    for (i, c) in text.chars().enumerate() {
        if units + 1 >= col {
            char_idx = Some(i);
            break;
        }
        units += c.len_utf16();
    }
    let char_idx = match char_idx {
        Some(i) => i,
        None if col - 1 == text.encode_utf16().count() && !text.is_empty() => {
            text.chars().count() - 1
        }
        None => return Err(NavError::NoIdentifier),
    };
    identifier_at(text, char_idx + 1)
        .map(|(name, _)| name)
        .ok_or(NavError::NoIdentifier)
}

// -- definition --------------------------------------------------------------

/// Light import map for the querying file: local name → path hint.
/// Rust `use`, Python `from/import`, TS/JS `import from`, Go (same package
/// handled structurally; cross-package via import path tails).
fn import_hints(ext: &str, lines: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<(String, String)>, local: &str, hint: &str| {
        let (local, hint) = (local.trim().to_string(), hint.trim().to_string());
        if !local.is_empty() && !hint.is_empty() && out.len() < 200 {
            out.push((local, hint));
        }
    };
    match ext {
        "rs" => {
            for line in lines.iter().take(500) {
                let t = line.trim();
                let Some(rest) = t.strip_prefix("use ") else {
                    continue;
                };
                let rest = rest.trim_end_matches(';').trim();
                if rest.starts_with('{') || rest == "*" || rest.is_empty() {
                    continue;
                }
                // use a::b::{C, D as E} | use a::b::C | use a::b::C as D
                if let Some((prefix, group)) = rest.split_once('{') {
                    let prefix = prefix.trim().trim_end_matches(':');
                    for item in group.trim_end_matches('}').split(',') {
                        let item = item.trim();
                        if item.is_empty() || item == "*" {
                            continue;
                        }
                        let (name, alias) = match item.split_once(" as ") {
                            Some((n, a)) => (n.trim(), a.trim()),
                            None => (item, item),
                        };
                        let segs: Vec<&str> =
                            prefix.split("::").chain(std::iter::once(name)).collect();
                        push(&mut out, alias, &segs.join("/"));
                    }
                } else {
                    let (path, alias) = match rest.split_once(" as ") {
                        Some((p, a)) => (p.trim(), a.trim()),
                        None => (rest, rest.rsplit("::").next().unwrap_or(rest)),
                    };
                    if alias == "*" || alias == "self" || alias == "super" || alias == "crate" {
                        continue;
                    }
                    push(&mut out, alias, &path.replace("::", "/"));
                }
                if out.len() >= 200 {
                    break;
                }
            }
        }
        "py" => {
            for line in lines.iter().take(500) {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("from ") {
                    // from a.b import C, D as E
                    let Some((mods, names)) = rest.split_once(" import ") else {
                        continue;
                    };
                    let base = mods.trim().trim_start_matches('.').replace('.', "/");
                    for item in names.split(',') {
                        let item = item.trim().trim_matches(|c| c == '(' || c == ')').trim();
                        if item.is_empty() || item == "*" {
                            continue;
                        }
                        let (name, alias) = match item.split_once(" as ") {
                            Some((n, a)) => (n.trim(), a.trim()),
                            None => (item, item),
                        };
                        push(&mut out, alias, &format!("{base}/{name}"));
                        push(&mut out, alias, &base);
                    }
                } else if let Some(rest) = t.strip_prefix("import ") {
                    for item in rest.split(',') {
                        let item = item.trim();
                        if item.is_empty() {
                            continue;
                        }
                        let (path, alias) = match item.split_once(" as ") {
                            Some((p, a)) => (p.trim(), a.trim()),
                            None => (item, item.split('.').next().unwrap_or(item)),
                        };
                        push(&mut out, alias, &path.replace('.', "/"));
                    }
                }
                if out.len() >= 200 {
                    break;
                }
            }
        }
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => {
            for line in lines.iter().take(500) {
                let t = line.trim();
                if !t.starts_with("import ") {
                    continue;
                }
                let Some((clause, from)) = t.split_once(" from ") else {
                    continue;
                };
                let path = from
                    .trim()
                    .trim_matches(|c| c == '\'' || c == '"' || c == ';')
                    .trim();
                let clause = clause.trim_start_matches("import ").trim();
                if clause.starts_with('*') {
                    // import * as N from 'p'
                    if let Some(alias) = clause.split(" as ").nth(1) {
                        push(&mut out, alias.trim(), path);
                    }
                    continue;
                }
                // Split default import from the {A, B as C} group.
                let (default, named) = match (clause.find('{'), clause.rfind('}')) {
                    (Some(b), Some(e)) if e > b => {
                        let named = &clause[b + 1..e];
                        let default = clause[..b].trim().trim_end_matches(',').trim();
                        (default, named)
                    }
                    _ => (clause, ""),
                };
                if !default.is_empty() {
                    push(&mut out, default, path);
                }
                for item in named.split(',') {
                    let item = item.trim();
                    if item.is_empty() {
                        continue;
                    }
                    let alias = match item.split_once(" as ") {
                        Some((_, a)) => a.trim(),
                        None => item,
                    };
                    push(&mut out, alias, path);
                }
                if out.len() >= 200 {
                    break;
                }
            }
        }
        "go" => {
            for line in lines.iter().take(200) {
                let t = line.trim().trim_matches('"').trim();
                // import "a/b/c" (also inside parenthesized blocks, linewise)
                let inner = t.trim_start_matches("import ").trim().trim_matches('"');
                if inner.starts_with('/') || inner.contains("://") || !inner.contains('/') {
                    continue;
                }
                let base = inner.rsplit('/').next().unwrap_or(inner);
                let alias = line
                    .split_whitespace()
                    .next()
                    .filter(|w| *w != "import" && !w.starts_with('"'))
                    .unwrap_or(base);
                push(&mut out, alias.trim_matches('"'), inner);
                if out.len() >= 200 {
                    break;
                }
            }
        }
        _ => {}
    }
    out
}

/// Resolve a relative import hint to candidate workspace-relative files
/// (extension variants); module-style hints resolve via suffix match.
fn resolve_hint_files(current_path: &str, hint: &str) -> Vec<String> {
    let hint = hint
        .trim()
        .trim_matches(|c| c == '\'' || c == '"' || c == ';');
    if hint.is_empty() {
        return Vec::new();
    }
    if hint.starts_with('.') {
        let dir = current_path
            .rfind('/')
            .map(|i| &current_path[..i])
            .unwrap_or("");
        let mut segs: Vec<&str> = if dir.is_empty() {
            vec![]
        } else {
            dir.split('/').collect()
        };
        for part in hint.split('/') {
            match part {
                "." | "" => {}
                ".." => {
                    segs.pop();
                }
                p => segs.push(p),
            }
        }
        let base = segs.join("/");
        return [
            base.clone(),
            format!("{base}.ts"),
            format!("{base}.tsx"),
            format!("{base}.js"),
            format!("{base}.jsx"),
            format!("{base}/index.ts"),
            format!("{base}/index.tsx"),
            format!("{base}/index.js"),
            format!("{base}.py"),
            format!("{base}/__init__.py"),
            format!("{base}.rs"),
        ]
        .into_iter()
        .collect();
    }
    Vec::new()
}

/// Module-style hints (rust a/b/C, python a/b, go a/b/c): candidate file
/// suffixes that would define the name. Both the full path and the parent
/// module file (the item may live directly in `a/b.rs` for `use a::b::C`).
fn module_suffixes(hint: &str) -> Vec<String> {
    let mut segs: Vec<&str> = hint
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    // Rust roots never appear in paths.
    if matches!(segs.first(), Some(&"crate" | &"self" | &"super")) {
        segs.remove(0);
    }
    if segs.is_empty() {
        return Vec::new();
    }
    let full = segs.join("/");
    let mut out = vec![
        format!("{full}.rs"),
        format!("{full}/mod.rs"),
        format!("{full}.py"),
        format!("{full}/__init__.py"),
        format!("{full}.go"),
        format!("{full}.ts"),
        format!("{full}.tsx"),
        format!("{full}.js"),
        full.clone(),
    ];
    if segs.len() > 1 {
        let parent = segs[..segs.len() - 1].join("/");
        out.extend([
            format!("{parent}.rs"),
            format!("{parent}/mod.rs"),
            format!("{parent}.py"),
            format!("{parent}/__init__.py"),
        ]);
    }
    out
}

fn same_dir(a: &str, b: &str) -> bool {
    let da = a.rfind('/').map(|i| &a[..i]).unwrap_or("");
    let db = b.rfind('/').map(|i| &b[..i]).unwrap_or("");
    da == db
}

/// Word before `ident` when written as `qual.ident` or `qual::ident`.
/// `line`/`col` are 1-based (col in UTF-16 units, like the API).
fn qualifier_at(lines: &[String], line: usize, col: usize) -> Option<String> {
    let text = lines.get(line.saturating_sub(1))?;
    let chars: Vec<char> = text.chars().collect();
    // UTF-16 col → char index, clamped into the line.
    let mut units = 0usize;
    let mut i = chars.len();
    for (k, c) in chars.iter().enumerate() {
        if units + 1 >= col {
            i = k;
            break;
        }
        units += c.len_utf16();
    }
    if i == 0 {
        return None;
    }
    // Walk left over the identifier to its start.
    let mut s = i.min(chars.len());
    while s > 0 && is_ident_char(chars[s - 1]) {
        s -= 1;
    }
    // Separator immediately before?
    let sep_len = if s >= 2 && chars[s - 2] == ':' && chars[s - 1] == ':' {
        2
    } else if s >= 1 && chars[s - 1] == '.' {
        1
    } else {
        return None;
    };
    let mut e = s - sep_len;
    while e > 0 && is_ident_char(chars[e - 1]) {
        e -= 1;
    }
    let q: String = chars[e..s - sep_len].iter().collect();
    (!q.is_empty()).then_some(q)
}

/// Ranked goto-definition candidates, best first, capped at 10.
pub fn definitions(
    nav: &Nav<'_>,
    path: &str,
    line: usize,
    col: usize,
    limit: usize,
) -> Result<Vec<Definition>, NavError> {
    let ident = identifier_at_pos(nav.root, path, line, col)?;
    let cands = nav.syms.by_name(&ident);
    if cands.is_empty() {
        return Ok(Vec::new());
    }
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    let (_, cur_lines) = read_lines(nav.root, path)?;
    let hints = import_hints(&ext, &cur_lines);
    // Qualifier at the query site (`pkg.Sym`, `Store::new`, `mod.func`):
    // package/module aliases resolve through the import map too.
    let qualifier = qualifier_at(&cur_lines, line, col);
    // Resolve hints to concrete files once.
    let mut hint_files: HashMap<String, Vec<String>> = HashMap::new();
    for (local, hint) in &hints {
        let files = resolve_hint_files(path, hint);
        let entry = hint_files.entry(local.clone()).or_default();
        entry.extend(files);
        entry.extend(module_suffixes(hint));
    }
    let mut scored: Vec<(u64, &ResolvedSymbol)> = Vec::new();
    for (sym, _) in &cands {
        let mut score: u64 = 0;
        if sym.path == path {
            score += 300;
        } else if same_dir(&sym.path, path) {
            score += 200;
        }
        // Import hits: the used name itself (from-imports) or its
        // qualifier (package/module aliases) resolving to this file.
        let mut hinted = hint_files.get(&ident);
        let qual_files;
        if hinted.is_none() {
            if let Some(q) = qualifier.as_deref().and_then(|q| hint_files.get(q)) {
                qual_files = q;
                hinted = Some(qual_files);
            }
        }
        if let Some(files) = hinted {
            if files
                .iter()
                .any(|f| sym.path == *f || sym.path.ends_with(&format!("/{f}")))
            {
                score += 100;
            }
            // Go: the import path tail names the package dir (or file).
            if ext == "go"
                && files.iter().any(|f| {
                    let last = f.rsplit('/').next().unwrap_or(f.as_str());
                    let dir = sym.path.rfind('/').map(|i| &sym.path[..i]).unwrap_or("");
                    let dname = dir.rsplit('/').next().unwrap_or(dir);
                    let stem = sym
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(sym.path.as_str())
                        .split('.')
                        .next()
                        .unwrap_or("");
                    dname == last || stem == last
                })
            {
                score += 100;
            }
        }
        // Prefer value definitions over re-export noise: shorter paths win.
        scored.push((score, sym));
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.path.len().cmp(&b.1.path.len()))
            .then_with(|| a.1.path.cmp(&b.1.path))
            .then_with(|| a.1.line.cmp(&b.1.line))
    });
    Ok(scored
        .into_iter()
        .take(limit.clamp(1, 10))
        .map(|(_, s)| Definition {
            path: s.path.clone(),
            line: s.line,
            end_line: s.end_line,
            kind: s.kind.as_str(),
            name: s.name.clone(),
            source: "treesitter",
        })
        .collect())
}

// -- references --------------------------------------------------------------

fn utf16_col_to_byte(line: &str, col_units: usize) -> Option<usize> {
    if col_units == 0 {
        return None;
    }
    let mut units = 0usize;
    for (byte, c) in line.char_indices() {
        if units + 1 >= col_units {
            return Some(byte);
        }
        units += c.len_utf16();
    }
    (units + 1 >= col_units).then_some(line.len())
}

fn line_start_bytes(bytes: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

#[cfg(feature = "tree-sitter")]
fn is_identifier_use(lang: &tree_sitter::Language, bytes: &[u8], byte_off: usize) -> bool {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(lang).is_err() {
        return true;
    }
    let Some(tree) = parser.parse(bytes, None) else {
        return true;
    };
    let root = tree.root_node();
    let Some(mut node) = root.descendant_for_byte_range(byte_off, byte_off + 1) else {
        return true;
    };
    // Descend to the tightest named node at the offset.
    loop {
        let mut advanced = false;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named()
                && child.start_byte() <= byte_off
                && byte_off < child.end_byte()
                && (child.end_byte() - child.start_byte()) < (node.end_byte() - node.start_byte())
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
    // Comments and string literals are not identifier uses.
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

#[cfg(feature = "tree-sitter")]
fn language_for_refs(ext: &str) -> Option<tree_sitter::Language> {
    match ext {
        #[cfg(feature = "ts-rust")]
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        #[cfg(feature = "ts-go")]
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        #[cfg(feature = "ts-typescript")]
        "ts" | "mts" | "cts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        #[cfg(feature = "ts-typescript")]
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        #[cfg(feature = "ts-javascript")]
        "js" | "jsx" | "mjs" | "cjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        #[cfg(feature = "ts-python")]
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        #[cfg(feature = "ts-java")]
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        #[cfg(feature = "ts-c")]
        "c" => Some(tree_sitter_c::LANGUAGE.into()),
        #[cfg(feature = "ts-cpp")]
        "h" | "hpp" | "cc" | "cxx" | "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),
        #[cfg(feature = "ts-csharp")]
        "cs" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        #[cfg(feature = "ts-ruby")]
        "rb" => Some(tree_sitter_ruby::LANGUAGE.into()),
        #[cfg(feature = "ts-php")]
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),
        _ => None,
    }
}

/// Word-boundary references to the identifier at (path, line, col),
/// filtered to real identifier nodes for supported languages.
pub fn references(
    nav: &Nav<'_>,
    path: &str,
    line: usize,
    col: usize,
    limit: usize,
) -> Result<(Vec<Reference>, bool), NavError> {
    let ident = identifier_at_pos(nav.root, path, line, col)?;
    if ident.len() > 128 {
        return Err(NavError::BadPosition("identifier too long".into()));
    }
    let limit = limit.clamp(1, MAX_REFERENCES);
    let mut q = crate::scan::Query::literal(ident.clone());
    q.word = true;
    q.case = crate::scan::Case::Sensitive;
    q.max_files = 200;
    q.max_per_file = 50;
    q.max_file_bytes = MAX_FILE_BYTES;
    let stop = std::sync::atomic::AtomicBool::new(false);
    let resp = crate::scan::search(nav.snap, nav.root, &q, &stop).map_err(NavError::BadPosition)?;
    let mut out = Vec::new();
    let mut truncated = resp.truncated;
    // Per-file parse cache for identifier filtering.
    type ParsedFile = Option<(Vec<u8>, Vec<usize>)>;
    let mut parsed: HashMap<String, ParsedFile> = HashMap::new();
    'files: for f in &resp.files {
        let ext = f.path.rsplit('.').next().unwrap_or("").to_lowercase();
        #[cfg(feature = "tree-sitter")]
        let lang = language_for_refs(&ext);
        #[cfg(not(feature = "tree-sitter"))]
        let lang: Option<()> = None;
        for h in &f.hits {
            if out.len() >= limit {
                truncated = true;
                break 'files;
            }
            let mut col_out = None;
            #[cfg(feature = "tree-sitter")]
            if let Some(lang) = lang.as_ref() {
                let entry = parsed.entry(f.path.clone()).or_insert_with(|| {
                    let abs = nav.root.join(f.path.trim_start_matches('/'));
                    let bytes = std::fs::read(&abs).ok()?;
                    if bytes.len() as u64 > MAX_FILE_BYTES {
                        return None;
                    }
                    let starts = line_start_bytes(&bytes);
                    Some((bytes, starts))
                });
                if let Some((bytes, starts)) = entry {
                    if h.cut_start {
                        // Window cut the line start: cannot verify, keep it.
                    } else if let Some(&(s, _)) = h.ranges.first() {
                        let text = &h.text;
                        if let Some(rel) = utf16_col_to_byte(text, s + 1) {
                            let Some(line_off) = starts.get(h.line.saturating_sub(1)).copied()
                            else {
                                continue;
                            };
                            let off = line_off + rel.min(bytes.len().saturating_sub(line_off));
                            if !is_identifier_use(lang, bytes, off) {
                                continue;
                            }
                            col_out = Some(s + 1);
                        }
                    }
                }
            }
            #[cfg(not(feature = "tree-sitter"))]
            let _ = (&ext, &lang);
            out.push(Reference {
                path: f.path.clone(),
                line: h.line,
                col: col_out,
            });
        }
    }
    Ok((out, truncated))
}

// -- hover -------------------------------------------------------------------

fn comment_prefixes(ext: &str) -> &'static [&'static str] {
    match ext {
        "rs" | "go" | "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "java"
        | "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "cs" | "php" => &["//"],
        "py" | "rb" => &["#"],
        _ => &[],
    }
}

/// Signature line(s) + contiguous doc comment above the definition.
pub fn hover(nav: &Nav<'_>, path: &str, line: usize, col: usize) -> Result<Hover, NavError> {
    let defs = definitions(nav, path, line, col, 1)?;
    let d = defs.into_iter().next().ok_or(NavError::NoIdentifier)?;
    let (_, lines) = read_lines(nav.root, &d.path)?;
    let ext = d.path.rsplit('.').next().unwrap_or("").to_lowercase();
    // Signature: first line, extended while delimiters unbalance (≤4 lines).
    let mut sig = String::new();
    let mut depth = 0i32;
    for l in lines.iter().skip(d.line as usize - 1).take(4) {
        if !sig.is_empty() {
            sig.push(' ');
        }
        sig.push_str(l.trim());
        depth += l
            .chars()
            .filter(|c| *c == '(' || *c == '{' || *c == '[')
            .count() as i32;
        depth -= l
            .chars()
            .filter(|c| *c == ')' || *c == '}' || *c == ']')
            .count() as i32;
        if depth <= 0 {
            break;
        }
    }
    if sig.len() > 500 {
        sig.truncate(500);
    }
    // Doc: contiguous line comments directly above.
    let prefixes = comment_prefixes(&ext);
    let mut doc: Vec<String> = Vec::new();
    if !prefixes.is_empty() && d.line >= 2 {
        for l in lines.iter().take(d.line as usize - 1).rev() {
            let t = l.trim();
            let Some(body) = prefixes.iter().find_map(|p| t.strip_prefix(p)) else {
                break;
            };
            let body = body
                .strip_prefix('/')
                .or_else(|| body.strip_prefix('!'))
                .unwrap_or(body);
            doc.push(body.trim().to_string());
            if doc.len() >= 20 {
                break;
            }
        }
        doc.reverse();
    }
    Ok(Hover {
        name: d.name,
        kind: d.kind,
        path: d.path,
        line: d.line,
        signature: sig,
        doc: if doc.is_empty() {
            None
        } else {
            Some(doc.join("\n"))
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_positions() {
        assert_eq!(identifier_at("fn main() {}", 4), Some(("main".into(), 4)));
        assert_eq!(identifier_at("fn main() {}", 1), Some(("fn".into(), 1)));
        assert_eq!(identifier_at("  ", 1), None);
        assert_eq!(identifier_at("a+b", 2), None);
        assert_eq!(identifier_at("", 1), None);
    }

    #[test]
    fn smoke_ts_class() {
        use crate::symbols::outline_ts;
        let syms = outline_ts("ts", "export class Store {\n    run(): void {}\n}\n").unwrap();
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Store"), "{names:?}");
        assert!(names.contains(&"run"), "{names:?}");
    }
}

#[cfg(test)]
mod golden_tests {
    use super::{definitions, Nav};
    use crate::fileindex::FileSnapshot;
    use crate::symindex::{SymbolIndex, SymbolState};
    use std::sync::Arc;

    struct Fixture {
        _dir: tempfile::TempDir,
        snap: Arc<FileSnapshot>,
        syms: Arc<SymbolIndex>,
    }

    fn fixture(files: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        for (p, content) in files {
            let full = dir.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, content).unwrap();
        }
        let mut items: Vec<(String, u64, i64)> = files
            .iter()
            .map(|(p, _)| {
                let m = std::fs::metadata(dir.path().join(p)).unwrap();
                (
                    p.to_string(),
                    m.len(),
                    m.modified()
                        .unwrap()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64,
                )
            })
            .collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let snap = Arc::new(FileSnapshot {
            lower: items.iter().map(|(p, _, _)| p.to_lowercase()).collect(),
            base_off: items
                .iter()
                .map(|(p, _, _)| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
                .collect(),
            sizes: items.iter().map(|(_, s, _)| *s).collect(),
            mtimes: items.iter().map(|(_, _, m)| *m).collect(),
            paths: items.into_iter().map(|(p, _, _)| p).collect(),
            generation: 7,
        });
        let syms = SymbolIndex::new(dir.path().join("cache"), dir.path().to_path_buf());
        syms.ensure_built(&snap);
        let t0 = std::time::Instant::now();
        while syms.state() != SymbolState::Ready {
            assert!(
                t0.elapsed() < std::time::Duration::from_secs(15),
                "index build stall"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Fixture {
            _dir: dir,
            snap,
            syms,
        }
    }

    /// Definition top-1 accuracy golden set (B6 acceptance: ≥ 80 % on 50
    /// cases across Rust/Go/TS/Python). Columns are 1-based.
    #[test]
    fn golden_definition_top1() {
        let fx = fixture(&[
            ("rs/src/main.rs", "use crate::util::helper;\nuse crate::util::Config;\nuse store::Store;\n\nfn main() {\n    helper();\n    let _c = Config;\n    let s = Store::new();\n    s.run();\n    Config::load();\n}\n"),
            ("rs/src/util.rs", "pub fn helper() {}\npub struct Config;\nimpl Config {\n    pub fn load() {}\n}\n"),
            ("rs/src/store.rs", "pub struct Store;\nimpl Store {\n    pub fn new() {}\n    pub fn run(&self) {}\n}\n"),
            ("rs/src/extra.rs", "pub fn helper() {}\n"),
            ("go/cmd/main.go", "package main\n\nimport \"example.com/proj/store\"\n\nfunc main() {\n    db := store.Open()\n    db.Query()\n}\n"),
            ("go/store/store.go", "package store\n\ntype DB struct{}\n\nfunc Open() *DB { return nil }\n\nfunc (d *DB) Query() {}\n"),
            ("go/other/other.go", "package other\n\nfunc Open() {}\n"),
            ("ts/src/util.ts", "export function helper(): void {}\nexport interface Config { x: number }\nexport class Store {\n    run(): void {}\n}\n"),
            ("ts/src/other.ts", "export function helper(): void {}\n"),
            ("ts/src/main.ts", "import { helper, Config, Store } from './util';\n\nhelper();\nconst c: Config | null = null;\nconst s = new Store();\ns.run();\nfunction local() {}\nlocal();\n"),
            ("py/util.py", "def helper():\n    pass\nclass Config:\n    def load(self):\n        pass\n"),
            ("py/other.py", "def helper():\n    pass\ndef other_helper():\n    pass\n"),
            ("py/main.py", "from util import helper, Config\nfrom other import other_helper\n\nhelper()\nc = Config()\nother_helper()\n"),
        ]);
        let nav = Nav {
            root: fx._dir.path(),
            snap: &fx.snap,
            syms: &fx.syms,
        };
        // (query path, line, col, expected path, expected line)
        let cases: &[(&str, usize, usize, &str, u32)] = &[
            // Rust: imports beat the extra.rs decoy; same-file wins at home.
            ("rs/src/main.rs", 6, 7, "rs/src/util.rs", 1),
            ("rs/src/main.rs", 7, 14, "rs/src/util.rs", 2),
            ("rs/src/main.rs", 8, 13, "rs/src/store.rs", 1),
            ("rs/src/main.rs", 8, 20, "rs/src/store.rs", 3),
            ("rs/src/main.rs", 9, 7, "rs/src/store.rs", 4),
            ("rs/src/main.rs", 10, 13, "rs/src/util.rs", 4),
            ("rs/src/main.rs", 10, 5, "rs/src/util.rs", 2),
            ("rs/src/main.rs", 5, 4, "rs/src/main.rs", 5),
            ("rs/src/main.rs", 1, 18, "rs/src/util.rs", 1),
            ("rs/src/main.rs", 2, 18, "rs/src/util.rs", 2),
            ("rs/src/main.rs", 3, 12, "rs/src/store.rs", 1),
            ("rs/src/util.rs", 1, 8, "rs/src/util.rs", 1),
            ("rs/src/util.rs", 2, 12, "rs/src/util.rs", 2),
            ("rs/src/util.rs", 4, 12, "rs/src/util.rs", 4),
            ("rs/src/store.rs", 1, 12, "rs/src/store.rs", 1),
            ("rs/src/store.rs", 2, 6, "rs/src/store.rs", 1),
            ("rs/src/store.rs", 3, 12, "rs/src/store.rs", 3),
            ("rs/src/store.rs", 4, 12, "rs/src/store.rs", 4),
            ("rs/src/extra.rs", 1, 8, "rs/src/extra.rs", 1),
            // Go: cross-package import beats the other/ decoy.
            ("go/cmd/main.go", 6, 17, "go/store/store.go", 5),
            ("go/cmd/main.go", 7, 8, "go/store/store.go", 7),
            ("go/cmd/main.go", 5, 6, "go/cmd/main.go", 5),
            ("go/store/store.go", 3, 6, "go/store/store.go", 3),
            ("go/store/store.go", 5, 6, "go/store/store.go", 5),
            ("go/store/store.go", 5, 14, "go/store/store.go", 3),
            ("go/store/store.go", 7, 14, "go/store/store.go", 7),
            ("go/other/other.go", 3, 6, "go/other/other.go", 3),
            // TypeScript: relative import beats the deep decoy.
            ("ts/src/main.ts", 3, 1, "ts/src/util.ts", 1),
            ("ts/src/main.ts", 4, 10, "ts/src/util.ts", 2),
            ("ts/src/main.ts", 5, 15, "ts/src/util.ts", 3),
            ("ts/src/main.ts", 6, 3, "ts/src/util.ts", 4),
            ("ts/src/main.ts", 7, 10, "ts/src/main.ts", 7),
            ("ts/src/main.ts", 8, 1, "ts/src/main.ts", 7),
            ("ts/src/main.ts", 1, 10, "ts/src/util.ts", 1),
            ("ts/src/main.ts", 1, 26, "ts/src/util.ts", 3),
            ("ts/src/util.ts", 1, 17, "ts/src/util.ts", 1),
            ("ts/src/util.ts", 2, 18, "ts/src/util.ts", 2),
            ("ts/src/util.ts", 3, 14, "ts/src/util.ts", 3),
            ("ts/src/util.ts", 4, 5, "ts/src/util.ts", 4),
            ("ts/src/other.ts", 1, 17, "ts/src/other.ts", 1),
            // Python: from-imports beat the other.py decoy.
            ("py/main.py", 4, 1, "py/util.py", 1),
            ("py/main.py", 5, 5, "py/util.py", 3),
            ("py/main.py", 6, 1, "py/other.py", 3),
            ("py/main.py", 1, 26, "py/util.py", 3),
            ("py/main.py", 1, 20, "py/util.py", 1),
            ("py/util.py", 1, 5, "py/util.py", 1),
            ("py/util.py", 3, 7, "py/util.py", 3),
            ("py/util.py", 4, 9, "py/util.py", 4),
            ("py/other.py", 1, 5, "py/other.py", 1),
            ("py/other.py", 3, 5, "py/other.py", 3),
        ];
        assert!(cases.len() >= 50, "golden set shrank: {}", cases.len());
        let mut top1 = 0usize;
        let mut misses = Vec::new();
        for (path, line, col, exp_path, exp_line) in cases {
            match definitions(&nav, path, *line, *col, 10) {
                Ok(defs) => {
                    let hit = defs.first().map(|d| (d.path.as_str(), d.line));
                    if hit == Some((*exp_path, *exp_line)) {
                        top1 += 1;
                    } else {
                        misses.push((*path, *line, *col, hit.map(|h| h.0.to_string())));
                    }
                }
                Err(e) => misses.push((*path, *line, *col, Some(format!("ERR {e}")))),
            }
        }
        let pct = top1 * 100 / cases.len();
        assert!(
            pct >= 80,
            "top-1 {top1}/{} ({pct}%): {misses:?}",
            cases.len()
        );
    }
}
