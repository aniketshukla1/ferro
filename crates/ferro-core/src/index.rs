use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Instant;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowLine {
    pub n: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Window {
    pub total: usize,
    pub start: usize,
    pub lines: Vec<WindowLine>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileMeta {
    pub size: u64,
    pub total_lines: usize,
}

#[derive(Debug, Default)]
pub struct Index {
    root: PathBuf,
    files: RwLock<Vec<FileEntry>>,
    indexed_ms: RwLock<u128>,
    pr: RwLock<Option<crate::pr::PrCtx>>,
}

impl Index {
    pub fn new(root: PathBuf) -> Self {
        let root = root.canonicalize().unwrap_or(root);
        // Instant cold start: serve cached list immediately, rebuild() refreshes.
        let (files, indexed_ms) = crate::cache::load(&root).unwrap_or_default();
        Self {
            root,
            files: RwLock::new(files),
            indexed_ms: RwLock::new(indexed_ms),
            pr: RwLock::new(None),
        }
    }

    pub fn set_pr(&self, ctx: crate::pr::PrCtx) {
        *self.pr.write().unwrap() = Some(ctx);
    }

    pub fn pr_ctx(&self) -> Option<crate::pr::PrCtx> {
        self.pr.read().unwrap().clone()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn snapshot(&self) -> Vec<FileEntry> {
        self.files.read().unwrap().clone()
    }

    pub fn stats(&self) -> (usize, u128) {
        (
            self.files.read().unwrap().len(),
            *self.indexed_ms.read().unwrap(),
        )
    }

    /// Parallel walk, gitignore-aware via `ignore` crate. Skips dir symlinks.
    pub async fn rebuild(&self) {
        let root = self.root.clone();
        let walk_root = root.clone();
        let t0 = Instant::now();
        let files = tokio::task::spawn_blocking(move || walk(&walk_root))
            .await
            .unwrap_or_default();
        let ms = t0.elapsed().as_millis();
        *self.files.write().unwrap() = files.clone();
        *self.indexed_ms.write().unwrap() = ms;
        crate::cache::save(&root, &files, ms);
        tracing::info!(
            "ferro indexed {} files in {}ms",
            self.files.read().unwrap().len(),
            ms
        );
    }

    pub fn safe_join(&self, rel: &str) -> Option<PathBuf> {
        // Reject traversal: must stay under root.
        let p = self.root.join(rel.trim_start_matches('/'));
        let canonical_root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let canonical_p = if p.is_absolute() {
            p
        } else {
            canonical_root.join(rel)
        };
        // Normalize lexically without hitting disk for missing files.
        let joined = self.root.join(rel);
        let normalized = normalize(&joined);
        if normalized.starts_with(&canonical_root) || normalized == canonical_root {
            Some(normalized)
        } else {
            let _ = canonical_p;
            None
        }
    }

    /// O(n) streaming window read. Never loads the whole file into the UI.
    /// Caps line length at 2000 chars to bound a single row.
    pub fn read_window(&self, rel: &str, start: usize, count: usize) -> Option<Window> {
        let p = self.safe_join(rel)?;
        let count = count.clamp(1, 2000);
        let f = std::fs::File::open(&p).ok()?;
        let mut reader = std::io::BufReader::new(f);
        let mut lines: Vec<WindowLine> = Vec::with_capacity(count.min(256));
        let mut total = 0usize;
        let mut buf = String::new();
        use std::io::BufRead;
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            // Strip trailing \n\r without allocating the whole file.
            while buf.ends_with('\n') || buf.ends_with('\r') {
                buf.pop();
            }
            if buf.len() > 2000 {
                buf.truncate(2000);
            }
            if total >= start && lines.len() < count {
                lines.push(WindowLine {
                    n: total + 1,
                    text: std::mem::take(&mut buf),
                });
                // buf was moved; reinit for next iter
                buf = String::new();
            }
            total += 1;
            // Safety cap: don't scan past 2M lines in one request.
            if total > 2_000_000 {
                break;
            }
        }
        Some(Window {
            total,
            start,
            lines,
        })
    }

    pub fn file_meta(&self, rel: &str) -> Option<FileMeta> {
        let p = self.safe_join(rel)?;
        let md = std::fs::metadata(&p).ok()?;
        // Fast newline count on bytes to avoid UTF-8 decode of huge files.
        let total_lines = count_lines_fast(&p);
        Some(FileMeta {
            size: md.len(),
            total_lines,
        })
    }
}

fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn walk(root: &Path) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .threads(num_cpus())
        .build();
    for entry in walker.flatten() {
        let ft = match entry.file_type() {
            Some(t) => t,
            None => continue,
        };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
        // Skip .git internals and build output for speed.
        if path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            s == ".git" || s == ".ferro" || s == "target" || s == "node_modules"
        }) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        // Skip huge binaries from listing (>8MB) but keep them searchable on demand.
        if size > 8 * 1024 * 1024 {
            continue;
        }
        out.push(FileEntry { path: rel, size });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn count_lines_fast(p: &Path) -> usize {
    let Ok(bytes) = std::fs::read(p) else {
        return 0;
    };
    if bytes.is_empty() {
        return 0;
    }
    let mut n = bytes.iter().filter(|&&b| b == b'\n').count();
    if !bytes.ends_with(b"\n") {
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn window_slices_without_full_load() {
        let dir = tempfile::tempdir().unwrap();
        let fp = dir.path().join("big.txt");
        let mut f = std::fs::File::create(&fp).unwrap();
        for i in 1..=5000 {
            writeln!(f, "line {i}").unwrap();
        }
        let idx = Index::new(dir.path().to_path_buf());
        let w = idx.read_window("big.txt", 100, 10).unwrap();
        assert_eq!(w.total, 5000);
        assert_eq!(w.start, 100);
        assert_eq!(w.lines.len(), 10);
        assert_eq!(w.lines[0].n, 101);
        assert_eq!(w.lines[0].text, "line 101");
        let m = idx.file_meta("big.txt").unwrap();
        assert_eq!(m.total_lines, 5000);
    }
}
