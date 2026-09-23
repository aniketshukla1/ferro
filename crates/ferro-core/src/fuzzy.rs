//! Two-pass O(n) fuzzy scorer, px0-compatible feel:
//! basename weight, word boundaries, camel humps, consecutive runs.

pub fn score(query: &str, path: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let p: Vec<char> = path.chars().collect();
    let pl: Vec<char> = path.chars().map(|c| c.to_ascii_lowercase()).collect();

    // Pass 1: subsequence check.
    let mut qi = 0;
    let mut hits: Vec<usize> = Vec::with_capacity(q.len());
    for (i, c) in pl.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            hits.push(i);
            qi += 1;
        }
    }
    if qi != q.len() {
        return None;
    }

    // Pass 2: tighten from the end.
    let mut tight = vec![0usize; q.len()];
    let mut pi = p.len();
    for k in (0..q.len()).rev() {
        let found = hits[k];
        let mut j = if k + 1 < q.len() {
            tight[k + 1].saturating_sub(1)
        } else {
            pi.saturating_sub(1)
        };
        // walk back to earliest tight position >= (k==0?0:tight[k-1]+1)
        let lo = if k == 0 { 0 } else { hits[k - 1] + 1 };
        let mut best = hits[k];
        let mut jj = std::cmp::min(j, p.len().saturating_sub(1));
        while jj >= lo && jj < p.len() {
            if pl[jj] == q[k] {
                best = jj;
                // prefer tighter (rightmost before next hit) — break on first from right
                // unless it breaks contiguity bonus; rightmost is already tightest.
                break;
            }
            if jj == 0 {
                break;
            }
            jj -= 1;
            let _ = (found, j);
            j = jj;
        }
        let _ = pi;
        tight[k] = best;
        pi = best;
    }

    let basename_start = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    let baselen = p.len().saturating_sub(basename_start) as i64;

    let mut s: i64 = 0;
    let mut prev: Option<usize> = None;
    for (k, &h) in tight.iter().enumerate() {
        let in_base = h >= basename_start;
        if k == 0 && in_base && h == basename_start {
            s += 20;
        }
        if in_base {
            s += 14;
        }
        // verbatim in basename
        if in_base && p[h] == query.chars().nth(k).unwrap_or(p[h]) {
            s += 4;
        }
        // word boundary / path boundary
        if h == 0 || p[h - 1] == '/' || p[h - 1] == '_' || p[h - 1] == '-' || p[h - 1] == ' ' {
            s += 16;
        }
        // camel hump: lower->Upper
        if h > 0 && p[h - 1].is_ascii_lowercase() && p[h].is_ascii_uppercase() {
            s += 14;
        }
        // consecutive
        if let Some(pr) = prev {
            if h == pr + 1 {
                s += 12;
            } else {
                let gap = (h - pr - 1).min(12) as i64;
                s -= gap;
            }
        }
        prev = Some(h);
    }
    // query verbatim in basename bonus
    let ql = query.to_ascii_lowercase();
    let base = path
        .get(basename_start..)
        .unwrap_or("")
        .to_ascii_lowercase();
    if base.contains(&ql) {
        s += 40;
    }

    s -= (path.len() as i64) / 8;
    s -= (path.matches('/').count() as i64) * 2;
    let _ = baselen;
    Some(s)
}

pub fn rank<'a>(query: &str, paths: &'a [String], limit: usize) -> Vec<(&'a str, i64)> {
    let mut v: Vec<(&str, i64)> = paths
        .iter()
        .filter_map(|p| score(query, p).map(|s| (p.as_str(), s)))
        .collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.1));
    v.truncate(limit);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basename_wins() {
        let a = score("srv", "src/server.rs").unwrap();
        let b = score("srv", "src/services/billing/reconcile_verbose.rs").unwrap();
        assert!(a > b);
    }
    #[test]
    fn subsequence_required() {
        assert!(score("zzz", "src/main.rs").is_none());
    }
}
