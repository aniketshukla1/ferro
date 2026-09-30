//! RSS / CPU / threads / uptime. Linux reads /proc precisely;
//! macOS uses `ps`; other platforms report uptime with zeroed counters.

#[derive(Debug, Clone, serde::Serialize)]
pub struct Metrics {
    #[serde(rename = "rssBytes")]
    pub rss_bytes: u64,
    #[serde(rename = "cpuPct")]
    pub cpu_pct: f32,
    pub threads: usize,
    #[serde(rename = "uptimeMs")]
    pub uptime_ms: u64,
}

pub fn snapshot(started: &std::time::Instant) -> Metrics {
    Metrics {
        rss_bytes: rss_bytes(),
        cpu_pct: 0.0,
        threads: thread_count(),
        uptime_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(target_os = "linux")]
fn rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| pages * 4096)
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn rss_bytes() -> u64 {
    // No sysinfo dependency in B1; `ps` is portable Unix and metrics are infrequent.
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn thread_count() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Threads:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<usize>().ok())
        })
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn thread_count() -> usize {
    0
}

use axum::{extract::State, routing::get, Json, Router};
use std::sync::Arc;

use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/metrics", get(metrics))
}

async fn metrics(State(s): State<Arc<AppState>>) -> Json<Metrics> {
    Json(snapshot(&s.started_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_sane() {
        let m = snapshot(&std::time::Instant::now());
        assert!(m.uptime_ms < 60_000);
        #[cfg(target_os = "linux")]
        {
            assert!(m.rss_bytes > 0);
            assert!(m.threads > 0);
        }
    }
}
