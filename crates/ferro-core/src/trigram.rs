//! Trigram index (B2b): background-built, mmap-queried.
//! Layout in `<index_dir>/`:
//! - `meta.json`: `{version, docs, bytes, builtMs}`
//! - `docs.bin`: `u32 n` then per doc `u32 path_len, path, u64 size, i64 mtime`
//! - `lexicon.bin`: `u64 n` then sorted `(u32 key, u64 offset, u32 count)` ×16 B
//! - `postings.bin`: delta-varint (LEB128) u32 doc ids, concatenated
//!
//! Trigrams run over ASCII-lowercased bytes. Retrieval intersects posting
//! lists and the caller verifies candidates with the scan engine, so every
//! approximation here only widens — never narrows — the answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::fileindex::FileSnapshot;

pub const INDEX_VERSION: u32 = 1;
/// Files larger than this are skipped by the index (scan covers them).
pub const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DocEntry {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug)]
pub struct BuiltIndex {
    pub docs: Vec<DocEntry>,
    /// trigram key → sorted doc ids.
    pub postings: HashMap<u32, Vec<u32>>,
    pub bytes: u64,
}

pub fn trigram_key(t: &[u8; 3]) -> u32 {
    ((t[0] as u32) << 16) | ((t[1] as u32) << 8) | t[2] as u32
}

fn lower_byte(b: u8) -> u8 {
    if b.is_ascii_uppercase() {
        b + 32
    } else {
        b
    }
}

/// Sorted unique trigram keys of ASCII-lowercased `bytes`.
pub fn file_trigrams(bytes: &[u8]) -> Vec<u32> {
    if bytes.len() < 3 {
        return Vec::new();
    }
    let mut out: Vec<u32> = Vec::with_capacity(bytes.len().saturating_sub(2).min(65536));
    let mut w = [lower_byte(bytes[0]), lower_byte(bytes[1]), 0u8];
    for &b in &bytes[2..] {
        w[2] = lower_byte(b);
        out.push(trigram_key(&w));
        w[0] = w[1];
        w[1] = w[2];
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Sliding trigrams of an already-lowercased literal. Empty when shorter
/// than 3 bytes (→ full scan).
pub fn literal_trigrams(lit: &[u8]) -> Vec<u32> {
    if lit.len() < 3 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(lit.len() - 2);
    for w in lit.windows(3) {
        out.push(trigram_key(&[w[0], w[1], w[2]]));
    }
    out.sort_unstable();
    out.dedup();
    out
}

pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}

/// Build over the snapshot. `excluded` decides skipped paths (search
/// excludes); files over `MAX_INDEX_BYTES` or binary are skipped.
/// Parallel over the background pool via rayon; postings come out sorted
/// because per-thread maps merge in doc-id order per chunk... — no: threads
/// race, so each trigram vec is sorted after merging.
pub fn build_index(
    root: &Path,
    snap: &FileSnapshot,
    excluded: &globset::GlobSet,
    max_file_bytes: u64,
) -> BuiltIndex {
    use rayon::prelude::*;
    let cap = max_file_bytes.min(MAX_INDEX_BYTES);
    // Doc ids follow snapshot order (already path-sorted).
    let mut docs = Vec::with_capacity(snap.len());
    let mut take: Vec<(u32, String)> = Vec::with_capacity(snap.len());
    for (i, p) in snap.paths.iter().enumerate() {
        if excluded.is_match(p) {
            continue;
        }
        let size = snap.sizes.get(i).copied().unwrap_or(0);
        if size > cap {
            continue;
        }
        let id = docs.len() as u32;
        docs.push(DocEntry {
            path: p.clone(),
            size,
            mtime: snap.mtimes.get(i).copied().unwrap_or(0),
        });
        take.push((id, p.clone()));
    }
    let bytes: u64 = docs.iter().map(|d| d.size).sum();
    // Per-file trigram sets in parallel, then invert.
    let per_doc: Vec<(u32, Vec<u32>)> = take
        .par_iter()
        .filter_map(|(id, rel)| {
            let full = root.join(rel);
            let b = std::fs::read(&full).ok()?;
            if b.len() as u64 > cap || is_binary(&b) {
                return None;
            }
            Some((*id, file_trigrams(&b)))
        })
        .collect();
    let mut postings: HashMap<u32, Vec<u32>> = HashMap::new();
    for (id, tris) in per_doc {
        for t in tris {
            postings.entry(t).or_default().push(id);
        }
    }
    for v in postings.values_mut() {
        v.sort_unstable();
    }
    BuiltIndex {
        docs,
        postings,
        bytes,
    }
}

fn write_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_le_bytes());
}

fn write_u64(w: &mut Vec<u8>, v: u64) {
    w.extend_from_slice(&v.to_le_bytes());
}

fn write_varint(w: &mut Vec<u8>, mut v: u32) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        w.push(b);
        if v == 0 {
            break;
        }
    }
}

pub fn read_varint(r: &[u8], pos: &mut usize) -> Option<u32> {
    let mut v = 0u32;
    let mut shift = 0;
    loop {
        let b = *r.get(*pos)?;
        *pos += 1;
        v |= ((b & 0x7f) as u32) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            break;
        }
        if shift >= 35 {
            return None;
        }
    }
    Some(v)
}

pub fn write_index(
    dir: &Path,
    built: &BuiltIndex,
    built_ms: u128,
    generation: u64,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let meta = serde_json::json!({
        "version": INDEX_VERSION,
        "docs": built.docs.len(),
        "bytes": built.bytes,
        "builtMs": built_ms,
        "generation": generation,
    });
    // Data files first, meta last: readers double-check the generation
    // before and after opening and retry on mismatch (no torn reads).
    let mut docs = Vec::new();
    write_u32(&mut docs, built.docs.len() as u32);
    for d in &built.docs {
        write_u32(&mut docs, d.path.len() as u32);
        docs.extend_from_slice(d.path.as_bytes());
        write_u64(&mut docs, d.size);
        docs.extend_from_slice(&d.mtime.to_le_bytes());
    }
    atomic_file(&dir.join("docs.bin"), &docs)?;
    // postings + lexicon
    let mut keys: Vec<u32> = built.postings.keys().copied().collect();
    keys.sort_unstable();
    let mut post = Vec::new();
    let mut lex = Vec::new();
    write_u64(&mut lex, keys.len() as u64);
    for k in keys {
        let ids = &built.postings[&k];
        let off = post.len() as u64;
        let mut prev = 0u32;
        for (i, &id) in ids.iter().enumerate() {
            write_varint(&mut post, if i == 0 { id } else { id - prev });
            prev = id;
        }
        write_u32(&mut lex, k);
        write_u64(&mut lex, off);
        write_u32(&mut lex, ids.len() as u32);
    }
    atomic_file(&dir.join("postings.bin"), &post)?;
    atomic_file(&dir.join("lexicon.bin"), &lex)?;
    atomic_file(
        &dir.join("meta.json"),
        serde_json::to_vec(&meta).unwrap().as_slice(),
    )?;
    Ok(())
}

/// Temp file + rename (readers never see partial files).
fn atomic_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Query-time mmap'd index.
pub struct MmapIndex {
    pub docs: Vec<DocEntry>,
    _docs_map: memmap2::Mmap,
    lex_map: memmap2::Mmap,
    post_map: memmap2::Mmap,
    pub lex_count: u64,
}

impl MmapIndex {
    pub fn open(dir: &Path) -> Option<Self> {
        // Generation double-check against torn reads (writers replace
        // files + meta via atomic renames, meta last).
        let gen_a = read_generation(dir)?;
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
        if meta.get("version").and_then(|v| v.as_u64()) != Some(INDEX_VERSION as u64) {
            return None;
        }
        let docs_f = std::fs::File::open(dir.join("docs.bin")).ok()?;
        let lex_f = std::fs::File::open(dir.join("lexicon.bin")).ok()?;
        let post_f = std::fs::File::open(dir.join("postings.bin")).ok()?;
        // SAFETY: files are replaced by atomic rename, never modified in
        // place; a rename racing our open yields either generation entirely.
        let docs_map = unsafe { memmap2::Mmap::map(&docs_f).ok()? };
        let lex_map = unsafe { memmap2::Mmap::map(&lex_f).ok()? };
        let post_map = unsafe { memmap2::Mmap::map(&post_f).ok()? };
        let docs = parse_docs(&docs_map)?;
        let lex_count = read_u64(&lex_map, 0);
        let gen_b = read_generation(dir)?;
        if gen_a != gen_b {
            return None;
        }
        Some(Self {
            docs,
            _docs_map: docs_map,
            lex_map,
            post_map,
            lex_count,
        })
    }

    /// Decode postings for one trigram. `None` = trigram never indexed
    /// (empty answer modulo the unscanned delta set).
    pub fn postings(&self, key: u32) -> Option<Vec<u32>> {
        // Binary search the 16-byte lexicon records.
        let rec = |i: u64| -> (u32, u64, u32) {
            let off = 8 + i as usize * 16;
            (
                read_u32(&self.lex_map, off),
                read_u64(&self.lex_map, off + 4),
                read_u32(&self.lex_map, off + 12),
            )
        };
        let mut lo = 0u64;
        let mut hi = self.lex_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (k, off, count) = rec(mid);
            if k == key {
                return Some(decode_postings(
                    &self.post_map,
                    off as usize,
                    count as usize,
                ));
            } else if k < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        None
    }

    /// Postings length without decoding (for cheapest-first intersection).
    pub fn postings_len(&self, key: u32) -> Option<usize> {
        self.postings(key).map(|v| v.len())
    }
}

fn read_u32(r: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(r[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn read_u64(r: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(r[off..off + 8].try_into().unwrap_or([0; 8]))
}

fn read_generation(dir: &Path) -> Option<u64> {
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
    meta.get("generation").and_then(|v| v.as_u64())
}

fn parse_docs(map: &[u8]) -> Option<Vec<DocEntry>> {
    let n = read_u32(map, 0) as usize;
    let mut pos = 4usize;
    let mut docs = Vec::with_capacity(n.min(1_000_000));
    for _ in 0..n {
        let len = read_u32(map, pos) as usize;
        pos += 4;
        let path = std::str::from_utf8(map.get(pos..pos + len)?)
            .ok()?
            .to_string();
        pos += len;
        let size = read_u64(map, pos);
        pos += 8;
        let mtime = i64::from_le_bytes(map.get(pos..pos + 8)?.try_into().ok()?);
        pos += 8;
        docs.push(DocEntry { path, size, mtime });
    }
    Some(docs)
}

fn decode_postings(post: &[u8], mut pos: usize, count: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(count.min(1_000_000));
    let mut prev = 0u32;
    for i in 0..count {
        let Some(d) = read_varint(post, &mut pos) else {
            break;
        };
        let id = if i == 0 { d } else { prev.wrapping_add(d) };
        prev = id;
        out.push(id);
    }
    out
}

/// Intersect decoded posting lists (cheapest first). Empty input = no
/// constraint (caller falls back to scan).
pub fn intersect(mut lists: Vec<Vec<u32>>) -> Vec<u32> {
    if lists.is_empty() {
        return Vec::new();
    }
    lists.sort_by_key(|l| l.len());
    let mut acc = std::mem::take(&mut lists[0]);
    for other in lists.iter().skip(1) {
        acc = intersect_two(&acc, other);
        if acc.is_empty() {
            break;
        }
    }
    acc
}

fn intersect_two(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    out
}

// -- regex query planning -------------------------------------------------------

/// Required trigram AND-set for a regex. Empty = no usable literals (scan).
/// Case-insensitive patterns fold through the same ASCII-lowercasing as the
/// index, so verification stays exact via the scan engine.
pub fn plan_regex(pattern: &str, case_insensitive: bool) -> Vec<u32> {
    let hir = regex_syntax::ParserBuilder::new()
        .unicode(true)
        .build()
        .parse(pattern);
    let Ok(hir) = hir else {
        return Vec::new();
    };
    let mut out = extract(&hir, case_insensitive);
    out.sort_unstable();
    out.dedup();
    out
}

fn lower_bytes(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|&b| lower_byte(b)).collect()
}

fn extract(hir: &regex_syntax::hir::Hir, _ci: bool) -> Vec<u32> {
    use regex_syntax::hir::HirKind::*;
    match hir.kind() {
        Empty | Look(_) => Vec::new(),
        Literal(lit) => {
            // The index folds ASCII case; fold the query the same way.
            // Verification stays exact via the scan engine.
            literal_trigrams(&lower_bytes(&lit.0))
        }
        Class(_) => Vec::new(),
        Repetition(rep) => {
            if rep.min == 0 {
                Vec::new()
            } else {
                extract(&rep.sub, _ci)
            }
        }
        Capture(cap) => extract(&cap.sub, _ci),
        Concat(hs) => {
            let mut out = Vec::new();
            for h in hs.iter() {
                out.extend(extract(h, _ci));
            }
            out.sort_unstable();
            out.dedup();
            out
        }
        Alternation(hs) => {
            // Required regardless of branch: intersect children's sets.
            let mut it = hs.iter();
            let Some(first) = it.next() else {
                return Vec::new();
            };
            let mut acc = extract(first, _ci);
            acc.sort_unstable();
            for h in it {
                let mut b = extract(h, _ci);
                b.sort_unstable();
                acc = intersect_two(&acc, &b);
                if acc.is_empty() {
                    break;
                }
            }
            acc
        }
    }
}

/// Trigram key set for a query in a given mode (already cased per query).
/// Returns None when the index cannot help (short queries, regex without
/// literals) — the caller scans.
pub fn plan_query(pattern: &str, literal: bool, ci: bool) -> Option<Vec<u32>> {
    if pattern.len() < 3 {
        return None;
    }
    let tris = if literal {
        let mut low = vec![0u8; pattern.len()];
        for (i, b) in pattern.bytes().enumerate() {
            low[i] = lower_byte(b);
        }
        literal_trigrams(&low)
    } else {
        plan_regex(pattern, ci)
    };
    if tris.is_empty() {
        return None;
    }
    Some(tris)
}

/// Decode the useful posting lists, cheapest first. `None` = scan instead;
/// empty vec = a trigram is provably absent (delta set still scanned).
/// Trigrams with more than `high_df_cap` postings are skipped (too common
/// to filter; verification keeps results exact).
pub fn fetch_ordered(idx: &MmapIndex, keys: &[u32], high_df_cap: usize) -> Option<Vec<Vec<u32>>> {
    let mut out: Vec<Vec<u32>> = Vec::with_capacity(keys.len());
    for k in keys {
        match idx.postings(*k) {
            None => return Some(Vec::new()),
            Some(v) if v.len() > high_df_cap => {}
            Some(v) => out.push(v),
        }
    }
    if out.is_empty() {
        return None;
    }
    out.sort_by_key(|v| v.len());
    Some(out)
}

// -- search engine: policy, freshness, background builds ------------------------

/// Very common trigrams are skipped at query time (no filtering power).
pub const HIGH_DF_SKIP: usize = 500_000;
/// Auto-build thresholds (spec): 5,000 files or 50 MiB of text.
pub const AUTO_MIN_FILES: usize = 5_000;
pub const AUTO_MIN_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchIndexState {
    Off,
    Building,
    Ready,
}

impl SearchIndexState {
    pub fn as_str(self) -> &'static str {
        match self {
            SearchIndexState::Off => "off",
            SearchIndexState::Building => "building",
            SearchIndexState::Ready => "ready",
        }
    }
}

pub struct SearchEngine {
    dir: PathBuf,
    state: std::sync::RwLock<SearchIndexState>,
    built_generation: std::sync::atomic::AtomicU64,
    built_docs: std::sync::atomic::AtomicUsize,
    delta: std::sync::Mutex<std::collections::HashSet<String>>,
    pool: rayon::ThreadPool,
    mmap: std::sync::RwLock<Option<std::sync::Arc<MmapIndex>>>,
    on_transition: std::sync::Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

impl SearchEngine {
    pub fn new(dir: PathBuf) -> std::sync::Arc<Self> {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads((cpus / 2).max(1))
            .thread_name(|i| format!("ferro-index-{i}"))
            .build()
            .expect("index pool");
        std::sync::Arc::new(Self {
            dir,
            state: std::sync::RwLock::new(SearchIndexState::Off),
            built_generation: std::sync::atomic::AtomicU64::new(0),
            built_docs: std::sync::atomic::AtomicUsize::new(0),
            delta: std::sync::Mutex::new(std::collections::HashSet::new()),
            pool,
            mmap: std::sync::RwLock::new(None),
            on_transition: std::sync::Mutex::new(None),
        })
    }

    /// Try loading a previously built index (warm start).
    pub fn preload(self: &std::sync::Arc<Self>, generation: u64) {
        if let Some(m) = MmapIndex::open(&self.dir) {
            let docs = m.docs.len();
            *self.mmap.write().unwrap() = Some(std::sync::Arc::new(m));
            self.built_docs
                .store(docs, std::sync::atomic::Ordering::Relaxed);
            self.built_generation
                .store(generation, std::sync::atomic::Ordering::Relaxed);
            self.set_state(SearchIndexState::Ready);
        }
    }

    pub fn state(&self) -> SearchIndexState {
        *self.state.read().unwrap()
    }

    fn set_state(&self, s: SearchIndexState) {
        let changed = *self.state.read().unwrap() != s;
        *self.state.write().unwrap() = s;
        if changed {
            if let Some(cb) = self.on_transition.lock().unwrap().clone() {
                cb();
            }
        }
    }

    pub fn set_on_transition(&self, f: std::sync::Arc<dyn Fn() + Send + Sync>) {
        *self.on_transition.lock().unwrap() = Some(f);
    }

    /// Record watcher changes: upserts and deletes both join the delta set
    /// (deleted paths act as tombstones via snapshot misses at query time).
    pub fn note_changes(&self, upserts: &[String], deletes: &[String]) {
        if self.state() == SearchIndexState::Off {
            return;
        }
        let mut d = self.delta.lock().unwrap();
        d.extend(upserts.iter().cloned());
        d.extend(deletes.iter().cloned());
    }

    pub fn delta_len(&self) -> usize {
        self.delta.lock().unwrap().len()
    }

    /// Policy check + background build. Cheap when fresh; called after every
    /// index rebuild, on watcher batches, and on idle.
    /// `mode`: `on` | `auto` | `off`. `default_exclude`: search.exclude globs.
    pub fn ensure_built(
        self: &std::sync::Arc<Self>,
        snap: &std::sync::Arc<FileSnapshot>,
        root: &Path,
        mode: &str,
        max_file_bytes: u64,
        default_exclude: &[String],
    ) {
        if mode == "off" {
            if self.state() != SearchIndexState::Off {
                self.set_state(SearchIndexState::Off);
            }
            return;
        }
        let gen = snap.generation;
        let fresh = self.state() == SearchIndexState::Ready
            && self
                .built_generation
                .load(std::sync::atomic::Ordering::Relaxed)
                == gen;
        if fresh {
            let docs = self
                .built_docs
                .load(std::sync::atomic::Ordering::Relaxed)
                .max(1);
            // Rebuild when the delta exceeds 5% of files (spec).
            if self.delta_len() * 100 <= docs * 5 {
                return;
            }
        }
        if self.state() == SearchIndexState::Building {
            return;
        }
        if mode == "auto" {
            let bytes: u64 = snap.sizes.iter().sum();
            if snap.len() < AUTO_MIN_FILES && bytes < AUTO_MIN_BYTES {
                return;
            }
        }
        if snap.is_empty() {
            return;
        }
        self.set_state(SearchIndexState::Building);
        let this = self.clone();
        let snap = snap.clone();
        let root = root.to_path_buf();
        let exclude = build_exclude(default_exclude);
        let dir = self.dir.clone();
        self.pool.spawn(move || {
            let t0 = std::time::Instant::now();
            let built = build_index(&root, &snap, &exclude, max_file_bytes);
            // A build that indexed nothing useful stays Off (saves queries
            // from consulting an empty map).
            if built.docs.is_empty() {
                this.set_state(SearchIndexState::Off);
                return;
            }
            if write_index(&dir, &built, t0.elapsed().as_millis(), gen).is_err() {
                this.set_state(SearchIndexState::Off);
                return;
            }
            // Clear the delta: the build just read current disk content, so
            // notes older than this point are covered. (A note racing its
            // own file's build read stays stale until the next rebuild;
            // the idle/threshold triggers bound that window.)
            this.delta.lock().unwrap().clear();
            this.built_docs
                .store(built.docs.len(), std::sync::atomic::Ordering::Relaxed);
            this.built_generation
                .store(gen, std::sync::atomic::Ordering::Relaxed);
            if MmapIndex::open(&dir)
                .map(|m| *this.mmap.write().unwrap() = Some(std::sync::Arc::new(m)))
                .is_some()
            {
                this.set_state(SearchIndexState::Ready);
            } else {
                this.set_state(SearchIndexState::Off);
            }
        });
    }

    /// Resolve planned trigram keys to snapshot indices: index hits plus
    /// delta members (always scanned directly), tombstones dropped via
    /// snapshot misses. `None` = fall back to full scan.
    pub fn candidates(&self, snap: &FileSnapshot, keys: &[u32]) -> Option<Vec<usize>> {
        let mmap = self.mmap.read().unwrap().clone()?;
        if self.state() != SearchIndexState::Ready {
            return None;
        }
        let lists = fetch_ordered(&mmap, keys, HIGH_DF_SKIP)?;
        let mut ids = intersect(lists);
        // Delta members always join (freshness over filtering).
        {
            let delta = self.delta.lock().unwrap();
            if !delta.is_empty() {
                let mut extra: Vec<u32> = Vec::new();
                for (i, d) in mmap.docs.iter().enumerate() {
                    if delta.contains(&d.path) {
                        extra.push(i as u32);
                    }
                }
                ids.extend(extra);
                ids.sort_unstable();
                ids.dedup();
            }
        }
        // Doc ids → snapshot indices via path binary search. The snapshot
        // stays path-sorted across rebuilds and deltas (see fileindex).
        // Delta paths missing from the index entirely (new files) resolve
        // here too; tombstones miss and drop out.
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(doc) = mmap.docs.get(id as usize) else {
                continue;
            };
            if let Ok(i) = snap.paths.binary_search(&doc.path) {
                out.push(i);
            }
        }
        {
            let delta = self.delta.lock().unwrap();
            for p in delta.iter() {
                if docs_contain(&mmap.docs, p) {
                    continue;
                }
                if let Ok(i) = snap.paths.binary_search(p) {
                    out.push(i);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }
}

fn docs_contain(docs: &[DocEntry], path: &str) -> bool {
    docs.binary_search_by(|d| d.path.as_str().cmp(path)).is_ok()
}

/// Default search excludes as a glob set (`!default` allowed through).
pub fn build_exclude(patterns: &[String]) -> globset::GlobSet {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigrams_fold_case() {
        assert_eq!(file_trigrams(b"ab"), Vec::<u32>::new());
        let a = file_trigrams(b"Serve");
        let b = file_trigrams(b"serve");
        assert_eq!(a, b);
        assert_eq!(
            literal_trigrams(b"serve"),
            vec![
                trigram_key(b"erv"),
                trigram_key(b"rve"),
                trigram_key(b"ser")
            ]
        );
    }

    #[test]
    fn varint_roundtrip() {
        for v in [0u32, 1, 127, 128, 300, 65536, u32::MAX] {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            let mut pos = 0;
            assert_eq!(read_varint(&buf, &mut pos), Some(v));
        }
    }

    #[test]
    fn regex_goldens() {
        let t = |p: &str| plan_regex(p, false);
        assert_eq!(t(r"foo.*bar").len(), 2);
        assert!(t(r"foo.*bar").contains(&trigram_key(b"foo")));
        assert!(t(r"foo.*bar").contains(&trigram_key(b"bar")));
        assert!(t(r"(abc|abd)x").is_empty());
        assert!(t(r"\d+").is_empty());
        assert!(t(r"^pub fn").contains(&trigram_key(b"pub")));
        assert!(t(r"server(-\d+)?").contains(&trigram_key(b"ser")));
        assert!(t(r"a").is_empty());
        assert!(t(r"[a-z]+").is_empty());
        // Case-insensitive folds to the same trigrams.
        assert_eq!(plan_regex("Serve", true), plan_regex("serve", false));
    }

    #[test]
    fn build_query_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn serve() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn other() {}\n").unwrap();
        std::fs::write(dir.path().join("c.bin"), [0u8, 1, 2]).unwrap();
        let snap = FileSnapshot {
            paths: vec!["a.rs".into(), "b.rs".into(), "c.bin".into()],
            lower: vec!["a.rs".into(), "b.rs".into(), "c.bin".into()],
            base_off: vec![0, 0, 0],
            sizes: vec![14, 14, 3],
            mtimes: vec![0, 0, 0],
            generation: 0,
        };
        let empty = globset::GlobSet::empty();
        let built = build_index(dir.path(), &snap, &empty, 8 * 1024 * 1024);
        assert_eq!(built.docs.len(), 3);
        // Binary file indexed with no trigrams.
        let idx_dir = dir.path().join("idx");
        write_index(&idx_dir, &built, 1, 0).unwrap();
        let idx = MmapIndex::open(&idx_dir).unwrap();
        assert_eq!(idx.docs.len(), 3);
        let hits = idx.postings(trigram_key(b"ser")).unwrap();
        assert_eq!(hits, vec![0]);
        assert!(idx.postings(trigram_key(b"zzz")).is_none());
        assert!(!idx.postings(trigram_key(b"fn ")).unwrap().is_empty());
    }
}
