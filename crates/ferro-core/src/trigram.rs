//! Trigram index (B2b): background-built, mmap-queried.
//!
//! Each build is written to its own directory `<index_dir>/g-<id>/` and
//! published by atomically replacing `<index_dir>/current`, which names it.
//! Readers resolve `current` once and only open files of one complete
//! build, so there are no torn reads, and concurrent writers (two ferro
//! processes on one repo) never share temp names. A build directory holds:
//! - `meta.json`: `{version, docs, bytes, builtMs}`
//! - `docs.bin`: `u32 n` then per doc `u32 path_len, path, u64 size,
//!   i64 mtime, u8 indexed`
//! - `lexicon.bin`: `u64 n` then sorted `(u32 key, u64 offset, u32 count)`
//!   ×16 B
//! - `postings.bin`: delta-varint (LEB128) u32 doc ids, concatenated
//!
//! Docs list every snapshot file. `indexed = false` marks files whose
//! trigrams are unknown (over the index cap, unreadable): queries always
//! scan them. Search excludes apply at query time only, so changing them
//! never hides indexed files.
//!
//! Trigrams run over ASCII-lowercased bytes, with the two non-ASCII letters
//! that Unicode case-folds to ASCII (KELVIN SIGN → k, LONG S → s) folded
//! too. Case-insensitive plans drop trigrams holding other non-ASCII bytes,
//! whose case variants differ in bytes. Retrieval intersects posting lists
//! and the caller verifies candidates with the scan engine, so every
//! approximation here only widens — never narrows — the answer.
//!
//! Freshness: the index answers for one snapshot generation at a time. On
//! a new generation it reconciles docs against the snapshot (size, mtime)
//! and adds every difference to the delta set, which is always scanned; the
//! watcher's notes cover same-second edits. Parsing is bounds-checked
//! throughout: a damaged index reads as absent and gets rebuilt.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::fileindex::FileSnapshot;

pub const INDEX_VERSION: u32 = 2;
/// Files larger than this are not indexed (queries scan them directly).
pub const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;
/// Files per parallel build chunk: bounds the per-file trigram sets held
/// in memory at once to one chunk's worth.
const BUILD_CHUNK: usize = 2048;

#[derive(Debug, Clone)]
pub struct DocEntry {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    /// False when the build could not read the file's trigrams.
    pub indexed: bool,
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
    b.to_ascii_lowercase()
}

/// KELVIN SIGN (E2 84 AA) and LATIN SMALL LETTER LONG S (C5 BF) case-fold
/// to ASCII `k` and `s`; fold them so a case-insensitive `k`/`s` trigram
/// still finds them. Everything else passes through.
fn fold_to_ascii(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let has = memchr::memchr2(0xE2, 0xC5, bytes).is_some();
    if !has {
        return std::borrow::Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(&[0xE2, 0x84, 0xAA]) {
            out.push(b'k');
            i += 3;
        } else if bytes[i..].starts_with(&[0xC5, 0xBF]) {
            out.push(b's');
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Sorted unique trigram keys of case-folded `bytes`.
pub fn file_trigrams(bytes: &[u8]) -> Vec<u32> {
    let bytes = fold_to_ascii(bytes);
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

/// Sliding trigrams of an already-folded literal. Empty when shorter than
/// 3 bytes (→ full scan).
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

/// Fold a query literal exactly as file content is folded.
fn fold_query(bytes: &[u8]) -> Vec<u8> {
    fold_to_ascii(bytes)
        .iter()
        .map(|&b| lower_byte(b))
        .collect()
}

pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}

/// Build over the whole snapshot (doc ids follow its path-sorted order).
/// Files are read in parallel chunks and inverted chunk by chunk, so only
/// one chunk of per-file trigram sets is alive at a time, and posting lists
/// come out sorted without a final sort.
pub fn build_index(root: &Path, snap: &FileSnapshot) -> BuiltIndex {
    use rayon::prelude::*;
    let mut docs: Vec<DocEntry> = snap
        .paths
        .iter()
        .enumerate()
        .map(|(i, p)| DocEntry {
            path: p.clone(),
            size: snap.sizes.get(i).copied().unwrap_or(0),
            mtime: snap.mtimes.get(i).copied().unwrap_or(0),
            indexed: true,
        })
        .collect();
    let mut postings: HashMap<u32, Vec<u32>> = HashMap::new();
    let ids: Vec<u32> = (0..docs.len() as u32).collect();
    for chunk in ids.chunks(BUILD_CHUNK) {
        // `None`: trigrams unknown (too big, unreadable) → always scanned.
        // `Some([])`: binary, never a text match.
        let per_doc: Vec<(u32, Option<Vec<u32>>)> = chunk
            .par_iter()
            .map(|&id| {
                let d = &docs[id as usize];
                if d.size > MAX_INDEX_BYTES {
                    return (id, None);
                }
                let tris = std::fs::read(root.join(&d.path)).ok().and_then(|b| {
                    if b.len() as u64 > MAX_INDEX_BYTES {
                        None
                    } else if is_binary(&b) {
                        Some(Vec::new())
                    } else {
                        Some(file_trigrams(&b))
                    }
                });
                (id, tris)
            })
            .collect();
        for (id, tris) in per_doc {
            match tris {
                None => docs[id as usize].indexed = false,
                Some(tris) => {
                    for t in tris {
                        postings.entry(t).or_default().push(id);
                    }
                }
            }
        }
    }
    let bytes = docs.iter().filter(|d| d.indexed).map(|d| d.size).sum();
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

/// Unique name for one build (process, time, counter): concurrent writers,
/// in-process or not, never collide.
fn build_name() -> String {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "g-{t:x}-{:x}-{:x}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    )
}

/// Write `built` into a fresh build directory, publish it as `current`,
/// and remove older builds. Returns the published directory.
pub fn write_index(
    index_dir: &Path,
    built: &BuiltIndex,
    built_ms: u128,
) -> Result<PathBuf, String> {
    let io = |e: std::io::Error| e.to_string();
    std::fs::create_dir_all(index_dir).map_err(io)?;
    let name = build_name();
    let tmp = index_dir.join(format!(".{name}.tmp"));
    std::fs::create_dir_all(&tmp).map_err(io)?;
    let written = (|| -> Result<(), String> {
        let mut docs = Vec::new();
        write_u32(&mut docs, built.docs.len() as u32);
        for d in &built.docs {
            write_u32(&mut docs, d.path.len() as u32);
            docs.extend_from_slice(d.path.as_bytes());
            write_u64(&mut docs, d.size);
            docs.extend_from_slice(&d.mtime.to_le_bytes());
            docs.push(d.indexed as u8);
        }
        std::fs::write(tmp.join("docs.bin"), &docs).map_err(io)?;
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
        std::fs::write(tmp.join("postings.bin"), &post).map_err(io)?;
        std::fs::write(tmp.join("lexicon.bin"), &lex).map_err(io)?;
        let meta = serde_json::json!({
            "version": INDEX_VERSION,
            "docs": built.docs.len(),
            "bytes": built.bytes,
            "builtMs": built_ms,
        });
        std::fs::write(tmp.join("meta.json"), meta.to_string()).map_err(io)?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let dir = index_dir.join(&name);
    std::fs::rename(&tmp, &dir).map_err(io)?;
    // Publish: an atomic rename of the pointer file.
    let ptr_tmp = index_dir.join(format!(".current-{name}.tmp"));
    std::fs::write(&ptr_tmp, &name).map_err(io)?;
    std::fs::rename(&ptr_tmp, index_dir.join("current")).map_err(io)?;
    cleanup(index_dir, &name);
    Ok(dir)
}

/// Best effort: drop superseded builds, stale temp dirs, and the flat v1
/// layout. Readers that already mapped an old build keep their mapping.
fn cleanup(index_dir: &Path, keep: &str) {
    let Ok(rd) = std::fs::read_dir(index_dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let stale_tmp = name.ends_with(".tmp")
            && e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > 3600);
        if (name.starts_with("g-") && name != keep) || stale_tmp {
            let _ = std::fs::remove_dir_all(e.path());
        }
        if matches!(
            name.as_str(),
            "docs.bin" | "lexicon.bin" | "postings.bin" | "meta.json"
        ) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Query-time mmap'd index (one published build).
pub struct MmapIndex {
    pub docs: Vec<DocEntry>,
    /// Docs whose trigrams are unknown: every query scans them.
    unindexed: Vec<u32>,
    lex_map: memmap2::Mmap,
    post_map: memmap2::Mmap,
    lex_count: usize,
}

/// One posting-list lookup.
pub enum Lookup {
    /// The trigram occurs in no indexed doc.
    Absent,
    List(Vec<u32>),
    /// The files disagree with themselves: answer by scanning.
    Corrupt,
}

impl MmapIndex {
    /// Open the published build of `index_dir`, or `None` (absent, other
    /// version, damaged).
    pub fn open(index_dir: &Path) -> Option<Self> {
        // A build superseded between reading `current` and opening its files
        // has just been cleaned up: re-resolve once.
        for _ in 0..2 {
            let name = std::fs::read_to_string(index_dir.join("current")).ok()?;
            let name = name.trim();
            if !name.starts_with("g-") || name.contains(['/', '\\']) {
                return None;
            }
            if let Some(m) = Self::open_build(&index_dir.join(name)) {
                return Some(m);
            }
        }
        None
    }

    fn open_build(dir: &Path) -> Option<Self> {
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
        if meta.get("version").and_then(|v| v.as_u64()) != Some(INDEX_VERSION as u64) {
            return None;
        }
        let docs_f = std::fs::File::open(dir.join("docs.bin")).ok()?;
        let lex_f = std::fs::File::open(dir.join("lexicon.bin")).ok()?;
        let post_f = std::fs::File::open(dir.join("postings.bin")).ok()?;
        // SAFETY: a published build directory is never modified; new builds
        // go to new directories.
        let docs_map = unsafe { memmap2::Mmap::map(&docs_f).ok()? };
        let lex_map = unsafe { memmap2::Mmap::map(&lex_f).ok()? };
        let post_map = unsafe { memmap2::Mmap::map(&post_f).ok()? };
        let docs = parse_docs(&docs_map)?;
        let lex_count = usize::try_from(read_u64(&lex_map, 0)?).ok()?;
        if lex_map.len() != lex_count.checked_mul(16)?.checked_add(8)? {
            return None;
        }
        // Every record must point inside postings.bin (≥ 1 byte per id).
        let mut prev_key = None;
        for i in 0..lex_count {
            let (key, off, count) = lex_record(&lex_map, i)?;
            if prev_key.is_some_and(|p| p >= key) {
                return None;
            }
            prev_key = Some(key);
            let end = usize::try_from(off).ok()?.checked_add(count as usize)?;
            if end > post_map.len() {
                return None;
            }
        }
        let unindexed = docs
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.indexed)
            .map(|(i, _)| i as u32)
            .collect();
        Some(Self {
            docs,
            unindexed,
            lex_map,
            post_map,
            lex_count,
        })
    }

    /// Decode the postings of one trigram.
    pub fn postings(&self, key: u32) -> Lookup {
        let (mut lo, mut hi) = (0usize, self.lex_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let Some((k, off, count)) = lex_record(&self.lex_map, mid) else {
                return Lookup::Corrupt;
            };
            match k.cmp(&key) {
                std::cmp::Ordering::Equal => {
                    return match decode_postings(&self.post_map, off as usize, count as usize) {
                        Some(ids) if ids.iter().all(|&id| (id as usize) < self.docs.len()) => {
                            Lookup::List(ids)
                        }
                        _ => Lookup::Corrupt,
                    };
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Lookup::Absent
    }
}

fn read_u32(r: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        r.get(off..off.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u64(r: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        r.get(off..off.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn lex_record(lex: &[u8], i: usize) -> Option<(u32, u64, u32)> {
    let off = 8usize.checked_add(i.checked_mul(16)?)?;
    Some((
        read_u32(lex, off)?,
        read_u64(lex, off + 4)?,
        read_u32(lex, off + 12)?,
    ))
}

fn parse_docs(map: &[u8]) -> Option<Vec<DocEntry>> {
    let n = read_u32(map, 0)? as usize;
    // Each doc takes at least 21 bytes: a count past the file is damage.
    if n > map.len() / 21 + 1 {
        return None;
    }
    let mut pos = 4usize;
    let mut docs = Vec::with_capacity(n);
    for _ in 0..n {
        let len = read_u32(map, pos)? as usize;
        pos += 4;
        let path = std::str::from_utf8(map.get(pos..pos.checked_add(len)?)?)
            .ok()?
            .to_string();
        pos += len;
        let size = read_u64(map, pos)?;
        pos += 8;
        let mtime = read_u64(map, pos)? as i64;
        pos += 8;
        let indexed = *map.get(pos)? != 0;
        pos += 1;
        docs.push(DocEntry {
            path,
            size,
            mtime,
            indexed,
        });
    }
    // Doc ids map to snapshot paths by binary search: they must be sorted.
    if docs.windows(2).any(|w| w[0].path >= w[1].path) {
        return None;
    }
    Some(docs)
}

/// `None` when the list runs past the file or a varint is malformed.
fn decode_postings(post: &[u8], mut pos: usize, count: usize) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(count.min(1_000_000));
    let mut prev = 0u32;
    for i in 0..count {
        let d = read_varint(post, &mut pos)?;
        let id = if i == 0 { d } else { prev.checked_add(d)? };
        prev = id;
        out.push(id);
    }
    Some(out)
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

// -- query planning ---------------------------------------------------------------

/// Case-insensitive plans keep ASCII-only trigrams: a non-ASCII letter's
/// other case is different bytes the folded index never saw.
fn ascii_only(mut keys: Vec<u32>, case_insensitive: bool) -> Vec<u32> {
    if case_insensitive {
        keys.retain(|k| k & 0x0080_8080 == 0);
    }
    keys
}

/// Required trigram AND-set for a regex. Empty = no usable literals (scan).
pub fn plan_regex(pattern: &str, case_insensitive: bool) -> Vec<u32> {
    let hir = regex_syntax::ParserBuilder::new()
        .unicode(true)
        .build()
        .parse(pattern);
    let Ok(hir) = hir else {
        return Vec::new();
    };
    let mut out = extract(&hir);
    out.sort_unstable();
    out.dedup();
    ascii_only(out, case_insensitive)
}

fn extract(hir: &regex_syntax::hir::Hir) -> Vec<u32> {
    use regex_syntax::hir::HirKind::*;
    match hir.kind() {
        Empty | Look(_) | Class(_) => Vec::new(),
        // The index folds case; fold the literal the same way (verification
        // stays exact via the scan engine).
        Literal(lit) => literal_trigrams(&fold_query(&lit.0)),
        Repetition(rep) => {
            if rep.min == 0 {
                Vec::new()
            } else {
                extract(&rep.sub)
            }
        }
        Capture(cap) => extract(&cap.sub),
        Concat(hs) => {
            let mut out: Vec<u32> = hs.iter().flat_map(extract).collect();
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
            let mut acc = extract(first);
            acc.sort_unstable();
            for h in it {
                let mut b = extract(h);
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

/// Trigram key set for a query. `case_insensitive` must be the scan's own
/// effective flag (`Query::case_insensitive`). `None` when the index cannot
/// help (short queries, regex without usable literals): the caller scans.
pub fn plan_query(pattern: &str, literal: bool, case_insensitive: bool) -> Option<Vec<u32>> {
    if pattern.len() < 3 {
        return None;
    }
    let tris = if literal {
        ascii_only(
            literal_trigrams(&fold_query(pattern.as_bytes())),
            case_insensitive,
        )
    } else {
        plan_regex(pattern, case_insensitive)
    };
    (!tris.is_empty()).then_some(tris)
}

/// Decode the useful posting lists, cheapest first. `None` = scan instead;
/// empty vec = a trigram is provably absent (unindexed docs and the delta
/// set are still scanned). Trigrams with more than `high_df_cap` postings
/// are skipped (too common to filter; verification keeps results exact).
pub fn fetch_ordered(idx: &MmapIndex, keys: &[u32], high_df_cap: usize) -> Option<Vec<Vec<u32>>> {
    let mut out: Vec<Vec<u32>> = Vec::with_capacity(keys.len());
    for k in keys {
        match idx.postings(*k) {
            Lookup::Absent => return Some(Vec::new()),
            Lookup::Corrupt => return None,
            Lookup::List(v) if v.len() > high_df_cap => {}
            Lookup::List(v) => out.push(v),
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

/// What the index plus the delta set account for.
#[derive(Default)]
struct Freshness {
    /// Snapshot generation reconciled against (None: not yet, e.g. right
    /// after a warm start from a previous session's index).
    covered: Option<u64>,
    /// Paths to scan regardless of the index → sequence number of the note.
    delta: HashMap<String, u64>,
    seq: u64,
}

pub struct SearchEngine {
    dir: PathBuf,
    state: RwLock<SearchIndexState>,
    /// Claimed by exactly one background build at a time.
    building: AtomicBool,
    /// The warm start from disk runs once, on the first policy check.
    preloaded: AtomicBool,
    fresh: Mutex<Freshness>,
    pool: rayon::ThreadPool,
    mmap: RwLock<Option<Arc<MmapIndex>>>,
    on_transition: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl SearchEngine {
    /// Run `f` once on every thread of the build pool (see `SymbolIndex::broadcast`).
    pub fn broadcast(&self, f: &(dyn Fn() + Sync)) {
        self.pool.broadcast(|_| f());
    }

    pub fn new(dir: PathBuf) -> Arc<Self> {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads((cpus / 2).max(1))
            .thread_name(|i| format!("ferro-index-{i}"))
            .build()
            .expect("index pool");
        Arc::new(Self {
            dir,
            state: RwLock::new(SearchIndexState::Off),
            building: AtomicBool::new(false),
            preloaded: AtomicBool::new(false),
            fresh: Mutex::new(Freshness::default()),
            pool,
            mmap: RwLock::new(None),
            on_transition: Mutex::new(None),
        })
    }

    /// Warm start from a previously published build (once; `ensure_built`
    /// calls it). It is reconciled against the live snapshot before it
    /// answers anything: files may have changed while ferro was not running.
    pub fn preload(&self) {
        if self.preloaded.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(m) = MmapIndex::open(&self.dir) {
            *self.mmap.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(m));
            self.set_state(SearchIndexState::Ready);
        }
    }

    pub fn state(&self) -> SearchIndexState {
        *self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn set_state(&self, s: SearchIndexState) {
        let changed = {
            let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
            let changed = *st != s;
            *st = s;
            changed
        };
        if changed {
            let cb = self
                .on_transition
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(cb) = cb {
                cb();
            }
        }
    }

    pub fn set_on_transition(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.on_transition.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    fn freshness(&self) -> std::sync::MutexGuard<'_, Freshness> {
        self.fresh.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record watcher changes: upserts and deletes join the delta set
    /// (deleted paths drop out at query time as snapshot misses).
    pub fn note_changes(&self, upserts: &[String], deletes: &[String]) {
        if self
            .mmap
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
        {
            return;
        }
        let mut f = self.freshness();
        for p in upserts.iter().chain(deletes) {
            f.seq += 1;
            let s = f.seq;
            f.delta.insert(p.clone(), s);
        }
    }

    pub fn delta_len(&self) -> usize {
        self.freshness().delta.len()
    }

    /// Bring the delta set up to `snap`: every file new since the build or
    /// whose size/mtime differs from the indexed copy is scanned directly.
    /// Once per snapshot generation (a merge of two sorted lists).
    fn sync(&self, snap: &FileSnapshot, m: &MmapIndex) {
        let mut f = self.freshness();
        if f.covered == Some(snap.generation) {
            return;
        }
        let (mut i, mut j) = (0usize, 0usize);
        let mut changed = Vec::new();
        while i < snap.len() {
            let p = &snap.paths[i];
            match m.docs.get(j).map(|d| d.path.as_str().cmp(p.as_str())) {
                Some(std::cmp::Ordering::Less) => j += 1,
                Some(std::cmp::Ordering::Equal) => {
                    let d = &m.docs[j];
                    if snap.sizes.get(i) != Some(&d.size) || snap.mtimes.get(i) != Some(&d.mtime) {
                        changed.push(p.clone());
                    }
                    i += 1;
                    j += 1;
                }
                _ => {
                    changed.push(p.clone());
                    i += 1;
                }
            }
        }
        for p in changed {
            f.seq += 1;
            let s = f.seq;
            f.delta.insert(p, s);
        }
        f.covered = Some(snap.generation);
    }

    /// Policy check + background build. Cheap when fresh; called after
    /// index rebuilds, on watcher batches, and on idle. `mode`: `on` |
    /// `auto` | `off`. While a rebuild runs, the previous index (plus its
    /// delta) keeps answering.
    pub fn ensure_built(self: &Arc<Self>, snap: &Arc<FileSnapshot>, root: &Path, mode: &str) {
        if mode == "off" {
            self.set_state(SearchIndexState::Off);
            return;
        }
        self.preload();
        let current = self.mmap.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(m) = &current {
            self.sync(snap, m);
            // Rebuild only when the delta exceeds 5% of files (spec).
            if self.delta_len() * 100 <= m.docs.len().max(1) * 5 {
                if self.state() != SearchIndexState::Ready {
                    self.set_state(SearchIndexState::Ready);
                }
                return;
            }
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
        // Exactly one build at a time (callers race: watcher, idle, jobs).
        if self
            .building
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if current.is_none() {
            self.set_state(SearchIndexState::Building);
        }
        let this = self.clone();
        let snap = snap.clone();
        let root = root.to_path_buf();
        let start_seq = self.freshness().seq;
        self.pool.spawn(move || {
            let t0 = std::time::Instant::now();
            let built = build_index(&root, &snap);
            let published = write_index(&this.dir, &built, t0.elapsed().as_millis())
                .ok()
                .and_then(|dir| MmapIndex::open_build(&dir));
            match published {
                Some(m) => {
                    {
                        // The build read content newer than every note up to
                        // `start_seq`; later notes may predate their file's
                        // read, so they stay.
                        let mut f = this.freshness();
                        f.delta.retain(|_, s| *s > start_seq);
                        f.covered = Some(snap.generation);
                    }
                    *this.mmap.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(m));
                    this.set_state(SearchIndexState::Ready);
                }
                None => {
                    let has_index = this
                        .mmap
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_some();
                    if !has_index {
                        this.set_state(SearchIndexState::Off);
                    }
                }
            }
            this.building.store(false, Ordering::Release);
        });
    }

    /// Resolve planned trigram keys to snapshot indices: index hits, every
    /// unindexed doc, and the delta set; paths gone from the snapshot drop
    /// out. `None` = fall back to a full scan.
    pub fn candidates(&self, snap: &FileSnapshot, keys: &[u32]) -> Option<Vec<usize>> {
        if self.state() != SearchIndexState::Ready {
            return None;
        }
        let m = self
            .mmap
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        self.sync(snap, &m);
        let lists = fetch_ordered(&m, keys, HIGH_DF_SKIP)?;
        let mut ids = intersect(lists);
        ids.extend_from_slice(&m.unindexed);
        let mut out: Vec<usize> = ids
            .into_iter()
            .filter_map(|id| m.docs.get(id as usize))
            .filter_map(|d| snap.paths.binary_search(&d.path).ok())
            .collect();
        let f = self.freshness();
        out.extend(
            f.delta
                .keys()
                .filter_map(|p| snap.paths.binary_search(p).ok()),
        );
        drop(f);
        out.sort_unstable();
        out.dedup();
        Some(out)
    }
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
        // KELVIN SIGN and LONG S fold to their ASCII letters.
        assert_eq!(
            file_trigrams("\u{212A}ey".as_bytes()),
            file_trigrams(b"key")
        );
        assert_eq!(file_trigrams("u\u{17F}e".as_bytes()), file_trigrams(b"use"));
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
        // Case-insensitive folds to the same (ASCII) trigrams.
        assert_eq!(plan_regex("Serve", true), plan_regex("serve", false));
    }

    #[test]
    fn case_insensitive_plans_keep_ascii_trigrams_only() {
        let key = |s: &[u8]| trigram_key(&[s[0], s[1], s[2]]);
        let ci = plan_query("café", true, true).unwrap();
        assert_eq!(ci, vec![key(b"caf")]);
        // Case-sensitive keeps them (bytes must match exactly anyway).
        let cs = plan_query("café", true, false).unwrap();
        assert!(cs.len() > 1);
        // Nothing ASCII left: the index cannot help.
        assert!(plan_query("éèê", true, true).is_none());
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
        let built = build_index(dir.path(), &snap);
        assert_eq!(built.docs.len(), 3);
        // Binary file: indexed, with no trigrams.
        assert!(built.docs[2].indexed);
        let idx_dir = dir.path().join("idx");
        write_index(&idx_dir, &built, 1).unwrap();
        let idx = MmapIndex::open(&idx_dir).unwrap();
        assert_eq!(idx.docs.len(), 3);
        assert!(matches!(idx.postings(trigram_key(b"ser")), Lookup::List(v) if v == vec![0]));
        assert!(matches!(idx.postings(trigram_key(b"zzz")), Lookup::Absent));
        assert!(matches!(idx.postings(trigram_key(b"fn ")), Lookup::List(v) if !v.is_empty()));
        // A second build replaces the first and cleans it up.
        write_index(&idx_dir, &built, 1).unwrap();
        let builds = std::fs::read_dir(&idx_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("g-"))
            .count();
        assert_eq!(builds, 1);
    }
}
