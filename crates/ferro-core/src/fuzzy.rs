//! Fuzzy file finder, v2 (B2a): byte-level two-pass scorer over the
//! snapshot arenas (no per-candidate allocation), rayon above 4k files with
//! per-thread top-k heaps, UTF-16 match positions, recency `boost`.
//! Weights keep the v1 feel: basename, boundaries, camel humps, runs.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

use crate::fileindex::FileSnapshot;

#[derive(Debug, Clone)]
pub struct FuzzyHit {
    pub index: usize,
    pub score: i64,
    /// UTF-16 code-unit indices into the path (for frontend highlighting).
    pub positions: Vec<usize>,
}

/// Byte-level two-pass match. `q` must be lowercased; `lower` is the
/// lowercased path. Returns (score, byte positions).
fn score_bytes(q: &[u8], path: &[u8], lower: &[u8], base_off: usize) -> Option<(i64, Vec<usize>)> {
    if q.is_empty() {
        return Some((0, vec![]));
    }
    if q.len() > path.len() {
        return None;
    }
    // Pass 1: greedy subsequence.
    let mut hits: Vec<usize> = Vec::with_capacity(q.len());
    let mut qi = 0;
    for (i, &c) in lower.iter().enumerate() {
        if qi < q.len() && c == q[qi] {
            hits.push(i);
            qi += 1;
        }
    }
    if qi != q.len() {
        return None;
    }
    // Pass 2: tighten from the end (rightmost tightest match).
    let mut tight = vec![0usize; q.len()];
    for k in (0..q.len()).rev() {
        let lo = if k == 0 { 0 } else { hits[k - 1] + 1 };
        let hi = if k + 1 < q.len() {
            tight[k + 1].saturating_sub(1)
        } else {
            lower.len().saturating_sub(1)
        };
        let mut best = hits[k];
        let mut j = hi.min(lower.len().saturating_sub(1));
        loop {
            if j >= lo && j < lower.len() && lower[j] == q[k] {
                best = j;
                break;
            }
            if j == 0 || j <= lo {
                break;
            }
            j -= 1;
        }
        tight[k] = best;
    }

    let mut s: i64 = 0;
    let mut prev: Option<usize> = None;
    for (k, &h) in tight.iter().enumerate() {
        let in_base = h >= base_off;
        if k == 0 && in_base && h == base_off {
            s += 20;
        }
        if in_base {
            s += 14;
        }
        if in_base && path.get(h) == q.get(k) {
            s += 4;
        }
        if h == 0
            || matches!(
                path.get(h.wrapping_sub(1)),
                Some(b'/') | Some(b'_') | Some(b'-') | Some(b' ')
            )
        {
            s += 16;
        }
        if h > 0
            && path
                .get(h - 1)
                .map(|c| c.is_ascii_lowercase())
                .unwrap_or(false)
            && path.get(h).map(|c| c.is_ascii_uppercase()).unwrap_or(false)
        {
            s += 14;
        }
        if let Some(pr) = prev {
            if h == pr + 1 {
                s += 12;
            } else {
                s -= (h - pr - 1).min(12) as i64;
            }
        }
        prev = Some(h);
    }
    // Verbatim query inside the basename.
    if base_off < lower.len() {
        let base = &lower[base_off..];
        if base.windows(q.len()).any(|w| w == q) {
            s += 40;
        }
    }
    s -= (path.len() as i64) / 8;
    s -= (path.iter().filter(|&&c| c == b'/').count() as i64) * 2;
    Some((s, tight))
}

/// Byte offsets → UTF-16 unit indices, snapped to char boundaries.
fn byte_to_utf16(path: &str, bytes: &[usize]) -> Vec<usize> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut bi = 0usize;
    let mut ui = 0usize;
    let mut want = bytes.iter().peekable();
    for (i, c) in path.char_indices() {
        while let Some(&&b) = want.peek() {
            if b < i {
                want.next();
                continue;
            }
            break;
        }
        while let Some(&&b) = want.peek() {
            if b == i {
                out.push(ui);
                want.next();
            } else {
                break;
            }
        }
        ui += c.len_utf16();
        bi = i + c.len_utf8();
        let _ = bi;
    }
    // Trailing positions at/past the end clamp to the end.
    while want.next().is_some() {
        out.push(ui);
    }
    out
}

/// Rank one snapshot. `boost` paths get +50 (frontend recency).
pub fn rank_snap(
    snap: &FileSnapshot,
    query: &str,
    limit: usize,
    boost: &HashSet<String>,
) -> Vec<FuzzyHit> {
    let limit = limit.clamp(1, 200);
    let q = query.to_lowercase();
    let qb = q.as_bytes();
    if qb.is_empty() {
        return vec![];
    }
    let n = snap.len();
    let use_rayon = n > 4000;
    // Per-thread top-k min-heaps keyed (score, Reverse<idx>) for determinism.
    let collect = |range: std::ops::Range<usize>| -> BinaryHeap<Reverse<(i64, Reverse<usize>)>> {
        let mut heap = BinaryHeap::with_capacity(limit + 1);
        for i in range {
            let path = snap.paths[i].as_bytes();
            let lower = snap.lower[i].as_bytes();
            if let Some((mut s, _)) = score_bytes(qb, path, lower, snap.base_off[i] as usize) {
                if boost.contains(&snap.paths[i]) {
                    s += 50;
                }
                heap.push(Reverse((s, Reverse(i))));
                if heap.len() > limit {
                    heap.pop();
                }
            }
        }
        heap
    };
    let mut merged: BinaryHeap<Reverse<(i64, Reverse<usize>)>> =
        BinaryHeap::with_capacity(limit + 1);
    if use_rayon {
        use rayon::prelude::*;
        let cpus = std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(4);
        let chunk = (n + cpus - 1) / cpus;
        let heaps: Vec<_> = (0..n)
            .collect::<Vec<_>>()
            .par_chunks(chunk.max(1))
            .map(|c| {
                let r = c[0]..c[c.len() - 1] + 1;
                collect(r)
            })
            .collect();
        for mut h in heaps {
            for item in h.drain() {
                merged.push(item);
                if merged.len() > limit {
                    merged.pop();
                }
            }
        }
    } else {
        merged = collect(0..n);
    }
    let mut sorted: Vec<(i64, usize)> = merged
        .into_iter()
        .map(|Reverse((s, Reverse(i)))| (s, i))
        .collect();
    sorted.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| snap.paths[a.1].cmp(&snap.paths[b.1]))
    });
    sorted.truncate(limit);
    sorted
        .into_iter()
        .map(|(score, index)| {
            let path = &snap.paths[index];
            let lower = &snap.lower[index];
            let tight = score_bytes(
                qb,
                path.as_bytes(),
                lower.as_bytes(),
                snap.base_off[index] as usize,
            )
            .map(|(_, t)| t)
            .unwrap_or_default();
            FuzzyHit {
                index,
                score,
                positions: byte_to_utf16(path, &tight),
            }
        })
        .collect()
}

/// Legacy compat: score one path (allocates; prefer [`rank_snap`]).
pub fn score(query: &str, path: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q = query.to_lowercase();
    let lower = path.to_lowercase();
    let base = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    score_bytes(q.as_bytes(), path.as_bytes(), lower.as_bytes(), base).map(|(s, _)| s)
}

/// Legacy compat: rank a slice (sequential).
pub fn rank<'a>(query: &str, paths: &'a [String], limit: usize) -> Vec<(&'a str, i64)> {
    let snap = FileSnapshot {
        paths: paths.to_vec(),
        lower: paths.iter().map(|p| p.to_lowercase()).collect(),
        base_off: paths
            .iter()
            .map(|p| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
            .collect(),
        sizes: vec![0; paths.len()],
        mtimes: vec![0; paths.len()],
        generation: 0,
    };
    rank_snap(&snap, query, limit, &HashSet::new())
        .into_iter()
        .map(|h| (paths[h.index].as_str(), h.score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(paths: &[&str]) -> FileSnapshot {
        let v: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        FileSnapshot {
            lower: v.iter().map(|p| p.to_lowercase()).collect(),
            base_off: v
                .iter()
                .map(|p| p.rfind('/').map(|i| i + 1).unwrap_or(0) as u32)
                .collect(),
            sizes: vec![0; v.len()],
            mtimes: vec![0; v.len()],
            generation: 0,
            paths: v,
        }
    }

    #[test]
    fn basename_wins() {
        assert!(
            score("srv", "src/server.rs").unwrap()
                > score("srv", "src/services/billing/reconcile_verbose.rs").unwrap()
        );
    }

    #[test]
    fn subsequence_required() {
        assert!(score("zzz", "src/main.rs").is_none());
    }

    #[test]
    fn exact_basename_first() {
        let s = snap(&[
            "src/kubelet.go",
            "src/kubelet/kubelet.go",
            "pkg/kubelet.go.bak",
        ]);
        let r = rank_snap(&s, "kubelet.go", 3, &HashSet::new());
        assert_eq!(s.paths[r[0].index], "src/kubelet.go");
    }

    #[test]
    fn tight_cluster_wins() {
        let s = snap(&[
            "pkg/scheduler/scheduler.go",
            "pkg/scheduler/eventhandlers.go",
        ]);
        let r = rank_snap(&s, "schdlr", 2, &HashSet::new());
        assert_eq!(s.paths[r[0].index], "pkg/scheduler/scheduler.go");
    }

    #[test]
    fn shallow_wins_ties() {
        let s = snap(&[
            "a/cmd/kube-apiserver/apiserver.go",
            "cmd/kube-apiserver/apiserver.go",
        ]);
        let r = rank_snap(&s, "apiserver", 2, &HashSet::new());
        assert_eq!(s.paths[r[0].index], "cmd/kube-apiserver/apiserver.go");
    }

    #[test]
    fn boost_lifts() {
        let s = snap(&["src/zebra.rs", "src/alpha.rs"]);
        let plain = rank_snap(&s, "alpha", 2, &HashSet::new());
        assert_eq!(s.paths[plain[0].index], "src/alpha.rs");
        let mut b = HashSet::new();
        b.insert("src/zebra.rs".to_string());
        let boosted = rank_snap(&s, "zebr", 2, &b);
        assert_eq!(s.paths[boosted[0].index], "src/zebra.rs");
    }

    #[test]
    fn positions_are_utf16() {
        let s = snap(&["src/école.rs"]);
        let r = rank_snap(&s, "cole", 1, &HashSet::new());
        assert_eq!(r.len(), 1);
        // 'c' is at byte 6, UTF-16 unit 5 (é is 2 bytes, 1 unit).
        assert_eq!(r[0].positions[0], 5);
    }
}
