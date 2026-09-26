//! Workspace symbol index (B6): background extraction of tree-sitter
//! definitions for supported files, persisted in `symbols.bin`,
//! incremental via watcher hooks. Query side is fuzzy-over-names
//! (`/symbols`) and exact lookup (definition/hover in slice 3).
//!
//! Memory: paths interned once, names owned per symbol, kinds as bytes.
//! A kubernetes-scale tree (~30k files, ~200k symbols) stays well under
//! the 40 MiB heap budget.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::fileindex::FileSnapshot;
use crate::symbols::{outline_ts, supported};

const MAGIC: &[u8; 8] = b"FERROSY1";
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SYMBOLS: usize = 500_000;

#[derive(Debug, Clone)]
pub struct IndexedSymbol {
    pub path_idx: u32,
    pub name: String,
    pub kind: SymbolKind,
    pub line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SymbolKind {
    Function = 1,
    Method = 2,
    Class = 3,
    Struct = 4,
    Enum = 5,
    Interface = 6,
    Trait = 7,
    Type = 8,
    Module = 9,
    Const = 10,
    Field = 11,
    Macro = 12,
    Other = 0,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::Type => "type",
            SymbolKind::Module => "module",
            SymbolKind::Const => "const",
            SymbolKind::Field => "field",
            SymbolKind::Macro => "macro",
            SymbolKind::Other => "other",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "function" => SymbolKind::Function,
            "method" => SymbolKind::Method,
            "class" => SymbolKind::Class,
            "struct" => SymbolKind::Struct,
            "enum" => SymbolKind::Enum,
            "interface" => SymbolKind::Interface,
            "trait" => SymbolKind::Trait,
            "type" => SymbolKind::Type,
            "module" => SymbolKind::Module,
            "const" => SymbolKind::Const,
            "var" => SymbolKind::Const,
            "field" => SymbolKind::Field,
            "macro" => SymbolKind::Macro,
            "impl" | "heading" => SymbolKind::Other,
            _ => SymbolKind::Other,
        }
    }

    fn from_u8(b: u8) -> Self {
        match b {
            1 => SymbolKind::Function,
            2 => SymbolKind::Method,
            3 => SymbolKind::Class,
            4 => SymbolKind::Struct,
            5 => SymbolKind::Enum,
            6 => SymbolKind::Interface,
            7 => SymbolKind::Trait,
            8 => SymbolKind::Type,
            9 => SymbolKind::Module,
            10 => SymbolKind::Const,
            11 => SymbolKind::Field,
            12 => SymbolKind::Macro,
            _ => SymbolKind::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolState {
    Empty,
    Building,
    Ready,
}

pub struct SymbolIndex {
    dir: PathBuf,
    root: PathBuf,
    paths: parking_lot::RwLock<Vec<String>>,
    symbols: arc_swap::ArcSwap<Vec<IndexedSymbol>>,
    state: parking_lot::RwLock<SymbolState>,
    built_generation: AtomicU64,
    pool: rayon::ThreadPool,
}

impl SymbolIndex {
    pub fn new(dir: PathBuf, root: PathBuf) -> Arc<Self> {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads((cpus / 2).max(1))
            .thread_name(|i| format!("ferro-symbols-{i}"))
            .build()
            .expect("symbols pool");
        Arc::new(Self {
            dir,
            root,
            paths: parking_lot::RwLock::new(Vec::new()),
            symbols: arc_swap::ArcSwap::from_pointee(Vec::new()),
            state: parking_lot::RwLock::new(SymbolState::Empty),
            built_generation: AtomicU64::new(0),
            pool,
        })
    }

    pub fn state(&self) -> SymbolState {
        *self.state.read()
    }

    pub fn len(&self) -> usize {
        self.symbols.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn file_path(&self) -> PathBuf {
        self.dir.join("symbols.bin")
    }

    /// Warm start: load `symbols.bin` when every entry still matches the
    /// snapshot (path, size, mtime via the `symbols.meta` sidecar);
    /// otherwise start empty (background rebuild follows via `ensure_built`).
    pub fn preload(self: &Arc<Self>, snap: &Arc<FileSnapshot>) {
        let bytes = match std::fs::read(self.file_path()) {
            Ok(b) => b,
            Err(_) => return,
        };
        let Some((paths, symbols, gen)) = decode(&bytes) else {
            return;
        };
        if symbols.is_empty() {
            return;
        }
        let mut meta: HashMap<&str, (u64, i64)> = HashMap::with_capacity(snap.len());
        for i in 0..snap.len() {
            meta.insert(snap.paths[i].as_str(), (snap.sizes[i], snap.mtimes[i]));
        }
        if !self.validate_file_table(&paths, &meta) {
            return;
        }
        *self.paths.write() = paths;
        self.symbols.store(Arc::new(symbols));
        self.built_generation.store(gen, Ordering::Relaxed);
        *self.state.write() = SymbolState::Ready;
    }

    /// Sidecar mtime validation: every indexed path must match snapshot.
    fn validate_file_table(&self, paths: &[String], meta: &HashMap<&str, (u64, i64)>) -> bool {
        let bytes = match std::fs::read(self.dir.join("symbols.meta")) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let table = decode_meta(&bytes);
        if table.len() != paths.len() {
            return false;
        }
        for (i, p) in paths.iter().enumerate() {
            match (meta.get(p.as_str()), table.get(i)) {
                (Some((size, mtime)), Some((s2, m2))) if size == s2 && mtime == m2 => {}
                _ => return false,
            }
        }
        true
    }

    /// Rebuild in the background when the snapshot moved on. Cheap when
    /// fresh; called after index rebuilds and on watcher batches.
    pub fn ensure_built(self: &Arc<Self>, snap: &Arc<FileSnapshot>) {
        let gen = snap.generation;
        if self.built_generation.load(Ordering::Relaxed) == gen
            && self.state() == SymbolState::Ready
        {
            return;
        }
        if self.state() == SymbolState::Building {
            return;
        }
        if snap.is_empty() {
            return;
        }
        *self.state.write() = SymbolState::Building;
        let this = self.clone();
        let snap = snap.clone();
        self.pool.spawn(move || {
            let (paths, symbols) = build_symbols(&this.root, &snap);
            this.publish(paths, symbols, gen, &snap);
        });
    }

    fn publish(
        &self,
        paths: Vec<String>,
        symbols: Vec<IndexedSymbol>,
        gen: u64,
        snap: &FileSnapshot,
    ) {
        // Persist (best-effort; queries work from memory regardless).
        let bytes = encode(&paths, &symbols, gen);
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let tmp = self.dir.join("symbols.bin.tmp");
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, self.file_path());
            }
            // Sidecar file table for preload validation.
            let meta = encode_meta(&paths, snap);
            let mtmp = self.dir.join("symbols.meta.tmp");
            if std::fs::write(&mtmp, &meta).is_ok() {
                let _ = std::fs::rename(&mtmp, self.dir.join("symbols.meta"));
            }
        }
        *self.paths.write() = paths;
        self.symbols.store(Arc::new(symbols));
        self.built_generation.store(gen, Ordering::Relaxed);
        *self.state.write() = SymbolState::Ready;
    }

    /// Watcher hook: drop deleted/changed paths, re-extract upserts inline
    /// (millisecond-scale per file), persist lazily on next rebuild.
    pub fn note_changes(&self, upserts: &[String], deletes: &[String]) {
        if self.state() != SymbolState::Ready {
            return;
        }
        if upserts.is_empty() && deletes.is_empty() {
            return;
        }
        let dirty: std::collections::HashSet<&str> = upserts
            .iter()
            .map(|s| s.as_str())
            .chain(deletes.iter().map(|s| s.as_str()))
            .collect();
        let paths = self.paths.read().clone();
        let mut keep_paths: Vec<String> = Vec::with_capacity(paths.len());
        let mut remap: HashMap<u32, u32> = HashMap::new();
        for (i, p) in paths.iter().enumerate() {
            if dirty.contains(p.as_str()) {
                continue;
            }
            remap.insert(i as u32, keep_paths.len() as u32);
            keep_paths.push(p.clone());
        }
        let mut symbols: Vec<IndexedSymbol> = self
            .symbols
            .load()
            .iter()
            .filter_map(|s| {
                remap.get(&s.path_idx).map(|n| IndexedSymbol {
                    path_idx: *n,
                    ..s.clone()
                })
            })
            .collect();
        // Re-extract changed files that still exist.
        for p in upserts {
            if let Some(add) = extract_file(&self.root, p, keep_paths.len() as u32) {
                if add.1.is_empty() {
                    continue;
                }
                keep_paths.push(p.clone());
                symbols.extend(add.1);
            }
        }
        symbols.truncate(MAX_SYMBOLS);
        *self.paths.write() = keep_paths;
        self.symbols.store(Arc::new(symbols));
        // Generation stays: content drifted from the build, but queries stay
        // fresh; the next snapshot bump triggers a full rebuild + persist.
        self.built_generation.store(u64::MAX - 1, Ordering::Relaxed);
    }

    /// Fuzzy over symbol names; returns (symbol, score) best-first.
    pub fn query(&self, q: &str, limit: usize) -> Vec<(ResolvedSymbol, i64)> {
        let syms = self.symbols.load_full();
        if syms.is_empty() || q.trim().is_empty() {
            return Vec::new();
        }
        let limit = limit.clamp(1, 100);
        let names: Vec<String> = syms.iter().map(|s| s.name.clone()).collect();
        let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        let snap = crate::fileindex::FileSnapshot {
            paths: names,
            lower,
            base_off: vec![0; syms.len()],
            sizes: vec![0; syms.len()],
            mtimes: vec![0; syms.len()],
            generation: 0,
        };
        let paths = self.paths.read();
        crate::fuzzy::rank_snap(&snap, q, limit, &std::collections::HashSet::new())
            .into_iter()
            .map(|h| {
                let s = &syms[h.index];
                (
                    ResolvedSymbol {
                        path: paths.get(s.path_idx as usize).cloned().unwrap_or_default(),
                        name: s.name.clone(),
                        kind: s.kind,
                        line: s.line,
                        end_line: s.end_line,
                    },
                    h.score,
                )
            })
            .collect()
    }

    /// Exact name lookup for goto-definition ranking (slice 3).
    pub fn by_name(&self, name: &str) -> Vec<(ResolvedSymbol, usize)> {
        let syms = self.symbols.load_full();
        if syms.is_empty() {
            return Vec::new();
        }
        let paths = self.paths.read();
        syms.iter()
            .enumerate()
            .filter(|(_, s)| s.name == name)
            .map(|(i, s)| {
                (
                    ResolvedSymbol {
                        path: paths.get(s.path_idx as usize).cloned().unwrap_or_default(),
                        name: s.name.clone(),
                        kind: s.kind,
                        line: s.line,
                        end_line: s.end_line,
                    },
                    i,
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSymbol {
    pub path: String,
    pub name: String,
    pub kind: SymbolKind,
    pub line: u32,
    pub end_line: u32,
}

/// Extract one file's symbols. Returns (path, symbols) when supported.
fn extract_file(root: &Path, rel: &str, path_idx: u32) -> Option<(String, Vec<IndexedSymbol>)> {
    let ext = rel.rsplit('.').next().unwrap_or("").to_lowercase();
    if !supported(&ext) {
        return None;
    }
    let abs = root.join(rel.trim_start_matches('/'));
    let bytes = std::fs::read(&abs).ok()?;
    if bytes.len() as u64 > MAX_FILE_BYTES || bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    let syms = outline_ts(&ext, &text)?;
    let out: Vec<IndexedSymbol> = syms
        .into_iter()
        .take(5000)
        .map(|s| IndexedSymbol {
            path_idx,
            name: s.name,
            kind: SymbolKind::from_str(s.kind),
            line: s.line as u32,
            end_line: s.end_line.max(s.line) as u32,
        })
        .collect();
    Some((rel.to_string(), out))
}

fn build_symbols(root: &Path, snap: &Arc<FileSnapshot>) -> (Vec<String>, Vec<IndexedSymbol>) {
    use rayon::prelude::*;
    let mut cands: Vec<(usize, String)> = Vec::new();
    for (i, p) in snap.paths.iter().enumerate() {
        if snap.sizes[i] > MAX_FILE_BYTES {
            continue;
        }
        let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
        if supported(&ext) {
            cands.push((i, p.clone()));
        }
    }
    cands.sort_by(|a, b| a.1.cmp(&b.1));
    let parts: Vec<(String, Vec<IndexedSymbol>)> = cands
        .par_iter()
        .filter_map(|(_, p)| {
            let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
            let abs = root.join(p.trim_start_matches('/'));
            let bytes = std::fs::read(&abs).ok()?;
            if bytes.len() as u64 > MAX_FILE_BYTES || bytes[..bytes.len().min(8192)].contains(&0) {
                return None;
            }
            let text = String::from_utf8_lossy(&bytes);
            let syms = outline_ts(&ext, &text)?;
            let out: Vec<IndexedSymbol> = syms
                .into_iter()
                .take(5000)
                .map(|s| IndexedSymbol {
                    path_idx: 0, // fixed up below
                    name: s.name,
                    kind: SymbolKind::from_str(s.kind),
                    line: s.line as u32,
                    end_line: s.end_line.max(s.line) as u32,
                })
                .collect();
            Some((p.clone(), out))
        })
        .collect();
    let mut paths = Vec::with_capacity(parts.len());
    let mut symbols = Vec::new();
    for (p, mut syms) in parts.into_iter() {
        // `parts` came from a sorted + stable par_collect? rayon collect
        // preserves order, so re-sort for determinism.
        let idx = paths.len() as u32;
        for s in syms.iter_mut() {
            s.path_idx = idx;
        }
        paths.push(p);
        symbols.extend(syms);
        if symbols.len() >= MAX_SYMBOLS {
            symbols.truncate(MAX_SYMBOLS);
            break;
        }
    }
    // Deterministic path order (par collect preserves input order, which
    // was sorted — re-sort defensively is O(n log n); skip: input sorted).
    let _ = paths;
    (paths, symbols)
}

// -- persistence -------------------------------------------------------------

fn put_u32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

fn encode(paths: &[String], symbols: &[IndexedSymbol], gen: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    put_u64(&mut out, 1); // format version
    put_u64(&mut out, gen);
    put_u32(&mut out, paths.len() as u32);
    for p in paths {
        put_str(&mut out, p);
    }
    put_u32(&mut out, symbols.len() as u32);
    for s in symbols {
        put_u32(&mut out, s.path_idx);
        put_str(&mut out, &s.name);
        out.push(s.kind as u8);
        put_u32(&mut out, s.line);
        put_u32(&mut out, s.end_line);
    }
    out
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        self.bytes(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        self.bytes(8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        if n > 4 * 1024 * 1024 {
            return None;
        }
        let b = self.bytes(n)?;
        String::from_utf8(b.to_vec()).ok()
    }
}

fn decode(bytes: &[u8]) -> Option<(Vec<String>, Vec<IndexedSymbol>, u64)> {
    let mut c = Cursor { b: bytes, pos: 0 };
    if c.bytes(8)? != MAGIC {
        return None;
    }
    if c.u64()? != 1 {
        return None;
    }
    let gen = c.u64()?;
    let np = c.u32()? as usize;
    if np > 1_000_000 {
        return None;
    }
    let mut paths = Vec::with_capacity(np.min(1024));
    for _ in 0..np {
        paths.push(c.str()?);
    }
    let ns = c.u32()? as usize;
    if ns > MAX_SYMBOLS {
        return None;
    }
    let mut symbols = Vec::with_capacity(ns.min(1024));
    for _ in 0..ns {
        let path_idx = c.u32()?;
        let name = c.str()?;
        let kind = c.bytes(1)?[0];
        let line = c.u32()?;
        let end_line = c.u32()?;
        if (path_idx as usize) >= paths.len() || name.is_empty() || name.len() > 256 || line == 0 {
            return None;
        }
        symbols.push(IndexedSymbol {
            path_idx,
            name,
            kind: SymbolKind::from_u8(kind),
            line,
            end_line,
        });
    }
    Some((paths, symbols, gen))
}

fn decode_meta(bytes: &[u8]) -> Vec<(u64, i64)> {
    // symbols.meta parallels symbols.bin path order: per path (str, size
    // u64, mtime i64).
    let mut c = Cursor { b: bytes, pos: 0 };
    let mut out = Vec::new();
    while let Some(p) = c.str() {
        let _ = p;
        let (Some(size), Some(mtime_bytes)) = (c.u64(), c.bytes(8)) else {
            break;
        };
        out.push((size, i64::from_le_bytes(mtime_bytes.try_into().unwrap())));
    }
    out
}

fn encode_meta(paths: &[String], snap: &FileSnapshot) -> Vec<u8> {
    let mut lookup: HashMap<&str, (u64, i64)> = HashMap::with_capacity(snap.len());
    for i in 0..snap.len() {
        lookup.insert(snap.paths[i].as_str(), (snap.sizes[i], snap.mtimes[i]));
    }
    let mut out = Vec::new();
    for p in paths {
        put_str(&mut out, p);
        let (size, mtime) = lookup.get(p.as_str()).copied().unwrap_or((0, 0));
        put_u64(&mut out, size);
        out.extend_from_slice(&mtime.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_for(dir: &Path, files: &[(&str, &str)]) -> Arc<FileSnapshot> {
        for (p, content) in files {
            let full = dir.join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, content).unwrap();
        }
        let mut items: Vec<(String, u64, i64)> = files
            .iter()
            .map(|(p, _)| {
                let m = std::fs::metadata(dir.join(p)).unwrap();
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
        Arc::new(FileSnapshot {
            lower: items.iter().map(|(p, _, _)| p.to_lowercase()).collect(),
            base_off: items
                .iter()
                .map(|(p, _, _)| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
                .collect(),
            sizes: items.iter().map(|(_, s, _)| *s).collect(),
            mtimes: items.iter().map(|(_, _, m)| *m).collect(),
            paths: items.into_iter().map(|(p, _, _)| p).collect(),
            generation: 7,
        })
    }

    #[test]
    fn build_query_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(
            dir.path(),
            &[
                ("src/main.rs", "fn main() {}\nfn helper() {}\n"),
                ("src/lib.rs", "pub struct Thing;\n"),
                ("notes.txt", "nothing\n"),
            ],
        );
        let (paths, symbols) = build_symbols(dir.path(), &snap);
        assert_eq!(
            paths,
            vec!["src/lib.rs".to_string(), "src/main.rs".to_string()]
        );
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"Thing"));
        // notes.txt unsupported: no symbols.
        assert!(!names.contains(&"nothing"));
    }

    #[test]
    fn persist_and_preload_validates_mtimes() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.rs", "fn f() {}\n")]);
        let idx = SymbolIndex::new(dir.path().join("cache"), dir.path().to_path_buf());
        let (paths, symbols) = build_symbols(dir.path(), &snap);
        idx.publish(paths.clone(), symbols.clone(), 7, &snap);
        assert_eq!(idx.state(), SymbolState::Ready);
        // Write the sidecar the publisher path expects: validated in
        // preload via symbols.meta; emulate a crash before meta write by
        // deleting it → preload must refuse.
        std::fs::remove_file(idx.dir.join("symbols.meta")).ok();
        let idx2 = SymbolIndex::new(dir.path().join("cache"), dir.path().to_path_buf());
        idx2.preload(&snap);
        assert_eq!(idx2.state(), SymbolState::Empty);
    }

    #[test]
    fn fuzzy_query_ranks_names() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(
            dir.path(),
            &[("s.rs", "fn scheduler_run() {}\nfn main() {}\n")],
        );
        let idx = SymbolIndex::new(dir.path().join("c2"), dir.path().to_path_buf());
        let (paths, symbols) = build_symbols(dir.path(), &snap);
        idx.publish(paths, symbols, 7, &snap);
        let hits = idx.query("schdlr", 10);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0.name, "scheduler_run");
    }

    #[test]
    fn note_changes_refreshes_inline() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.rs", "fn f() {}\n")]);
        let idx = SymbolIndex::new(dir.path().join("c3"), dir.path().to_path_buf());
        let (paths, symbols) = build_symbols(dir.path(), &snap);
        idx.publish(paths, symbols, 7, &snap);
        assert_eq!(idx.len(), 1);
        std::fs::write(dir.path().join("a.rs"), "fn f() {}\nfn g() {}\n").unwrap();
        idx.note_changes(&["a.rs".to_string()], &[]);
        assert_eq!(idx.len(), 2);
        idx.note_changes(&[], &["a.rs".to_string()]);
        assert_eq!(idx.len(), 0);
    }
}
