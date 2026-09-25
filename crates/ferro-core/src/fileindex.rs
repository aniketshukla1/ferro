//! FileIndex snapshot (B2a): contiguous path arenas swapped with ArcSwap.
//! The snapshot is never cloned per request; rayon workers borrow it.
//! Incremental add/remove hooks (watcher, B3) bump the generation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Lowercase + basename offsets live next to each path so scoring touches
/// hot, contiguous memory.
#[derive(Debug, Clone, Default)]
pub struct FileSnapshot {
    pub paths: Vec<String>,
    pub lower: Vec<String>,
    /// Byte offset where the basename starts within `paths[i]`.
    pub base_off: Vec<u32>,
    pub sizes: Vec<u64>,
    pub mtimes: Vec<i64>,
    pub generation: u64,
}

impl FileSnapshot {
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn basename(&self, i: usize) -> &str {
        &self.paths[i][self.base_off[i] as usize..]
    }
}

#[derive(Debug)]
pub struct FileIndex {
    inner: arc_swap::ArcSwap<FileSnapshot>,
    generation: AtomicU64,
}

impl Default for FileIndex {
    fn default() -> Self {
        Self {
            inner: arc_swap::ArcSwap::from_pointee(FileSnapshot::default()),
            generation: AtomicU64::new(0),
        }
    }
}

impl FileIndex {
    pub fn load(&self) -> Arc<FileSnapshot> {
        self.inner.load_full()
    }

    /// Swap in a fresh walk. Returns the new generation.
    pub fn store(&self, paths: Vec<String>, sizes: Vec<u64>, mtimes: Vec<i64>) -> u64 {
        debug_assert_eq!(paths.len(), sizes.len());
        debug_assert_eq!(paths.len(), mtimes.len());
        let mut lower = Vec::with_capacity(paths.len());
        let mut base_off = Vec::with_capacity(paths.len());
        for p in &paths {
            lower.push(p.to_lowercase());
            base_off.push(p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32);
        }
        let gen = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner.store(Arc::new(FileSnapshot {
            paths,
            lower,
            base_off,
            sizes,
            mtimes,
            generation: gen,
        }));
        gen
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Incremental hooks for the B3 watcher: rebuild the snapshot with
    /// additions/updates applied and removals dropped.
    pub fn apply_delta(&self, upsert: &[(String, u64, i64)], remove: &[String]) -> u64 {
        let cur = self.load();
        let mut map: std::collections::HashMap<&str, (u64, i64)> =
            std::collections::HashMap::with_capacity(cur.len());
        for i in 0..cur.len() {
            map.insert(cur.paths[i].as_str(), (cur.sizes[i], cur.mtimes[i]));
        }
        for r in remove {
            map.remove(r.as_str());
        }
        for (p, size, mtime) in upsert {
            map.insert(p.as_str(), (*size, *mtime));
        }
        let mut paths: Vec<String> = map.keys().map(|s| s.to_string()).collect();
        paths.sort();
        let (sizes, mtimes) = paths
            .iter()
            .map(|p| map[p.as_str()])
            .unzip::<u64, i64, Vec<u64>, Vec<i64>>();
        self.store(paths, sizes, mtimes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_delta() {
        let idx = FileIndex::default();
        let g1 = idx.store(vec!["b.rs".into(), "a.rs".into()], vec![1, 2], vec![10, 20]);
        assert_eq!(g1, 1);
        let snap = idx.load();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.basename(0), "b.rs");
        assert_eq!(snap.lower[1], "a.rs");
        let g2 = idx.apply_delta(&[("c.rs".into(), 3, 30)], &["b.rs".into()]);
        assert_eq!(g2, 2);
        let snap = idx.load();
        let paths: Vec<&str> = snap.paths.iter().map(|s| s.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "c.rs"]);
        // Old snapshot still intact (ArcSwap).
        assert_eq!(g1, 1);
    }
}
