//! Workspace symbol index (B6): background extraction of tree-sitter
//! definitions for supported files, persisted in `symbols.bin`,
//! incremental via watcher hooks. Query side is fuzzy-over-names
//! (`/symbols`) and exact lookup (definition/hover in slice 3).
//!
//! Memory: paths interned once; names once plus their lowercase form (the
//! fuzzy ranker's table); kinds as bytes; one Arc-swapped table per publish.
//! A kubernetes-scale tree (~30k files, ~200k symbols) stays well under
//! the 40 MiB heap budget.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::fileindex::FileSnapshot;
use crate::idents::RefIndex;
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

const EMPTY: u8 = 0;
const BUILDING: u8 = 1;
const READY: u8 = 2;

/// Watched batches up to this many changed files are re-extracted inline;
/// bigger ones (a branch switch) go to the background build.
const INLINE_MAX: usize = 64;

/// A symbol's place in the table; its name is `Table::names.paths[i]`.
#[derive(Debug, Clone, Copy)]
struct SymMeta {
    path_idx: u32,
    kind: SymbolKind,
    line: u32,
    end_line: u32,
}

/// One published index: paths, symbols, and the names the fuzzy ranker
/// reads (lowercased once here, not on every keystroke). Swapped as one, so
/// a query never pairs one build's symbols with another build's paths.
#[derive(Debug, Default)]
struct Table {
    paths: Vec<String>,
    syms: Vec<SymMeta>,
    /// Symbol names as a rankable snapshot (`paths` holds the names).
    names: FileSnapshot,
    /// Identifier postings for references. Inactive until a build fills it.
    refs: RefIndex,
}

impl Table {
    fn new(paths: Vec<String>, symbols: Vec<IndexedSymbol>, refs: RefIndex) -> Self {
        let n = symbols.len();
        let mut names = Vec::with_capacity(n);
        let mut syms = Vec::with_capacity(n);
        for s in symbols {
            syms.push(SymMeta {
                path_idx: s.path_idx,
                kind: s.kind,
                line: s.line,
                end_line: s.end_line,
            });
            names.push(s.name);
        }
        let lower = names.iter().map(|n| n.to_lowercase()).collect();
        Self {
            paths,
            syms,
            names: FileSnapshot {
                paths: names,
                lower,
                base_off: vec![0; n],
                ..Default::default()
            },
            refs,
        }
    }

    fn heap_bytes(&self) -> usize {
        let mut n = self.refs.heap_bytes();
        for p in &self.paths {
            n += p.capacity() + std::mem::size_of::<String>();
        }
        for name in &self.names.paths {
            n += name.capacity() + std::mem::size_of::<String>();
        }
        for lower in &self.names.lower {
            n += lower.capacity() + std::mem::size_of::<String>();
        }
        n += self.syms.capacity() * std::mem::size_of::<SymMeta>();
        n
    }

    fn resolve(&self, i: usize) -> ResolvedSymbol {
        let m = self.syms[i];
        ResolvedSymbol {
            path: self
                .paths
                .get(m.path_idx as usize)
                .cloned()
                .unwrap_or_default(),
            name: self.names.paths[i].clone(),
            kind: m.kind,
            line: m.line,
            end_line: m.end_line,
        }
    }

    /// The build/persist form of every symbol.
    fn indexed(&self) -> impl Iterator<Item = IndexedSymbol> + '_ {
        self.syms
            .iter()
            .zip(&self.names.paths)
            .map(|(m, name)| IndexedSymbol {
                path_idx: m.path_idx,
                name: name.clone(),
                kind: m.kind,
                line: m.line,
                end_line: m.end_line,
            })
    }
}

#[derive(Debug)]
pub struct SymbolIndex {
    dir: PathBuf,
    root: PathBuf,
    table: arc_swap::ArcSwap<Table>,
    /// Serializes table writers: a finished build and watcher updates.
    write: parking_lot::Mutex<()>,
    state: std::sync::atomic::AtomicU8,
    built_generation: AtomicU64,
    /// Something asked for symbols (navigation, an agent tool) in this or an earlier session.
    /// Until then background refreshes skip the build: on kubernetes the table is ~36 MiB.
    wanted: std::sync::atomic::AtomicBool,
    pool: rayon::ThreadPool,
}

impl SymbolIndex {
    /// Run `f` once on every thread of the build pool (the idle scavenger returns each
    /// worker's freed allocator pages to the OS this way).
    pub fn broadcast(&self, f: &(dyn Fn() + Sync)) {
        self.pool.broadcast(|_| f());
    }

    pub fn new(dir: PathBuf, root: PathBuf) -> Arc<Self> {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads((cpus / 2).max(1))
            .thread_name(|i| format!("ferro-symbols-{i}"))
            .build()
            .expect("symbols pool");
        // Used before (a table was persisted): keep it warm from the start.
        let wanted = dir.join("symbols.bin").exists();
        Arc::new(Self {
            wanted: std::sync::atomic::AtomicBool::new(wanted),
            dir,
            root,
            table: arc_swap::ArcSwap::from_pointee(Table::default()),
            write: parking_lot::Mutex::new(()),
            state: std::sync::atomic::AtomicU8::new(EMPTY),
            built_generation: AtomicU64::new(0),
            pool,
        })
    }

    pub fn state(&self) -> SymbolState {
        match self.state.load(Ordering::Acquire) {
            BUILDING => SymbolState::Building,
            READY => SymbolState::Ready,
            _ => SymbolState::Empty,
        }
    }

    pub fn len(&self) -> usize {
        self.table.load().syms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn file_path(&self) -> PathBuf {
        self.dir.join("symbols.bin")
    }

    /// Warm start: load `symbols.bin` when every entry still matches the
    /// snapshot (path, size, mtime via the `symbols.meta` sidecar);
    /// otherwise start empty. The table is readable immediately, but state
    /// stays empty so the first `ensure_built` rebuilds and is what flips
    /// the index to Ready. Ready is the signal nav uses before it times
    /// reference queries, and only a full build carries reference postings.
    pub fn preload(self: &Arc<Self>, snap: &Arc<FileSnapshot>) {
        let bytes = match std::fs::read(self.file_path()) {
            Ok(b) => b,
            Err(_) => return,
        };
        let Some((paths, symbols, _gen)) = decode(&bytes) else {
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
        let _w = self.write.lock();
        // Keep the symbol table for definition queries during the rebuild,
        // but do not report Ready. Ready means `build_index` has published
        // reference postings. A warm `symbols.bin` has no postings, and the
        // nav budget waits on Ready before it times `GET /nav/references`.
        self.table
            .store(Arc::new(Table::new(paths, symbols, RefIndex::inactive())));
        self.built_generation.store(0, Ordering::Release);
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

    /// Background refresh (after index rebuilds, watcher batches, idle): builds only once
    /// something has used symbols, so workspaces that never navigate do not pay for them.
    pub fn refresh(self: &Arc<Self>, snap: &Arc<FileSnapshot>) {
        if self.wanted.load(Ordering::Acquire) {
            self.ensure_built(snap);
        }
    }

    /// Build (or rebuild when the snapshot moved on) in the background, on demand. Cheap when
    /// fresh. Marks symbols as wanted, so later background refreshes keep them current.
    pub fn ensure_built(self: &Arc<Self>, snap: &Arc<FileSnapshot>) {
        self.wanted.store(true, Ordering::Release);
        let gen = snap.generation;
        if snap.is_empty() {
            return;
        }
        let cur = self.state.load(Ordering::Acquire);
        if cur == BUILDING || (cur == READY && self.built_generation.load(Ordering::Acquire) == gen)
        {
            return;
        }
        // Exactly one caller claims the build.
        if self
            .state
            .compare_exchange(cur, BUILDING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let this = self.clone();
        let snap = snap.clone();
        self.pool.spawn(move || {
            // A panic in a pool job would abort the process; fall back to the
            // table we had instead.
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                build_index(&this.root, &snap)
            }));
            match built {
                Ok((paths, symbols, refs)) => this.publish(paths, symbols, refs, gen, &snap),
                Err(_) => {
                    let fallback = if this.is_empty() { EMPTY } else { READY };
                    this.state.store(fallback, Ordering::Release);
                }
            }
        });
    }

    fn publish(
        &self,
        paths: Vec<String>,
        symbols: Vec<IndexedSymbol>,
        refs: RefIndex,
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
        let table = Table::new(paths, symbols, refs);
        let _w = self.write.lock();
        self.table.store(Arc::new(table));
        self.built_generation.store(gen, Ordering::Release);
        self.state.store(READY, Ordering::Release);
    }

    /// Watcher hook: drop deleted/changed paths and re-extract upserts inline
    /// (milliseconds per file), leaving the index current as of snapshot
    /// `gen`: a saved file never costs a full rebuild. Bigger batches, and
    /// changes that land while a build runs, are left to the (next) build.
    /// Persists lazily with that next build.
    pub fn note_changes(&self, upserts: &[String], deletes: &[String], gen: u64) {
        if upserts.is_empty() && deletes.is_empty() {
            return;
        }
        if upserts.len() > INLINE_MAX {
            return;
        }
        let _w = self.write.lock();
        if self.state.load(Ordering::Acquire) != READY {
            return;
        }
        let dirty: std::collections::HashSet<&str> = upserts
            .iter()
            .map(|s| s.as_str())
            .chain(deletes.iter().map(|s| s.as_str()))
            .collect();
        let old = self.table.load_full();
        let mut keep_paths: Vec<String> = Vec::with_capacity(old.paths.len());
        let mut remap: HashMap<u32, u32> = HashMap::new();
        for (i, p) in old.paths.iter().enumerate() {
            if dirty.contains(p.as_str()) {
                continue;
            }
            remap.insert(i as u32, keep_paths.len() as u32);
            keep_paths.push(p.clone());
        }
        let mut symbols: Vec<IndexedSymbol> = old
            .indexed()
            .filter_map(|s| {
                remap
                    .get(&s.path_idx)
                    .map(|n| IndexedSymbol { path_idx: *n, ..s })
            })
            .collect();
        let mut refs = old.refs.clone();
        refs.remap(&remap);
        // Re-extract changed files that still exist.
        for p in upserts {
            let Some((text, add)) = extract_file(&self.root, p, keep_paths.len() as u32) else {
                continue;
            };
            let ext = p.rsplit('.').next().unwrap_or("");
            let names = if crate::idents::is_vendor_path(p) {
                Vec::new()
            } else {
                crate::idents::unique_names(ext, &text)
            };
            if add.is_empty() && names.is_empty() {
                continue;
            }
            keep_paths.push(p.clone());
            let idx = (keep_paths.len() - 1) as u32;
            if !names.is_empty() {
                refs.add_file(idx, &names);
            }
            symbols.extend(add);
        }
        symbols.truncate(MAX_SYMBOLS);
        self.table
            .store(Arc::new(Table::new(keep_paths, symbols, refs)));
        self.built_generation.store(gen, Ordering::Release);
    }

    /// Identifier references from the symbol-index build. `None` when that
    /// build has not published postings yet (preload, or a test table).
    pub fn references_of(&self, name: &str, limit: usize) -> Option<(Vec<ResolvedRef>, bool)> {
        let t = self.table.load_full();
        let (hits, truncated) = t.refs.lookup(name, &self.root, &t.paths, limit)?;
        Some((
            hits.into_iter()
                .map(|(path, line, col)| ResolvedRef { path, line, col })
                .collect(),
            truncated,
        ))
    }

    /// Bytes retained by the published symbol and reference tables.
    pub fn heap_bytes(&self) -> usize {
        self.table.load().heap_bytes()
    }

    /// Stderr breakdown so a scale run can see which table blew the budget.
    pub fn explain_heap(&self) {
        let t = self.table.load();
        let refs = t.refs.heap_bytes();
        let mut paths = 0usize;
        for p in &t.paths {
            paths += p.capacity() + std::mem::size_of::<String>();
        }
        let mut symbol_names = 0usize;
        for name in &t.names.paths {
            symbol_names += name.capacity() + std::mem::size_of::<String>();
        }
        let mut lower = 0usize;
        for name in &t.names.lower {
            lower += name.capacity() + std::mem::size_of::<String>();
        }
        let metas = t.syms.capacity() * std::mem::size_of::<SymMeta>();
        eprintln!(
            "heap parts refs={refs} paths={paths} symbol_names={symbol_names} lower={lower} metas={metas}"
        );
        t.refs.explain_heap();
    }

    pub fn reference_names(&self) -> usize {
        self.table.load().refs.name_count()
    }

    /// Fuzzy over symbol names; returns (symbol, score) best-first.
    pub fn query(&self, q: &str, limit: usize) -> Vec<(ResolvedSymbol, i64)> {
        let t = self.table.load_full();
        if t.syms.is_empty() || q.trim().is_empty() {
            return Vec::new();
        }
        let limit = limit.clamp(1, 100);
        crate::fuzzy::rank_snap(&t.names, q, limit, &std::collections::HashSet::new())
            .into_iter()
            .map(|h| (t.resolve(h.index), h.score))
            .collect()
    }

    /// Exact name lookup for goto-definition ranking (slice 3).
    pub fn by_name(&self, name: &str) -> Vec<(ResolvedSymbol, usize)> {
        let t = self.table.load_full();
        t.names
            .paths
            .iter()
            .enumerate()
            .filter(|(_, n)| n.as_str() == name)
            .map(|(i, _)| (t.resolve(i), i))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedRef {
    pub path: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone)]
pub struct ResolvedSymbol {
    pub path: String,
    pub name: String,
    pub kind: SymbolKind,
    pub line: u32,
    pub end_line: u32,
}

/// Extract one file's symbols and its source (for the reference lexer).
/// Returns `(text, symbols)` when the extension is supported.
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
    let text = String::from_utf8_lossy(&bytes).into_owned();
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
    Some((text, out))
}

struct BuiltFile {
    path: String,
    symbols: Vec<IndexedSymbol>,
    prov: u32,
}

#[cfg(test)]
fn build_symbols(root: &Path, snap: &Arc<FileSnapshot>) -> (Vec<String>, Vec<IndexedSymbol>) {
    let (paths, symbols, _) = build_index(root, snap);
    (paths, symbols)
}

fn build_index(
    root: &Path,
    snap: &Arc<FileSnapshot>,
) -> (Vec<String>, Vec<IndexedSymbol>, RefIndex) {
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
    let shards = crate::idents::RefShards::new();
    let parts: Vec<BuiltFile> = cands
        .par_iter()
        .enumerate()
        .filter_map(|(prov, (_, p))| {
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
            if !crate::idents::is_vendor_path(p) {
                shards.note(prov as u32, &ext, &text);
            }
            Some(BuiltFile {
                path: p.clone(),
                symbols: out,
                prov: prov as u32,
            })
        })
        .collect();
    let mut remap = vec![u32::MAX; cands.len()];
    let mut paths = Vec::with_capacity(parts.len());
    let mut symbols = Vec::new();
    for mut part in parts.into_iter() {
        let idx = paths.len() as u32;
        remap[part.prov as usize] = idx;
        for s in part.symbols.iter_mut() {
            s.path_idx = idx;
        }
        paths.push(part.path);
        symbols.extend(part.symbols);
        if symbols.len() >= MAX_SYMBOLS {
            symbols.truncate(MAX_SYMBOLS);
            break;
        }
    }
    let refs = shards.finish(&remap);
    (paths, symbols, refs)
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
    use crate::idents::RefIndex;

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

    fn with_gen(snap: &FileSnapshot, generation: u64) -> Arc<FileSnapshot> {
        Arc::new(FileSnapshot {
            generation,
            ..snap.clone()
        })
    }

    /// Memory: background refreshes do not build the table until something used symbols;
    /// the first demand does, and later refreshes keep it current.
    #[test]
    fn background_refresh_waits_for_first_use() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("src/main.rs", "fn main() {}\n")]);
        let idx = SymbolIndex::new(dir.path().join("cache"), dir.path().to_path_buf());
        idx.refresh(&snap);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(idx.state(), SymbolState::Empty, "built without being asked");
        idx.ensure_built(&snap);
        let t0 = std::time::Instant::now();
        while idx.state() != SymbolState::Ready && t0.elapsed() < std::time::Duration::from_secs(5)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(idx.state(), SymbolState::Ready);
        // Used once (and persisted): a new session starts warm.
        let again = SymbolIndex::new(dir.path().join("cache"), dir.path().to_path_buf());
        again.refresh(&snap);
        let t0 = std::time::Instant::now();
        while again.state() != SymbolState::Ready
            && t0.elapsed() < std::time::Duration::from_secs(5)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            again.state(),
            SymbolState::Ready,
            "a workspace that navigated before stays warm"
        );
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
        idx.publish(
            paths.clone(),
            symbols.clone(),
            RefIndex::inactive(),
            7,
            &snap,
        );
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
        idx.publish(paths, symbols, RefIndex::inactive(), 7, &snap);
        let hits = idx.query("schdlr", 10);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0.name, "scheduler_run");
    }

    #[test]
    fn a_watched_change_updates_in_place_without_a_full_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.rs", "fn f() {}\n")]);
        let idx = SymbolIndex::new(dir.path().join("c4"), dir.path().to_path_buf());
        let (paths, symbols) = build_symbols(dir.path(), &snap);
        idx.publish(paths, symbols, RefIndex::inactive(), 7, &snap);
        std::fs::write(dir.path().join("a.rs"), "fn f() {}\nfn g() {}\n").unwrap();
        // The watcher applies the batch, then asks for a build of the same snapshot.
        idx.note_changes(&["a.rs".to_string()], &[], 8);
        idx.ensure_built(&with_gen(&snap, 8));
        assert_eq!(
            idx.state(),
            SymbolState::Ready,
            "a saved file must not rebuild the whole index"
        );
        assert_eq!(idx.len(), 2);
    }

    #[test]
    fn queries_never_pair_symbols_with_another_builds_paths() {
        // Two builds of one tree with the path table in opposite orders:
        // alpha lives in a.rs in both, beta in z.rs.
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(
            dir.path(),
            &[("a.rs", "fn alpha() {}\n"), ("z.rs", "fn beta() {}\n")],
        );
        let idx = SymbolIndex::new(dir.path().join("c5"), dir.path().to_path_buf());
        let sym = |name: &str, path_idx| IndexedSymbol {
            path_idx,
            name: name.into(),
            kind: SymbolKind::Function,
            line: 1,
            end_line: 1,
        };
        let a = (
            vec!["a.rs".to_string(), "z.rs".to_string()],
            vec![sym("alpha", 0), sym("beta", 1)],
        );
        let b = (
            vec!["z.rs".to_string(), "a.rs".to_string()],
            vec![sym("alpha", 1), sym("beta", 0)],
        );
        idx.publish(a.0.clone(), a.1.clone(), RefIndex::inactive(), 7, &snap);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (idx, stop, snap) = (idx.clone(), stop.clone(), snap.clone());
            std::thread::spawn(move || {
                let mut flip = false;
                while !stop.load(Ordering::Relaxed) {
                    let (p, s) = if flip { a.clone() } else { b.clone() };
                    idx.publish(p, s, RefIndex::inactive(), 7, &snap);
                    flip = !flip;
                }
            })
        };
        let t0 = std::time::Instant::now();
        let mut wrong = Vec::new();
        while t0.elapsed() < std::time::Duration::from_millis(400) {
            for (s, _) in idx.query("alpha", 5) {
                if s.name == "alpha" && s.path != "a.rs" {
                    wrong.push(s.path);
                }
            }
            for (s, _) in idx.by_name("beta") {
                if s.path != "z.rs" {
                    wrong.push(s.path);
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert!(wrong.is_empty(), "{} mismatched results", wrong.len());
    }

    #[test]
    fn note_changes_refreshes_inline() {
        let dir = tempfile::tempdir().unwrap();
        let snap = snap_for(dir.path(), &[("a.rs", "fn f() {}\n")]);
        let idx = SymbolIndex::new(dir.path().join("c3"), dir.path().to_path_buf());
        let (paths, symbols) = build_symbols(dir.path(), &snap);
        idx.publish(paths, symbols, RefIndex::inactive(), 7, &snap);
        assert_eq!(idx.len(), 1);
        std::fs::write(dir.path().join("a.rs"), "fn f() {}\nfn g() {}\n").unwrap();
        idx.note_changes(&["a.rs".to_string()], &[], 8);
        assert_eq!(idx.len(), 2);
        idx.note_changes(&[], &["a.rs".to_string()], 9);
        assert_eq!(idx.len(), 0);
    }
}
