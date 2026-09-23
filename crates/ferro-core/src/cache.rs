//! Persistent file-list cache: `{root}/.ferro/index.db` (sqlite).
//! Instant cold start: load cache synchronously in `Index::new`,
//! then background `rebuild()` re-walks and saves.
//! Invalidation: mtime (secs) + size per file; root walk still authoritative.

use std::path::Path;
use std::time::SystemTime;

use crate::index::FileEntry;

fn db_path(root: &Path) -> std::path::PathBuf {
    root.join(".ferro").join("index.db")
}

fn mtime_secs(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn load(root: &Path) -> Option<(Vec<FileEntry>, u128)> {
    let db = db_path(root);
    if !db.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open(&db).ok()?;
    let indexed_ms: u128 = conn
        .query_row("SELECT value FROM meta WHERE key='indexed_ms'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()?
        .parse()
        .unwrap_or(0);
    let mut stmt = conn
        .prepare("SELECT path, size FROM files ORDER BY path")
        .ok()?;
    let rows = stmt
        .query_map([], |r| {
            Ok(FileEntry {
                path: r.get(0)?,
                size: r.get::<_, i64>(1)? as u64,
            })
        })
        .ok()?;
    let mut out = Vec::new();
    for r in rows.flatten() {
        out.push(r);
    }
    if out.is_empty() {
        return None;
    }
    Some((out, indexed_ms))
}

pub fn save(root: &Path, entries: &[FileEntry], indexed_ms: u128) {
    let dir = root.join(".ferro");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let db = db_path(root);
    let Ok(conn) = rusqlite::Connection::open(&db) else {
        return;
    };
    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER);
         CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);",
    );
    // Best-effort atomic replace.
    let _ = conn.execute("DELETE FROM files", []);
    // Reuse a prepared insert outside the loop for speed on 95k files.
    if let Ok(mut ins) =
        conn.prepare("INSERT OR REPLACE INTO files(path,size,mtime) VALUES(?1,?2,?3)")
    {
        for e in entries {
            let full = root.join(&e.path);
            let mt = mtime_secs(&full);
            let _ = ins.execute(rusqlite::params![e.path, e.size as i64, mt]);
        }
    }
    let _ = conn.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES('indexed_ms',?1)",
        rusqlite::params![indexed_ms.to_string()],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let fp = dir.path().join("a.rs");
        std::fs::File::create(&fp)
            .unwrap()
            .write_all(b"fn main(){}")
            .unwrap();
        let entries = vec![crate::index::FileEntry {
            path: "a.rs".into(),
            size: 11,
        }];
        save(dir.path(), &entries, 7);
        let (back, ms) = load(dir.path()).unwrap();
        assert_eq!(ms, 7);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, "a.rs");
    }
}
