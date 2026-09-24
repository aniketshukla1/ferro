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
    metrics: Option<bool>,
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
    let want_metrics = p.metrics.unwrap_or(false);
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
                            let data = serde_json::to_string(&ev).unwrap_or_default();
                            // bus payloads are {event,data}; reserialize inner data only.
                            let inner = serde_json::from_str::<serde_json::Value>(&data)
                                .ok().and_then(|v| v.get("data").cloned()).unwrap_or(serde_json::Value::Null);
                            if tx.send(Event::default().event(name).id(id.to_string()).json_data(inner).unwrap()).is_err() { break; }
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
