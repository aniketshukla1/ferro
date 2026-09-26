//! Server event bus: `tokio::sync::broadcast` with sequence numbers.
//! Slow consumers get `resync` and refetch all state (API.md § 12).

use serde::Serialize;

/// Field names go out in camelCase, like every other API payload
/// (`workspaceKey`, `searchIndex`, `mtimeMs`). SSE data is built from this
/// by `v1::events`, which also unwraps the single-payload variants.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum ServerEvent {
    Hello {
        api: u8,
        version: String,
        workspace_key: String,
        generation: u64,
    },
    Index {
        state: String,
        files: usize,
        ms: u128,
        generation: u64,
        search_index: String,
    },
    Fs {
        changes: Vec<FileChange>,
        overflow: bool,
    },
    Git {
        status: serde_json::Value,
    },
    Hl {
        path: String,
        mtime_ms: u64,
    },
    Settings {
        values: serde_json::Value,
    },
    Workspace {
        key: String,
        root: String,
        mode: String,
    },
    Job {
        job: serde_json::Value,
    },
    Pr {
        pr: serde_json::Value,
    },
    Threads {
        changed: bool,
    },
    Drafts {
        drafts: serde_json::Value,
    },
    Metrics {
        rss_bytes: u64,
        cpu_pct: f32,
        threads: usize,
        uptime_ms: u64,
    },
    Resync {},
}

#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    pub path: String,
    pub kind: String,
}

/// Publish/subscribe helper owned by AppState.
#[derive(Debug, Clone)]
pub struct Events {
    tx: tokio::sync::broadcast::Sender<(u64, ServerEvent)>,
    seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Events {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(1024);
        Self {
            tx,
            seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    pub fn publish(&self, ev: ServerEvent) {
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let _ = self.tx.send((seq, ev));
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<(u64, ServerEvent)> {
        self.tx.subscribe()
    }
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_increases() {
        let e = Events::new();
        let mut rx = e.subscribe();
        e.publish(ServerEvent::Threads { changed: true });
        e.publish(ServerEvent::Threads { changed: false });
        assert_eq!(rx.try_recv().unwrap().0, 1);
        assert_eq!(rx.try_recv().unwrap().0, 2);
    }

    #[test]
    fn lagged_receiver_detected() {
        let e = Events::new();
        let mut rx = e.subscribe();
        for _ in 0..1100 {
            e.publish(ServerEvent::Threads { changed: true });
        }
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
        ));
    }
}
