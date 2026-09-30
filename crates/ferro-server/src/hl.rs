//! Class-based syntax highlighting per API.md § 11.
//! Output is only text + `<span class="t-…">`. Scope mapping is BACKEND.md
//! Appendix A. Windows carry `exact`: approximation (400-line context) vs a
//! background exact pass that emits an `hl` event.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

static SYNTAX: std::sync::OnceLock<SyntaxSet> = std::sync::OnceLock::new();

pub fn syntax_set() -> &'static SyntaxSet {
    SYNTAX.get_or_init(SyntaxSet::load_defaults_newlines)
}

pub fn find_syntax<'s>(
    ss: &'s SyntaxSet,
    language: Option<&str>,
    path: Option<&Path>,
) -> &'s SyntaxReference {
    if let Some(p) = path {
        if let Ok(Some(s)) = ss.find_syntax_for_file(p) {
            return s;
        }
    }
    if let Some(l) = language {
        if let Some(s) = ss
            .find_syntax_by_extension(l)
            .or_else(|| ss.find_syntax_by_name(l))
        {
            return s;
        }
        if let Some(s) = ss.find_syntax_by_token(l) {
            return s;
        }
    }
    ss.find_syntax_plain_text()
}

/// Map a scope stack (innermost last) to an Appendix A class.
/// Walks innermost→outermost; `punctuation.definition/section` scopes inherit.
pub fn class_for(stack: &[Scope]) -> Option<&'static str> {
    for scope in stack.iter().rev() {
        let s = scope.build_string();
        if s.starts_with("punctuation.definition.") || s.starts_with("punctuation.section.") {
            continue;
        }
        if s.starts_with("comment.block.documentation")
            || s.starts_with("comment.line.documentation")
        {
            return Some("t-cd");
        }
        if s.starts_with("comment") {
            return Some("t-c");
        }
        if s.starts_with("string.regexp") {
            return Some("t-re");
        }
        if s.starts_with("constant.character.escape") {
            return Some("t-se");
        }
        if s.starts_with("string") || s.starts_with("constant.character") {
            return Some("t-s");
        }
        if s.starts_with("constant.numeric") {
            return Some("t-n");
        }
        if s.starts_with("constant.language") || s.starts_with("constant.other") {
            return Some("t-b");
        }
        if s.starts_with("keyword.operator") {
            return Some("t-o");
        }
        if s.starts_with("keyword")
            || s.starts_with("storage.type")
            || s.starts_with("storage.modifier")
        {
            return Some("t-k");
        }
        if s.starts_with("entity.name.function.macro")
            || s.starts_with("entity.name.function.decorator")
            || s.starts_with("meta.annotation")
            || s.starts_with("meta.attribute")
            || s.starts_with("support.function.macro")
        {
            return Some("t-m");
        }
        if s.starts_with("entity.name.function") {
            return Some("t-fd");
        }
        if s.starts_with("support.function")
            || s.starts_with("variable.function")
            || s.starts_with("meta.function-call")
        {
            return Some("t-f");
        }
        if s.starts_with("support.type.builtin")
            || s.starts_with("storage.type.primitive")
            || s.starts_with("support.type.primitive")
        {
            return Some("t-tb");
        }
        if s.starts_with("entity.name.type")
            || s.starts_with("entity.name.class")
            || s.starts_with("entity.name.struct")
            || s.starts_with("entity.name.enum")
            || s.starts_with("entity.name.interface")
            || s.starts_with("entity.name.trait")
            || s.starts_with("entity.other.inherited-class")
            || s.starts_with("support.type")
            || s.starts_with("support.class")
        {
            return Some("t-t");
        }
        if s.starts_with("entity.name.namespace")
            || s.starts_with("entity.name.module")
            || s.starts_with("support.module")
        {
            return Some("t-ns");
        }
        if s.starts_with("variable.parameter") {
            return Some("t-vp");
        }
        if s.starts_with("variable.language") {
            return Some("t-vb");
        }
        if s.starts_with("variable.other.member")
            || s.starts_with("variable.other.property")
            || s.starts_with("support.type.property-name")
            || s.starts_with("meta.property-name")
        {
            return Some("t-pr");
        }
        if s.starts_with("variable") {
            return Some("t-v");
        }
        if s.starts_with("entity.name.tag") {
            return Some("t-tg");
        }
        if s.starts_with("entity.other.attribute-name") {
            return Some("t-at");
        }
        if s.starts_with("markup.heading") {
            return Some("t-h");
        }
        if s.starts_with("markup.italic") {
            return Some("t-em");
        }
        if s.starts_with("markup.bold") {
            return Some("t-st");
        }
        if s.starts_with("markup.underline.link")
            || s.starts_with("markup.link")
            || s.starts_with("string.other.link")
        {
            return Some("t-l");
        }
        if s.starts_with("markup.inserted") {
            return Some("t-ins");
        }
        if s.starts_with("markup.deleted") {
            return Some("t-del");
        }
        if s.starts_with("invalid") {
            return Some("t-err");
        }
        if s.starts_with("punctuation") {
            return Some("t-p");
        }
        // Unmapped scope: keep walking outward.
    }
    None
}

fn esc_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

/// Highlight one line given the current scope stack, advancing both.
/// Emits merged `<span class>` runs; `stack` ends holding the next line's state.
pub fn highlight_line(
    line: &str,
    ss: &SyntaxSet,
    parse: &mut ParseState,
    stack: &mut ScopeStack,
) -> String {
    // Output covers the line's content only, never its terminator (`\n`, `\r\n`):
    // scopes that end at the newline (line and doc comments) must not pull it in.
    let content = line.trim_end_matches('\n').trim_end_matches('\r');
    let end = content.len();
    let with_nl;
    let src = if line.ends_with('\n') {
        line
    } else {
        with_nl = format!("{line}\n");
        &with_nl
    };
    let ops = match parse.parse_line(src, ss) {
        Ok(ops) => ops,
        Err(_) => return escape_text(content),
    };
    let mut out = String::with_capacity(line.len() + 32);
    let mut pos = 0usize;
    let mut open: Option<&'static str> = None;
    let mut run = String::new();
    let flush = |out: &mut String, run: &mut String, open: &mut Option<&'static str>| {
        if run.is_empty() {
            return;
        }
        match open {
            Some(c) => {
                out.push_str(&format!("<span class=\"{c}\">"));
                esc_into(out, run);
                out.push_str("</span>");
            }
            None => esc_into(out, run),
        }
        run.clear();
    };
    for (off, op) in &ops {
        let off = (*off).min(src.len());
        if off > pos {
            let cls = class_for(stack.as_slice());
            if cls != open {
                flush(&mut out, &mut run, &mut open);
                open = cls;
            }
            // Clamp to the content: the terminator is not part of the line.
            let (a, b) = (pos.min(end), off.min(end));
            run.push_str(&src[a..b]);
            pos = off;
        }
        let _ = stack.apply(op);
    }
    if pos < end {
        let cls = class_for(stack.as_slice());
        if cls != open {
            flush(&mut out, &mut run, &mut open);
            open = cls;
        }
        run.push_str(&src[pos..end]);
    }
    flush(&mut out, &mut run, &mut open);
    out
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    esc_into(&mut out, s);
    out
}

#[derive(Debug, Clone)]
pub struct HlLine {
    pub n: usize,
    pub html: String,
}

#[derive(Debug, Clone)]
pub struct HlWindow {
    pub syntax: String,
    pub total: usize,
    pub start: usize,
    pub lines: Vec<HlLine>,
    pub exact: bool,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct FileVer {
    path: PathBuf,
    mtime_ms: u128,
    size: u64,
}

fn file_ver(path: &Path) -> Option<(FileVer, u64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime_ms = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    Some((
        FileVer {
            path: path.to_path_buf(),
            mtime_ms,
            size: md.len(),
        },
        mtime_ms as u64,
    ))
}

/// Windowed highlighter with exact-pass cache (B1).
/// Windows LRU is byte-budgeted (64 MiB); exact passes are cached per file
/// version (8 files) and announced through `on_exact` for `hl` events.
/// Cloning is cheap and shares every cache: callers clone it out of the
/// workspace mutex instead of holding that lock while highlighting.
#[derive(Clone)]
pub struct Highlighter {
    windows: Arc<parking_lot::Mutex<lru::LruCache<String, HlWindow>>>,
    windows_bytes: Arc<parking_lot::Mutex<usize>>,
    exact: Arc<parking_lot::Mutex<HashMap<FileVer, Arc<Vec<String>>>>>,
    in_flight: Arc<parking_lot::Mutex<std::collections::HashSet<FileVer>>>,
    on_exact: Option<Arc<dyn Fn(String, u64) + Send + Sync>>,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            windows: Arc::new(parking_lot::Mutex::new(lru::LruCache::unbounded())),
            windows_bytes: Arc::new(parking_lot::Mutex::new(0)),
            exact: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            in_flight: Arc::new(parking_lot::Mutex::new(std::collections::HashSet::new())),
            on_exact: None,
        }
    }

    pub fn on_exact(mut self, f: Arc<dyn Fn(String, u64) + Send + Sync>) -> Self {
        self.on_exact = Some(f);
        self
    }

    pub fn set_on_exact(&mut self, f: Arc<dyn Fn(String, u64) + Send + Sync>) {
        self.on_exact = Some(f);
    }

    fn window_key(ver: &FileVer, start: usize, count: usize) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            ver.path.display(),
            ver.mtime_ms,
            ver.size,
            start,
            count
        )
    }

    fn cache_window(&self, key: String, w: HlWindow) {
        let bytes: usize = w.lines.iter().map(|l| l.html.len()).sum();
        let mut used = self.windows_bytes.lock();
        let mut cache = self.windows.lock();
        *used += bytes;
        cache.push(key, w);
        while *used > 64 * 1024 * 1024 {
            match cache.pop_lru() {
                Some((_, old)) => {
                    *used = used.saturating_sub(old.lines.iter().map(|l| l.html.len()).sum());
                }
                None => break,
            }
        }
    }

    /// Synchronous exact pass over the whole file (used by the background
    /// job and by tests). Skipped past 50 MiB / 2M lines.
    pub fn exact_pass_sync(path: &Path, syntax_name: &str) -> Option<Arc<Vec<String>>> {
        let text = std::fs::read(path).ok()?;
        if text.len() > 50 * 1024 * 1024 {
            return None;
        }
        let text = String::from_utf8_lossy(&text).into_owned();
        let raws: Vec<&str> = text.split('\n').collect();
        let n = if text.ends_with('\n') {
            raws.len().saturating_sub(1)
        } else {
            raws.len()
        };
        if n > 2_000_000 {
            return None;
        }
        let ss = syntax_set();
        let syntax = ss
            .find_syntax_by_name(syntax_name)
            .unwrap_or_else(|| ss.find_syntax_plain_text());
        let mut parse = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut out = Vec::with_capacity(n.min(100_000));
        for raw in raws.into_iter().take(n) {
            out.push(highlight_line(raw, ss, &mut parse, &mut stack));
        }
        Some(Arc::new(out))
    }
    /// Highlight `count` lines from 0-based `start` of a file with `total` lines.
    /// Cache hits cost nothing; a miss replays up to 400 lines of context before
    /// the window, fetched through `load(first, n)` (0-based first line, line
    /// count, content without terminators) so only those lines are read (O(window)).
    #[allow(clippy::too_many_arguments)]
    pub fn window(
        &self,
        path: &Path,
        rel: &str,
        start: usize,
        count: usize,
        total: usize,
        language: Option<&str>,
        load: impl FnOnce(usize, usize) -> Option<Vec<String>>,
    ) -> Option<HlWindow> {
        let ss = syntax_set();
        let syntax = find_syntax(ss, language, Some(path));
        let (ver, mtime_ms) = file_ver(path)?;
        if ver.size > 50 * 1024 * 1024 {
            return None;
        }
        let key = Self::window_key(&ver, start, count);
        if let Some(w) = self.windows.lock().get(&key).cloned() {
            return Some(w);
        }
        // Exact cache hit?
        if let Some(full) = self.exact.lock().get(&ver).cloned() {
            let total = full.len();
            let lines = full
                .iter()
                .skip(start)
                .take(count)
                .enumerate()
                .map(|(i, html)| HlLine {
                    n: start + i + 1,
                    html: html.clone(),
                })
                .collect();
            let w = HlWindow {
                syntax: syntax.name.clone(),
                total,
                start,
                lines,
                exact: true,
            };
            self.cache_window(key, w.clone());
            return Some(w);
        }
        // Approximate: replay from up to 400 lines before the window.
        let from = start.saturating_sub(400);
        let raws = load(from, start - from + count)?;
        let mut parse = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut lines = Vec::with_capacity(count);
        for (i, raw) in raws.iter().enumerate() {
            let html = highlight_line(raw, ss, &mut parse, &mut stack);
            if from + i >= start {
                lines.push(HlLine {
                    n: from + i + 1,
                    html,
                });
            }
        }
        let w = HlWindow {
            syntax: syntax.name.clone(),
            total,
            start,
            lines,
            exact: from == 0,
        };
        self.cache_window(key, w.clone());
        // Kick one background exact pass per version.
        if total <= 2_000_000 && self.in_flight.lock().insert(ver.clone()) {
            let exact = self.exact.clone();
            let inflight = self.in_flight.clone();
            let on_exact = self.on_exact.clone();
            let path = path.to_path_buf();
            let rel = rel.to_string();
            let syntax_name = syntax.name.clone();
            tokio::spawn(async move {
                let full =
                    tokio::task::spawn_blocking(move || Self::exact_pass_sync(&path, &syntax_name))
                        .await
                        .ok()
                        .flatten();
                inflight.lock().remove(&ver);
                if let Some(full) = full {
                    exact.lock().insert(ver.clone(), full);
                    while exact.lock().len() > 8 {
                        let k = exact.lock().keys().next().cloned();
                        match k {
                            Some(k) => {
                                exact.lock().remove(&k);
                            }
                            None => break,
                        }
                    }
                    if let Some(cb) = on_exact {
                        cb(rel, mtime_ms);
                    }
                }
            });
        }
        Some(w)
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syntect::parsing::ScopeStackOp;

    #[test]
    fn python_comment_bleed_regression() {
        // D15: a `#` comment must not swallow the next line.
        let ss = syntax_set();
        let syntax = ss.find_syntax_by_extension("py").unwrap();
        let mut parse = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let l1 = highlight_line("x = 1  # comment", ss, &mut parse, &mut stack);
        let l2 = highlight_line("y = \"s\"", ss, &mut parse, &mut stack);
        assert!(l1.contains("t-c"), "{l1}");
        assert!(!l2.contains("t-c"), "{l2}");
        assert!(l2.contains("t-s"), "{l2}");
    }

    #[test]
    fn classes_are_closed_set() {
        let ss = syntax_set();
        let syntax = ss.find_syntax_by_extension("rs").unwrap();
        let mut parse = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let html = highlight_line("pub fn main() { // hi", ss, &mut parse, &mut stack);
        assert!(!html.contains("style="), "{html}");
        assert!(html.contains("class=\"t-k\""), "{html}");
        assert!(html.contains("class=\"t-c\""), "{html}");
        let _ = ScopeStackOp::Noop;
    }

    /// B1 acceptance: 2,000-line block comment, then code. A window at line
    /// 2,500 served from context is approximate; the exact pass is correct.
    #[tokio::test]
    async fn exact_pass_after_long_comment() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.py");
        let mut src = String::from("\"\"\"\n");
        for i in 0..2000 {
            src.push_str(&format!("comment line {i}\n"));
        }
        src.push_str("\"\"\"\n");
        for i in 0..600 {
            src.push_str(&format!("x{i} = \"s{i}\"\n"));
        }
        std::fs::write(&p, &src).unwrap();
        let h = Highlighter::new();
        // Approximation from 400 lines of context lands inside the comment.
        let idx = crate::lines::LineIndex::new();
        let total = idx.total_lines(&p).unwrap();
        let mut asked = None;
        let w = h
            .window(&p, "big.py", 2500, 3, total, Some("py"), |first, n| {
                asked = Some((first, n));
                let (win, _) = crate::lines::read_window_bytes_with(&idx, &p, first + 1, n).ok()?;
                Some(
                    win.into_iter()
                        .map(|(_, b)| String::from_utf8_lossy(&b).into_owned())
                        .collect(),
                )
            })
            .unwrap();
        assert!(!w.exact);
        assert_eq!(w.total, 2602);
        assert_eq!(
            w.lines.iter().map(|l| l.n).collect::<Vec<_>>(),
            vec![2501, 2502, 2503]
        );
        // Only the 400-line lookback plus the window is read, not the file.
        assert_eq!(asked, Some((2100, 403)));
        // Exact pass: code lines are strings, not comments.
        let full = Highlighter::exact_pass_sync(&p, "Python").unwrap();
        assert_eq!(full.len(), 2602);
        assert!(!full[2500].contains("t-c"), "{}", full[2500]);
        assert!(full[2500].contains("t-s"), "{}", full[2500]);
    }

    /// Review fix: scopes that end at the newline (doc and line comments) must
    /// not pull the terminator into the HTML, and CRLF must not leave `\r`.
    #[test]
    fn line_html_never_contains_the_terminator() {
        let ss = syntax_set();
        for (ext, line) in [
            ("rs", "//! Ferro HTTP boundary"),
            ("rs", "let x = 1; // trailing"),
            ("py", "x = 1  # comment"),
            ("rs", "let s = \"crlf\";\r"),
            ("rs", "/// doc\r\n"),
        ] {
            let syntax = ss.find_syntax_by_extension(ext).unwrap();
            let mut parse = ParseState::new(syntax);
            let mut stack = ScopeStack::new();
            let html = highlight_line(line, ss, &mut parse, &mut stack);
            assert!(
                !html.contains('\n') && !html.contains('\r'),
                "{line:?} -> {html:?}"
            );
            assert!(html.contains("t-"), "{html}");
        }
    }
}
