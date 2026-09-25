//! Long-operation registry with per-job cancellation (API.md § 7.1).
//! Jobs: index.rebuild, pr.open, pr.refresh, ai.review, harness.edit, search.index.

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub state: JobState,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(rename = "endedAt", skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
    #[serde(skip)]
    pub cancel: Option<tokio_util::sync::CancellationToken>,
}

impl Job {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            id: format!("j_{}", ulid::Ulid::new()),
            kind: kind.into(),
            state: JobState::Queued,
            started_at: now_iso(),
            ended_at: None,
            progress: None,
            result: None,
            error: None,
            cancel: None,
        }
    }
}

pub fn now_iso() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

#[derive(Debug, Default)]
struct Inner {
    order: VecDeque<String>,
    by_id: std::collections::HashMap<String, Job>,
}

#[derive(Debug, Clone, Default)]
pub struct JobManager {
    inner: Arc<parking_lot::Mutex<Inner>>,
}

impl JobManager {
    pub fn register(&self, mut job: Job, cancel: tokio_util::sync::CancellationToken) -> Job {
        job.cancel = Some(cancel);
        let mut inner = self.inner.lock();
        if inner.order.len() >= 64 {
            if let Some(old) = inner.order.pop_front() {
                inner.by_id.remove(&old);
            }
        }
        inner.order.push_back(job.id.clone());
        inner.by_id.insert(job.id.clone(), job.clone());
        job
    }

    pub fn update(&self, id: &str, f: impl FnOnce(&mut Job)) -> Option<Job> {
        let mut inner = self.inner.lock();
        let job = inner.by_id.get_mut(id)?;
        f(job);
        Some(job.clone())
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        self.inner.lock().by_id.get(id).cloned()
    }

    pub fn cancel(&self, id: &str) -> Option<Job> {
        let token = self
            .inner
            .lock()
            .by_id
            .get(id)
            .and_then(|j| j.cancel.clone());
        if let Some(t) = token {
            t.cancel();
        }
        self.update(id, |j| {
            if j.state == JobState::Queued || j.state == JobState::Running {
                j.state = JobState::Cancelled;
                j.ended_at = Some(now_iso());
            }
        })
    }

    pub fn list(&self) -> Vec<Job> {
        let inner = self.inner.lock();
        inner
            .order
            .iter()
            .filter_map(|id| inner.by_id.get(id).cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle() {
        let m = JobManager::default();
        let token = tokio_util::sync::CancellationToken::new();
        let j = m.register(Job::new("index.rebuild"), token);
        assert!(j.id.starts_with("j_"));
        m.update(&j.id, |x| x.state = JobState::Running);
        assert_eq!(m.get(&j.id).unwrap().state, JobState::Running);
        m.cancel(&j.id);
        assert_eq!(m.get(&j.id).unwrap().state, JobState::Cancelled);
        assert_eq!(m.list().len(), 1);
    }
}
