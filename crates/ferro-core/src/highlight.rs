//! Windowed syntax highlight: 1000-line window + 400 lines context for state.
//! syntect HighlightLines carried through context, per-line HTML output.
//! LRU of 128 windows (~25MB) keyed by rel:start:count:mtime.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::html::{append_highlighted_html_for_styled_line, IncludeBackground};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::index::Index;

#[derive(Debug, Clone, Serialize)]
pub struct HlLine {
    pub n: usize,
    pub html: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HlWindow {
    pub syntax: String,
    pub total: usize,
    pub start: usize,
    pub lines: Vec<HlLine>,
}

static SYNTAX: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();
static CACHE: OnceLock<Mutex<lru::LruCache<String, HlWindow>>> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    THEMES.get_or_init(ThemeSet::load_defaults)
}

fn cache() -> &'static Mutex<lru::LruCache<String, HlWindow>> {
    CACHE.get_or_init(|| Mutex::new(lru::LruCache::new(128.try_into().unwrap())))
}

pub fn css() -> String {
    String::new()
}

fn find_syntax<'s>(ss: &'s SyntaxSet, path: &Path) -> &'s SyntaxReference {
    if let Ok(Some(s)) = ss.find_syntax_for_file(path) {
        return s;
    }
    ss.find_syntax_plain_text()
}

fn mtime_ns(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

impl Index {
    pub fn highlight_window(&self, rel: &str, start: usize, count: usize) -> Option<HlWindow> {
        let count = count.clamp(1, 1000);
        let abs = self.safe_join(rel)?;
        let mt = mtime_ns(&abs);
        let key = format!("{rel}:{start}:{count}:{mt}");
        if let Ok(mut c) = cache().lock() {
            if let Some(hit) = c.get(&key) {
                return Some(hit.clone());
            }
        }
        let out = highlight_file(&abs, start, count)?;
        if let Ok(mut c) = cache().lock() {
            c.put(key, out.clone());
        }
        Some(out)
    }
}

fn highlight_file(abs: &Path, start: usize, count: usize) -> Option<HlWindow> {
    const CTX: usize = 400;
    let ss = syntax_set();
    let ts = theme_set();
    let syntax = find_syntax(ss, abs);
    let theme = &ts.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);

    // Stream lines: advance state through context, emit window.
    let f = std::fs::File::open(abs).ok()?;
    let reader = std::io::BufReader::new(f);
    use std::io::BufRead;
    let ctx_start = start.saturating_sub(CTX);
    let mut total = 0usize;
    let mut lines: Vec<HlLine> = Vec::with_capacity(count);
    let mut buf = String::new();
    for line in reader.lines() {
        let line = line.unwrap_or_default();
        if total < ctx_start {
            total += 1;
            continue;
        }
        // Feed highlighter for context + window to keep multiline state.
        let styled = h.highlight_line(&line, ss).ok()?;
        if total >= start && lines.len() < count {
            let mut html = String::with_capacity(line.len() + 32);
            append_highlighted_html_for_styled_line(&styled, IncludeBackground::No, &mut html)
                .ok()?;
            // Strip trailing newline entities syntect may add.
            lines.push(HlLine { n: total + 1, html });
        }
        total += 1;
        let _ = &mut buf;
        if total > start + count + 1_000_000 {
            break;
        }
        // We need total line count: continue scanning to EOF (bounded 2M).
        if lines.len() >= count && total > start + count {
            // Fast-forward count only: keep reading to get total without highlighting.
            // Switch to byte scan for the rest to save time.
            break;
        }
    }
    // Total: if we broke early, count remaining via fast path.
    let total = if lines.len() >= count {
        total + count_remaining(abs, total)
    } else {
        total
    };
    Some(HlWindow {
        syntax: syntax.name.clone(),
        total,
        start,
        lines,
    })
}

fn count_remaining(abs: &Path, skipped: usize) -> usize {
    // Count lines after `skipped` by streaming without highlight.
    let Ok(f) = std::fs::File::open(abs) else {
        return 0;
    };
    use std::io::BufRead;
    let reader = std::io::BufReader::new(f);
    let mut n = 0usize;
    for _ in reader.lines().skip(skipped) {
        n += 1;
        if n > 2_000_000 {
            break;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Index;
    use std::io::Write;

    #[test]
    fn highlights_rust_window() {
        let dir = tempfile::tempdir().unwrap();
        let fp = dir.path().join("main.rs");
        std::fs::File::create(&fp)
            .unwrap()
            .write_all(b"fn main() {\n    println!(\"hi\");\n}\n")
            .unwrap();
        let idx = Index::new(dir.path().to_path_buf());
        let w = idx.highlight_window("main.rs", 0, 10).unwrap();
        assert_eq!(w.total, 3);
        assert_eq!(w.lines.len(), 3);
        assert!(w.lines[0].html.contains("fn") || w.lines[0].html.contains("main"));
        assert!(w.lines[0].html.contains("<span"));
    }
}
