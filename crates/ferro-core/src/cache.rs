//! Persistent file-list cache under the ferro cache dir
//! (`cache_dir/workspaces/<key>/files.db`), never inside the repo.
//! Instant cold start: load cache synchronously in `Index::new`,
//! then background `rebuild()` re-walks and saves.
//! Invalidation: mtime (secs) + size per file; root walk still authoritative.

use std::path::Path;

use crate::dirs::FerroDirs;
use crate::index::FileEntry;

fn db_path(dirs: &FerroDirs, key: &str) -> std::path::PathBuf {
    dirs.workspace_cache_dir(key).join("files.db")
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
        .prepare("SELECT path, size, mtime FROM files ORDER BY path")
        .ok()?;
    let rows = stmt
        .query_map([], |r| {
            Ok(FileEntry {
                path: r.get(0)?,
                size: r.get::<_, i64>(1)? as u64,
                mtime: r.get::<_, i64>(2).unwrap_or(0),
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
    let t0 = std::time::Instant::now();
    let dir = dirs.workspace_cache_dir(key);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let db = db_path(dirs, key);
    let Ok(mut conn) = rusqlite::Connection::open(&db) else {
        return;
    };
    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER);
         CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);",
    );
    // Skip the write entirely when the list is unchanged (D10).
    let hash = list_hash(entries);
    let stored: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='list_hash'", [], |r| {
            r.get(0)
        })
        .ok();
    if stored.as_deref() == Some(hash.as_str()) {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES('indexed_ms',?1)",
            rusqlite::params![indexed_ms.to_string()],
        );
        tracing::info!("cache save skipped (unchanged): {} rows", entries.len());
        return;
    }
    // One transaction for the whole replace (D10: was N autocommits).
    let tx = match conn.transaction() {
        Ok(tx) => tx,
        Err(_) => return,
    };
    let _ = tx.execute("DELETE FROM files", []);
    // mtime comes from the walk metadata — no second stat pass (D10).
    if let Ok(mut ins) =
        tx.prepare("INSERT OR REPLACE INTO files(path,size,mtime) VALUES(?1,?2,?3)")
    {
        for e in entries {
            let _ = ins.execute(rusqlite::params![e.path, e.size as i64, e.mtime]);
        }
    }
    let _ = tx.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES('indexed_ms',?1)",
        rusqlite::params![indexed_ms.to_string()],
    );
    let _ = tx.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES('list_hash',?1)",
        rusqlite::params![hash],
    );
    let _ = tx.commit();
    tracing::info!(
        "cache save: {} rows in {} ms",
        entries.len(),
        t0.elapsed().as_millis()
    );
    let _ = root; // mtimes arrive via entries; root kept for API symmetry.
}

/// Hash of (path, size, mtime) for the skip-unchanged fast path.
fn list_hash(entries: &[FileEntry]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    entries.len().hash(&mut h);
    for e in entries {
        e.path.hash(&mut h);
        e.size.hash(&mut h);
        e.mtime.hash(&mut h);
    }
    format!("{:016x}", h.finish())
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
            mtime: 0,
        }];
        save_in(dir.path(), &dirs, &key, &entries, 7);
        // Nothing may be written into the repo itself.
        assert!(!dir.path().join(".ferro").exists());
        let (back, ms) = load_in(&dirs, &key).unwrap();
        assert_eq!(ms, 7);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, "a.rs");
        // mtime round-trips; second identical save is a fast path.
        let entries2 = vec![crate::index::FileEntry {
            path: "a.rs".into(),
            size: 11,
            mtime: 12345,
        }];
        save_in(dir.path(), &dirs, &key, &entries2, 8);
        let (back2, _) = load_in(&dirs, &key).unwrap();
        assert_eq!(back2[0].mtime, 12345);
        save_in(dir.path(), &dirs, &key, &entries2, 9);
        let (back3, ms3) = load_in(&dirs, &key).unwrap();
        assert_eq!((back3.len(), ms3), (1, 9));
    }
}
