//! GET /api/v1/events — server event stream (API.md § 12).
//! hello first, : ping every 15 s, metrics every 5 s with ?metrics=1,
//! resync on broadcast lag.

use axum::{
    extract::{Query, State},
    response::sse::{Event, Sse},
    routing::get,
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt as _;

use crate::state::AppState;

#[derive(Deserialize, Default)]
struct Params {
    /// API.md § 12 spells it `?metrics=1`; `true` is accepted too.
    metrics: Option<String>,
}

fn flag(v: Option<&str>) -> bool {
    matches!(v, Some("1" | "true"))
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/events", get(events))
}

async fn events(
    State(s): State<Arc<AppState>>,
    Query(p): Query<Params>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let mut bus_rx = s.bus.subscribe();
    let ws = s.ws();
    let (files, ms) = ws.index.stats();
    let hello = Event::default()
        .event("hello")
        .id("0")
        .json_data(serde_json::json!({
            "api": 1, "version": s.version,
            "workspaceKey": ws.key,
            "generation": ws.generation.load(std::sync::atomic::Ordering::Relaxed),
            "files": files, "ms": ms,
        }))
        .unwrap();
    let _ = tx.send(hello);
    let want_metrics = flag(p.metrics.as_deref());
    let started = s.started_at;
    tokio::spawn(async move {
        let mut ping = tokio::time::interval(std::time::Duration::from_secs(15));
        let mut metrics_tick = tokio::time::interval(std::time::Duration::from_secs(5));
        let mut seq = 0u64;
        loop {
            tokio::select! {
                _ = ping.tick() => {
                    if tx.send(Event::default().comment("ping")).is_err() { break; }
                }
                _ = metrics_tick.tick(), if want_metrics => {
                    seq += 1;
                    let m = crate::v1::metrics::snapshot(&started);
                    let ev = Event::default().event("metrics").id(seq.to_string())
                        .json_data(serde_json::json!(m)).unwrap();
                    if tx.send(ev).is_err() { break; }
                }
                msg = bus_rx.recv() => {
                    match msg {
                        Ok((id, ev)) => {
                            let name = event_name(&ev);
                            let data = event_data(&ev);
                            if tx.send(Event::default().event(name).id(id.to_string()).json_data(data).unwrap()).is_err() { break; }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            seq += 1;
                            if tx.send(Event::default().event("resync").id(seq.to_string()).json_data(serde_json::json!({})).unwrap()).is_err() { break; }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });
    Sse::new(UnboundedReceiverStream::new(rx).map(Ok)).keep_alive(
        axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(15)),
    )
}

/// Event data exactly as API.md § 12 lists it: payload events carry the
/// object itself (`git` a GitStatus, `job` a Job, `pr` a PrMeta), never a
/// `{status: …}`-style wrapper; the rest are the variant's fields.
fn event_data(ev: &crate::bus::ServerEvent) -> serde_json::Value {
    use crate::bus::ServerEvent::*;
    match ev {
        Git { status } => status.clone(),
        Job { job } => job.clone(),
        Pr { pr } => pr.clone(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|mut v| v.get_mut("data").map(serde_json::Value::take))
            .unwrap_or(serde_json::Value::Null),
    }
}

fn event_name(ev: &crate::bus::ServerEvent) -> &'static str {
    use crate::bus::ServerEvent::*;
    match ev {
        Hello { .. } => "hello",
        Index { .. } => "index",
        Fs { .. } => "fs",
        Git { .. } => "git",
        Hl { .. } => "hl",
        Settings { .. } => "settings",
        Workspace { .. } => "workspace",
        Job { .. } => "job",
        Pr { .. } => "pr",
        Threads { .. } => "threads",
        Drafts { .. } => "drafts",
        Metrics { .. } => "metrics",
        Resync {} => "resync",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review fix: `?metrics=1` (the spec's form) and `true` both enable metrics.
    #[test]
    fn metrics_flag_accepts_spec_and_bool_forms() {
        assert!(flag(Some("1")));
        assert!(flag(Some("true")));
        assert!(!flag(Some("0")));
        assert!(!flag(Some("false")));
        assert!(!flag(None));
    }

    /// Review fix: SSE data matches API.md § 12 (bare payloads, camelCase).
    #[test]
    fn event_data_matches_the_spec() {
        use crate::bus::ServerEvent;
        let st = serde_json::json!({ "branch": "main", "counts": { "staged": 1 } });
        assert_eq!(event_data(&ServerEvent::Git { status: st.clone() }), st);
        let job = serde_json::json!({ "id": "j1", "kind": "pr.open" });
        assert_eq!(event_data(&ServerEvent::Job { job: job.clone() }), job);
        let idx = event_data(&ServerEvent::Index {
            state: "ready".into(),
            files: 3,
            ms: 1,
            generation: 2,
            search_index: "off".into(),
        });
        assert_eq!(idx["searchIndex"], "off");
        let hello = event_data(&ServerEvent::Hello {
            api: 1,
            version: "x".into(),
            workspace_key: "k".into(),
            generation: 0,
        });
        assert_eq!(hello["workspaceKey"], "k");
        let hl = event_data(&ServerEvent::Hl {
            path: "a.rs".into(),
            mtime_ms: 5,
        });
        assert_eq!(hl["mtimeMs"], 5);
        assert_eq!(event_data(&ServerEvent::Resync {}), serde_json::json!({}));
    }
}
