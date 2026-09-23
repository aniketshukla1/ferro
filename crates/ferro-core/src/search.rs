use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

pub fn grep(root: &std::path::Path, query: &str, limit: usize) -> Vec<Hit> {
    if query.is_empty() || limit == 0 {
        return vec![];
    }
    let q = query.as_bytes().to_vec();
    let qlow = query.to_ascii_lowercase();

    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .follow_links(false)
        .threads(num_cpus())
        .build();

    let paths: Vec<PathBuf> = walker
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path().to_path_buf())
        .filter(|p| {
            !p.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                s == ".git" || s == ".ferro" || s == "target" || s == "node_modules"
            })
        })
        .collect();

    let mut out = Vec::new();
    for path in paths {
        if out.len() >= limit {
            break;
        }
        // Fast reject: read bytes, check contains (case-sensitive first, then fold).
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes.len() > 2_000_000 {
            continue; // bgLimit parity: skip giant files in full scan
        }
        if !contains(&bytes, &q) && !contains_fold(&bytes, qlow.as_bytes()) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // Exact line pass with 32-rune leading snippet.
        for (i, line) in text.lines().enumerate() {
            if out.len() >= limit {
                break;
            }
            if line.to_ascii_lowercase().contains(&qlow) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                out.push(Hit {
                    path: rel,
                    line: i + 1,
                    text: snippet(line),
                });
                if out.len() >= 5 && !line.is_empty() {
                    // keep scanning same file but cap per-file to avoid spam
                }
            }
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    hay.windows(needle.len()).any(|w| w == needle)
}

fn contains_fold(hay: &[u8], needle_low: &[u8]) -> bool {
    if needle_low.is_empty() {
        return true;
    }
    // In-place ASCII lower scan without alloc.
    let n = needle_low.len();
    if hay.len() < n {
        return false;
    }
    for w in hay.windows(n) {
        let mut ok = true;
        for (a, b) in w.iter().zip(needle_low.iter()) {
            if a.to_ascii_lowercase() != *b {
                ok = false;
                break;
            }
        }
        if ok {
            return true;
        }
    }
    false
}

fn snippet(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= 240 {
        return line.to_string();
    }
    // Show window around match start — simplified to head.
    chars.into_iter().take(240).collect()
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}
