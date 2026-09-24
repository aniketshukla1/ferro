//! Persistent file-list cache under the ferro cache dir
//! (`cache_dir/workspaces/<key>/files.db`), never inside the repo.
//! Instant cold start: load cache synchronously in `Index::new`,
//! then background `rebuild()` re-walks and saves.
//! Invalidation: mtime (secs) + size per file; root walk still authoritative.

use std::path::Path;
use std::time::SystemTime;

use crate::dirs::FerroDirs;
use crate::index::FileEntry;

fn db_path(dirs: &FerroDirs, key: &str) -> std::path::PathBuf {
    dirs.workspace_cache_dir(key).join("files.db")
}

fn mtime_secs(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn load_in(dirs: &FerroDirs, key: &str) -> Option<(Vec<FileEntry>, u128)> {
    let db = db_path(dirs, key);
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

pub fn save_in(root: &Path, dirs: &FerroDirs, key: &str, entries: &[FileEntry], indexed_ms: u128) {
    let dir = dirs.workspace_cache_dir(key);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let db = db_path(dirs, key);
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
        let home = tempfile::tempdir().unwrap();
        let dirs = FerroDirs::new(
            home.path().join("c"),
            home.path().join("s"),
            home.path().join("h"),
        );
        let key = dirs.workspace_key(dir.path());
        let fp = dir.path().join("a.rs");
        std::fs::File::create(&fp)
            .unwrap()
            .write_all(b"fn main(){}")
            .unwrap();
        let entries = vec![crate::index::FileEntry {
            path: "a.rs".into(),
            size: 11,
        }];
        save_in(dir.path(), &dirs, &key, &entries, 7);
        // Nothing may be written into the repo itself.
        assert!(!dir.path().join(".ferro").exists());
        let (back, ms) = load_in(&dirs, &key).unwrap();
        assert_eq!(ms, 7);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, "a.rs");
    }
}
