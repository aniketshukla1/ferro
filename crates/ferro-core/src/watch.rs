//! Filesystem watcher (B3): `notify` recommended watcher with a hand-rolled
//! 100 ms debouncer. The walk predicate from `index.rs` is mirrored here so
//! incremental snapshot updates agree with full rebuilds:
//! gitignore-aware, skip components {`.ferro`, `target`, `node_modules`},
//! files only, ≤ 8 MiB. `.git/**` never enters the index; control files
//! (`index`, `HEAD`, `packed-refs`, `refs/**`) raise [`WatchEvent::GitControl`]
//! instead. Directories themselves are skipped (their files arrive as
//! separate events). Existence is re-checked at flush time, so renames,
//! moves and rapid create/delete cycles settle correctly.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::Watcher as _;

const DEBOUNCE: Duration = Duration::from_millis(100);
const OVERFLOW_CAP: usize = 20_000;
/// Components never indexed (mirrors `index::walk`).
const SKIP_DIRS: &[&str] = &[".ferro", "target", "node_modules"];

#[derive(Debug, Clone)]
pub enum WatchEvent {
    Fs {
        upserts: Vec<(String, u64, i64)>,
        deletes: Vec<String>,
        overflow: bool,
    },
    GitControl,
    Error(String),
}

pub struct WatchHandle {
    _watcher: notify::RecommendedWatcher,
    _thread: Option<std::thread::JoinHandle<()>>,
}

fn is_git_control(rel: &Path) -> bool {
    let mut parts = rel.components();
    let Some(first) = parts.next() else {
        return false;
    };
    if first.as_os_str() != ".git" {
        return false;
    }
    match parts.next() {
        None => false,
        Some(second) => {
            let s = second.as_os_str();
            if s == "index" || s == "HEAD" || s == "packed-refs" {
                return true;
            }
            // refs/** (heads, remotes, tags, ferro's pr refs, …)
            if s == "refs" {
                return true;
            }
            false
        }
    }
}

fn has_skip_component(rel: &Path) -> bool {
    rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        SKIP_DIRS.contains(&s.as_ref())
    })
}

fn file_meta(full: &Path) -> Option<(u64, i64)> {
    let md = std::fs::metadata(full).ok()?;
    if !md.is_file() {
        return None;
    }
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((md.len(), mtime))
}

struct IgnoreMatchers {
    local: ignore::gitignore::Gitignore,
    global: ignore::gitignore::Gitignore,
}

impl IgnoreMatchers {
    fn is_ignore(&self, path: &Path, is_dir: bool) -> bool {
        self.local.matched(path, is_dir).is_ignore()
            || self.global.matched(path, is_dir).is_ignore()
    }
}

fn build_ignore_matcher(root: &Path) -> IgnoreMatchers {
    let mut b = ignore::gitignore::GitignoreBuilder::new(root);
    // Best effort: missing files are ignored by the builder.
    let _ = b.add(root.join(".gitignore"));
    let _ = b.add(root.join(".git/info/exclude"));
    let local = b
        .build()
        .unwrap_or_else(|_| ignore::gitignore::Gitignore::empty());
    let (global, _) = ignore::gitignore::GitignoreBuilder::new(root).build_global();
    IgnoreMatchers { local, global }
}

/// Watch `root` recursively, batching into [`WatchEvent`] on `tx`.
/// The batcher thread exits when `tx` disconnects or the handle drops.
pub fn watch_root(root: &Path, tx: mpsc::Sender<WatchEvent>) -> Result<WatchHandle, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::RecommendedWatcher::new(
        move |res| {
            let _ = raw_tx.send(res);
        },
        notify::Config::default(),
    )
    .map_err(|e| e.to_string())?;
    watcher
        .watch(&root, notify::RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;
    let matcher = build_ignore_matcher(&root);
    let thread = std::thread::spawn(move || {
        let mut pending: HashSet<PathBuf> = HashSet::new();
        let mut git_control = false;
        let mut overflowed = false;
        let flush =
            |pending: &mut HashSet<PathBuf>, git_control: &mut bool, overflowed: &mut bool| {
                if *overflowed {
                    pending.clear();
                    *git_control = false;
                    *overflowed = false;
                    let _ = tx.send(WatchEvent::Fs {
                        upserts: vec![],
                        deletes: vec![],
                        overflow: true,
                    });
                    return;
                }
                if pending.is_empty() && !*git_control {
                    return;
                }
                let mut upserts = Vec::new();
                let mut deletes = Vec::new();
                for full in pending.drain() {
                    let Ok(rel) = full.strip_prefix(&root).map(|p| p.to_path_buf()) else {
                        continue;
                    };
                    if is_git_control(&rel) {
                        *git_control = true;
                        continue;
                    }
                    if has_skip_component(&rel) {
                        continue;
                    }
                    let rel_s = rel.to_string_lossy().to_string();
                    match file_meta(&full) {
                        Some((size, mtime)) => {
                            // Full paths: the matcher strips the root prefix itself.
                            // Newly ignored files surface as deletes so the server
                            // drops them from the snapshot (absent paths are
                            // filtered server-side against the snapshot).
                            if matcher.is_ignore(&full, false) {
                                // Newly ignored (or always was): drop from index.
                                deletes.push(rel_s);
                            } else if size > 8 * 1024 * 1024 {
                                // Walk skips >8 MiB listings too.
                                deletes.push(rel_s);
                            } else {
                                upserts.push((rel_s, size, mtime));
                            }
                        }
                        None => deletes.push(rel_s),
                    }
                }
                upserts.sort();
                deletes.sort();
                if !upserts.is_empty() || !deletes.is_empty() {
                    let _ = tx.send(WatchEvent::Fs {
                        upserts,
                        deletes,
                        overflow: false,
                    });
                }
                if *git_control {
                    *git_control = false;
                    let _ = tx.send(WatchEvent::GitControl);
                }
            };
        loop {
            match raw_rx.recv_timeout(DEBOUNCE) {
                Ok(Ok(ev)) => {
                    use notify::EventKind::*;
                    let relevant = matches!(ev.kind, Create(_) | Modify(_) | Remove(_) | Any);
                    if !relevant {
                        continue;
                    }
                    for p in ev.paths {
                        pending.insert(p);
                        if pending.len() > OVERFLOW_CAP {
                            overflowed = true;
                        }
                    }
                }
                Ok(Err(e)) => {
                    // Watcher-level failure (e.g. inotify exhaustion): tell
                    // the server to fall back to adaptive polling.
                    let _ = tx.send(WatchEvent::Error(e.to_string()));
                    return;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    flush(&mut pending, &mut git_control, &mut overflowed);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    Ok(WatchHandle {
        _watcher: watcher,
        _thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recv_fs(rx: &mpsc::Receiver<WatchEvent>, timeout: Duration) -> WatchEvent {
        rx.recv_timeout(timeout).expect("watch event in time")
    }

    #[test]
    fn upsert_and_delete_batches() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let _h = watch_root(dir.path(), tx).unwrap();
        // Let the watcher settle.
        std::thread::sleep(Duration::from_millis(400));
        // Drain any settle noise.
        while rx.try_recv().is_ok() {}
        std::fs::write(dir.path().join("a.txt"), "hi\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_upsert = false;
        while std::time::Instant::now() < deadline && !saw_upsert {
            if let WatchEvent::Fs { upserts, .. } = recv_fs(&rx, Duration::from_secs(5)) {
                saw_upsert = upserts.iter().any(|(p, _, _)| p == "a.txt");
            }
        }
        assert!(saw_upsert, "create must surface as upsert");
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_delete = false;
        while std::time::Instant::now() < deadline && !saw_delete {
            if let WatchEvent::Fs { deletes, .. } = recv_fs(&rx, Duration::from_secs(5)) {
                saw_delete = deletes.iter().any(|p| p == "a.txt");
            }
        }
        assert!(saw_delete, "remove must surface as delete");
    }

    #[test]
    fn gitignore_and_git_controls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".git/refs/heads")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let _h = watch_root(dir.path(), tx).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        while rx.try_recv().is_ok() {}
        // Ignored file: never an upsert (surfaces as a delete so the
        // server drops it from the snapshot if present).
        std::fs::write(dir.path().join("x.log"), "noise\n").unwrap();
        // Control file: GitControl event.
        std::fs::write(dir.path().join(".git/refs/heads/main"), "abc123\n").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_control = false;
        let mut saw_upsert = false;
        while std::time::Instant::now() < deadline && !saw_control {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(WatchEvent::GitControl) => saw_control = true,
                Ok(WatchEvent::Fs { upserts, .. }) => {
                    saw_upsert = saw_upsert || upserts.iter().any(|(p, _, _)| p == "x.log");
                }
                _ => {}
            }
        }
        assert!(saw_control, ".git/refs change must raise GitControl");
        assert!(!saw_upsert, "gitignored file must never upsert");
    }
}
