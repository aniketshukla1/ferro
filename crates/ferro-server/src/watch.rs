//! Live updates (B3): fs events, incremental snapshots, status refresh.
//! The core watcher batches filesystem changes; this loop applies them to
//! the snapshot (bumping `generation`), publishes `fs` events, and refreshes
//! git status on control-file changes (emitting `git` only when the status
//! hash changed). When watching fails, an adaptive poller
//! (`clamp(4 × last status time, 1 s, 15 s)`) keeps status fresh.
//!
//! A watcher belongs to one workspace: opening another one calls
//! [`restart`], and a loop whose workspace is no longer current exits
//! instead of applying stale events to the new one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bus::ServerEvent;
use crate::state::{AppState, Workspace};

/// `(path, size, mtime)` as the index stores it.
pub type FileStat = (String, u64, i64);

/// Apply one fs batch to `ws`'s snapshot. Upserts are re-checked on disk
/// (regular files ≤ 8 MiB, like the walk); a delete removes the path and
/// everything below it (a removed or moved-away directory arrives as one
/// path), and deletes of never-indexed paths are dropped. Returns the
/// applied `(upserts, removed paths)`, or `None` when nothing changed.
pub fn apply_fs_batch(
    ws: &Workspace,
    upserts: &[FileStat],
    deletes: &[String],
) -> Option<(Vec<FileStat>, Vec<String>)> {
    let upserts: Vec<(String, u64, i64)> = upserts
        .iter()
        .filter(|(p, size, _)| {
            if *size > 8 * 1024 * 1024 {
                return false;
            }
            // Re-check: the file may have vanished (or become a symlink,
            // which the walk never lists) since the batch was built.
            std::fs::symlink_metadata(ws.root.join(p))
                .map(|m| m.is_file())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    let mut removed = Vec::new();
    if !deletes.is_empty() {
        let snap = ws.index.file_index.load();
        // One sorted view per batch: exact hits and `dir/` prefix ranges by
        // binary search instead of a scan of the snapshot per delete.
        let mut sorted: Vec<&str> = snap.paths.iter().map(String::as_str).collect();
        if !sorted.is_sorted() {
            sorted.sort_unstable();
        }
        for d in deletes {
            if sorted.binary_search(&d.as_str()).is_ok() {
                removed.push(d.clone());
                continue;
            }
            let prefix = format!("{d}/");
            let start = sorted.partition_point(|p| *p < prefix.as_str());
            removed.extend(
                sorted[start..]
                    .iter()
                    .take_while(|p| p.starts_with(&prefix))
                    .map(|p| p.to_string()),
            );
        }
    }
    if upserts.is_empty() && removed.is_empty() {
        return None;
    }
    ws.index.file_index.apply_delta(&upserts, &removed);
    ws.generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some((upserts, removed))
}

/// Refresh `ws`'s git status; store + publish only when the hash changed.
/// Returns the refresh duration for the adaptive poller.
fn refresh_status(state: &Arc<AppState>, ws: &Workspace) -> Duration {
    let t0 = Instant::now();
    let Some(g) = ws.git.as_ref().map(|g| g.repo.clone()) else {
        return t0.elapsed();
    };
    let Ok(st) = g.status_v2() else {
        return t0.elapsed();
    };
    let hash = ferro_core::git::status_hash(&st);
    let changed = ws
        .git_status
        .read()
        .as_ref()
        .map(|old| ferro_core::git::status_hash(old) != hash)
        .unwrap_or(true);
    if changed {
        *ws.git_status.write() = Some(st.clone());
        if let Ok(v) = serde_json::to_value(&st) {
            state.bus.publish(ServerEvent::Git { status: v });
        }
    }
    t0.elapsed()
}

fn auto_refresh(state: &Arc<AppState>, ws: &Workspace) -> bool {
    state
        .settings
        .effective(&ws.key)
        .get("git.autoRefresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Still the workspace the server is showing?
fn is_current(state: &Arc<AppState>, ws: &Arc<Workspace>) -> bool {
    Arc::ptr_eq(&state.ws(), ws)
}

/// Start watching the current workspace (or the adaptive fallback).
/// Idempotent while a watcher is running.
pub fn start(state: &Arc<AppState>) {
    let mut slot = state.watch.lock();
    if slot.is_some() {
        return;
    }
    // Captured for overflow rebuilds (blocking threads have no context).
    let rt = tokio::runtime::Handle::try_current().ok();
    let ws = state.ws();
    let (tx, rx) = std::sync::mpsc::channel::<ferro_core::watch::WatchEvent>();
    match ferro_core::watch::watch_root(&ws.root, tx) {
        Ok(h) => {
            *slot = Some(h);
            let (s2, ws2) = (state.clone(), ws.clone());
            std::thread::spawn(move || watch_loop(&s2, &ws2, rx, rt));
        }
        Err(e) => {
            tracing::warn!("watcher unavailable ({e}); adaptive status polling instead");
            drop(rx);
            fallback_poll(state, &ws);
        }
    }
    drop(slot);
    // Prime the status cache once (also feeds the first tree render).
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        let s2 = state.clone();
        h.spawn(async move {
            tokio::task::spawn_blocking(move || refresh_status(&s2, &ws))
                .await
                .ok();
        });
    }
}

/// Point live updates at the workspace that was just opened. Dropping the
/// old handle stops its watcher; its loop sees the channel close and exits.
pub fn restart(state: &Arc<AppState>) {
    drop(state.watch.lock().take());
    start(state);
}

fn watch_loop(
    state: &Arc<AppState>,
    ws: &Arc<Workspace>,
    rx: std::sync::mpsc::Receiver<ferro_core::watch::WatchEvent>,
    rt: Option<tokio::runtime::Handle>,
) {
    let mut pending_status = false;
    let mut last_refresh = Instant::now() - Duration::from_secs(3600);
    // Worktree edits never touch .git control files, yet they change the
    // status (untracked/modified). Any fs batch therefore schedules a
    // refresh too, through the same 1 s coalescing.
    let note_change = |last_refresh: &mut Instant, pending_status: &mut bool| {
        if !auto_refresh(state, ws) {
            return;
        }
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            *last_refresh = Instant::now();
            refresh_status(state, ws);
        } else {
            *pending_status = true;
        }
    };
    loop {
        let ev = rx.recv_timeout(Duration::from_secs(1));
        if !is_current(state, ws) {
            return;
        }
        match ev {
            Ok(ferro_core::watch::WatchEvent::Fs {
                upserts,
                deletes,
                overflow,
            }) => {
                if overflow {
                    // Too many changes (or an edited ignore file): full
                    // rebuild + overflow event, and a status refresh since
                    // the batch may have hidden a branch switch.
                    if let Some(h) = rt.as_ref() {
                        let ws2 = ws.clone();
                        h.spawn(async move {
                            ws2.index.rebuild().await;
                        });
                    }
                    ws.generation
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    state.bus.publish(ServerEvent::Fs {
                        changes: vec![],
                        overflow: true,
                    });
                    note_change(&mut last_refresh, &mut pending_status);
                    continue;
                }
                if let Some((upserts, removed)) = apply_fs_batch(ws, &upserts, &deletes) {
                    let changes = upserts
                        .iter()
                        .map(|(p, _, _)| crate::bus::FileChange {
                            path: p.clone(),
                            kind: "modify".into(),
                        })
                        .chain(removed.iter().map(|p| crate::bus::FileChange {
                            path: p.clone(),
                            kind: "delete".into(),
                        }))
                        .collect();
                    state.bus.publish(ServerEvent::Fs {
                        changes,
                        overflow: false,
                    });
                    // Freshness for the trigram index (delta + threshold).
                    {
                        let ws = state.ws();
                        let ups: Vec<String> = upserts.iter().map(|(p, _, _)| p.clone()).collect();
                        ws.search.note_changes(&ups, &deletes);
                        ws.symbols.note_changes(&ups, &deletes);
                    }
                    state.ensure_search_built();
                    state.ensure_symbols_built();
                    note_change(&mut last_refresh, &mut pending_status);
                }
            }
            Ok(ferro_core::watch::WatchEvent::GitControl) => {
                note_change(&mut last_refresh, &mut pending_status);
            }
            Ok(ferro_core::watch::WatchEvent::Error(e)) => {
                tracing::warn!("watcher failed ({e}); adaptive status polling instead");
                fallback_poll(state, ws);
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if pending_status {
                    pending_status = false;
                    if auto_refresh(state, ws) {
                        last_refresh = Instant::now();
                        refresh_status(state, ws);
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Fallback when watching is unavailable: poll status adaptively for as
/// long as `ws` is the current workspace.
fn fallback_poll(state: &Arc<AppState>, ws: &Arc<Workspace>) {
    let (s2, ws2) = (state.clone(), ws.clone());
    std::thread::spawn(move || {
        while is_current(&s2, &ws2) {
            if !auto_refresh(&s2, &ws2) {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
            let dur = refresh_status(&s2, &ws2);
            // interval = clamp(4 × last status time, 1 s, 15 s).
            let next = (dur * 4).clamp(Duration::from_secs(1), Duration::from_secs(15));
            std::thread::sleep(next);
        }
    });
}
