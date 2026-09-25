//! Line-offset index: O(window) reads without scanning from the start.
//! Keyed by (path, mtime, size); byte-budgeted LRU over offset tables.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct LineIndex {
    inner: parking_lot::Mutex<lru::LruCache<Key, Vec<u64>>>,
    budget_bytes: usize,
    used_bytes: parking_lot::Mutex<usize>,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct Key {
    path: PathBuf,
    mtime_ms: u128,
    size: u64,
}

impl LineIndex {
    pub fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(lru::LruCache::unbounded()),
            budget_bytes: 64 * 1024 * 1024,
            used_bytes: parking_lot::Mutex::new(0),
        }
    }

    fn table_bytes(nlines: usize) -> usize {
        (nlines + 1) * 8
    }

    /// Offsets of every line start, plus a sentinel end offset.
    pub fn offsets(&self, path: &Path) -> std::io::Result<Vec<u64>> {
        let md = std::fs::metadata(path)?;
        let mtime_ms = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let size = md.len();
        let key = Key {
            path: path.to_path_buf(),
            mtime_ms,
            size,
        };
        if let Some(t) = self.inner.lock().get(&key) {
            return Ok(t.clone());
        }
        let bytes = std::fs::read(path)?;
        let mut offs = Vec::new();
        offs.push(0u64);
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                offs.push((i + 1) as u64);
            }
        }
        // A trailing newline does not add a line (API.md § 4.2).
        if offs.len() > 1 && offs.last() == Some(&(bytes.len() as u64)) {
            offs.pop();
        }
        {
            let mut used = self.used_bytes.lock();
            *used += Self::table_bytes(offs.len());
            let mut inner = self.inner.lock();
            inner.push(key, offs.clone());
            while *used > self.budget_bytes {
                if let Some((_, v)) = inner.pop_lru() {
                    *used = used.saturating_sub(Self::table_bytes(v.len()));
                } else {
                    break;
                }
            }
        }
        Ok(offs)
    }

    pub fn total_lines(&self, path: &Path) -> std::io::Result<usize> {
        Ok(self.offsets(path)?.len())
    }
}

impl Default for LineIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// (line number, raw line bytes) plus the total line count.
pub type WindowBytes = (Vec<(usize, Vec<u8>)>, usize);

/// Read one window using the offset table. Returns (lines, total).
pub fn read_window_bytes(path: &Path, from: usize, count: usize) -> std::io::Result<WindowBytes> {
    let idx = LineIndex::new();
    read_window_bytes_with(&idx, path, from, count)
}

pub fn read_window_bytes_with(
    idx: &LineIndex,
    path: &Path,
    from: usize,
    count: usize,
) -> std::io::Result<WindowBytes> {
    let offs = idx.offsets(path)?;
    let total = offs.len();
    if from == 0 || from > total {
        return Ok((vec![], total));
    }
    let start = (from - 1).min(total.saturating_sub(1));
    let end = (start + count).min(total);
    let mut f = std::fs::File::open(path)?;
    let mut out = Vec::with_capacity(end - start);
    let mut buf = Vec::new();
    for (i, n) in (start..end).enumerate() {
        let s = offs[start + i];
        let e = if n + 1 < total {
            offs[n + 1]
        } else {
            f.metadata()?.len()
        };
        let len = e.saturating_sub(s).min(1_000_000) as usize;
        buf.resize(len, 0);
        f.seek(SeekFrom::Start(s))?;
        let mut got = 0;
        while got < len {
            let r = f.read(&mut buf[got..])?;
            if r == 0 {
                break;
            }
            got += r;
        }
        buf.truncate(got);
        // Strip the terminator (\n or \r\n); the offset table already accounts for it.
        if buf.last() == Some(&b'\n') {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
        }
        out.push((n + 1, std::mem::take(&mut buf)));
    }
    let _ = offs;
    Ok((out, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "l1\nl2\nl3\n").unwrap();
        let idx = LineIndex::new();
        // Trailing newline does not add a line.
        assert_eq!(idx.total_lines(&p).unwrap(), 3);
        let (win, total) = read_window_bytes_with(&idx, &p, 2, 5).unwrap();
        assert_eq!(total, 3);
        assert_eq!(win.len(), 2);
        assert_eq!(win[0], (2, b"l2".to_vec()));
        assert_eq!(win[1], (3, b"l3".to_vec()));
        let (win, _) = read_window_bytes_with(&idx, &p, 99, 5).unwrap();
        assert!(win.is_empty());
    }

    #[test]
    fn crlf_terminators_are_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crlf.txt");
        std::fs::write(&p, "a\r\nb\r\nc").unwrap();
        let idx = LineIndex::new();
        let (win, total) = read_window_bytes_with(&idx, &p, 1, 3).unwrap();
        assert_eq!(total, 3);
        assert_eq!(
            win,
            vec![(1, b"a".to_vec()), (2, b"b".to_vec()), (3, b"c".to_vec())]
        );
    }

    #[test]
    fn no_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.txt");
        std::fs::write(&p, "a\nb").unwrap();
        let idx = LineIndex::new();
        assert_eq!(idx.total_lines(&p).unwrap(), 2);
    }
}
