use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Instant;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Default)]
pub struct Index {
    root: PathBuf,
    files: RwLock<Vec<FileEntry>>,
    indexed_ms: RwLock<u128>,
}

impl Index {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            files: RwLock::new(Vec::new()),
            indexed_ms: RwLock::new(0),
        }
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
        let t0 = Instant::now();
        let files = tokio::task::spawn_blocking(move || walk(&root))
            .await
            .unwrap_or_default();
        let ms = t0.elapsed().as_millis();
        *self.files.write().unwrap() = files;
        *self.indexed_ms.write().unwrap() = ms;
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
            s == ".git" || s == "target" || s == "node_modules"
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
