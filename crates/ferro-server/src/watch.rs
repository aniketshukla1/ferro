//! Live updates (B3): fs events, incremental snapshots, status refresh.
//! The core watcher batches filesystem changes; this loop applies them to
//! the snapshot (bumping `generation`), publishes `fs` events, and refreshes
//! git status on control-file changes (emitting `git` only when the status
//! hash changed). When watching fails, an adaptive poller
//! (`clamp(4 × last status time, 1 s, 15 s)`) keeps status fresh.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bus::ServerEvent;
use crate::state::AppState;

/// Apply one fs batch: filter deletes against the snapshot (kills noise from
/// never-indexed paths), update sizes/mtimes, bump generation. Returns true
/// when the snapshot actually changed.
pub fn apply_fs_batch(
    state: &Arc<AppState>,
    upserts: &[(String, u64, i64)],
    deletes: &[String],
) -> bool {
    let ws = state.ws();
    let snap = ws.index.file_index.load();
    let in_index = |p: &str| snap.paths.iter().any(|x| x == p);
    let upserts: Vec<(String, u64, i64)> = upserts
        .iter()
        .filter(|(p, size, _)| {
            // Drop upserts for paths the walk would skip (race with delete).
            if *size > 8 * 1024 * 1024 {
                return false;
            }
            let full = ws.root.join(p);
            // Re-check existence: the file may have vanished since the batch.
            std::fs::metadata(&full)
                .map(|m| m.is_file())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    let deletes: Vec<String> = deletes.iter().filter(|p| in_index(p)).cloned().collect();
    if upserts.is_empty() && deletes.is_empty() {
        return false;
    }
    ws.index.file_index.apply_delta(&upserts, &deletes);
    ws.generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    true
}

/// Refresh git status; store + publish only when the hash changed.
/// Returns the refresh duration for the adaptive poller.
fn refresh_status(state: &Arc<AppState>) -> Duration {
    let t0 = Instant::now();
    let ws = state.ws();
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

fn auto_refresh(state: &Arc<AppState>) -> bool {
    let ws = state.ws();
    state
        .settings
        .effective(&ws.key)
        .get("git.autoRefresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Start watching (or the adaptive fallback). Idempotent per AppState.
pub fn start(state: &Arc<AppState>) {
    if state.watch.lock().is_some() {
        return;
    }
    // Captured for overflow rebuilds (blocking threads have no context).
    let rt = tokio::runtime::Handle::try_current().ok();
    let ws = state.ws();
    let (tx, rx) = std::sync::mpsc::channel::<ferro_core::watch::WatchEvent>();
    match ferro_core::watch::watch_root(&ws.root, tx) {
        Ok(h) => {
            *state.watch.lock() = Some(h);
            let s2 = state.clone();
            std::thread::spawn(move || watch_loop(&s2, rx, rt));
        }
        Err(e) => {
            tracing::warn!("watcher unavailable ({e}); adaptive status polling instead");
            drop(rx);
            fallback_poll(state);
        }
    }
    // Prime the status cache once (also feeds the first tree render).
    let s2 = state.clone();
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        h.spawn(async move {
            let s3 = s2.clone();
            tokio::task::spawn_blocking(move || refresh_status(&s3))
                .await
                .ok();
        });
    }
}

fn watch_loop(
    state: &Arc<AppState>,
    rx: std::sync::mpsc::Receiver<ferro_core::watch::WatchEvent>,
    rt: Option<tokio::runtime::Handle>,
) {
    let mut pending_status = false;
    let mut last_refresh = Instant::now() - Duration::from_secs(3600);
    // Worktree edits never touch .git control files, yet they change the
    // status (untracked/modified). Any fs batch therefore schedules a
    // refresh too, through the same 1 s coalescing.
    let note_change =
        |state: &Arc<AppState>, last_refresh: &mut Instant, pending_status: &mut bool| {
            if !auto_refresh(state) {
                return;
            }
            if last_refresh.elapsed() >= Duration::from_secs(1) {
                *last_refresh = Instant::now();
                refresh_status(state);
            } else {
                *pending_status = true;
            }
        };
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(ferro_core::watch::WatchEvent::Fs {
                upserts,
                deletes,
                overflow,
            }) => {
                if overflow {
                    // Too many changes: full rebuild + overflow event.
                    let ws = state.ws();
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
                    continue;
                }
                let changed = apply_fs_batch(state, &upserts, &deletes);
                if changed {
                    let changes = upserts
                        .iter()
                        .map(|(p, _, _)| crate::bus::FileChange {
                            path: p.clone(),
                            kind: "modify".into(),
                        })
                        .chain(deletes.iter().map(|p| crate::bus::FileChange {
                            path: p.clone(),
                            kind: "delete".into(),
                        }))
                        .collect();
                    state.bus.publish(ServerEvent::Fs {
                        changes,
                        overflow: false,
                    });
                    note_change(state, &mut last_refresh, &mut pending_status);
                }
            }
            Ok(ferro_core::watch::WatchEvent::GitControl) => {
                note_change(state, &mut last_refresh, &mut pending_status);
            }
            Ok(ferro_core::watch::WatchEvent::Error(e)) => {
                tracing::warn!("watcher failed ({e}); adaptive status polling instead");
                fallback_poll(state);
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if pending_status {
                    pending_status = false;
                    if auto_refresh(state) {
                        last_refresh = Instant::now();
                        refresh_status(state);
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Fallback when watching is unavailable: poll status adaptively.
fn fallback_poll(state: &Arc<AppState>) {
    let s2 = state.clone();
    std::thread::spawn(move || loop {
        if !auto_refresh(&s2) {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        let dur = refresh_status(&s2);
        // interval = clamp(4 × last status time, 1 s, 15 s).
        let next = (dur * 4).clamp(Duration::from_secs(1), Duration::from_secs(15));
        std::thread::sleep(next);
    });
}
