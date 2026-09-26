//! Review state (B4, API.md § 9): drafts, viewed files, rounds.
//! One directory per PR (`reviews/<host>/<owner>/<repo>/<n>/`) or per
//! workspace (local notes outside PR mode). All JSON is written atomically.
//! Drafts whose lines vanished at the current head are flagged `stale` via
//! hunk mapping, never silently moved.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftSource {
    Human,
    Ai,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub path: String,
    pub line: usize,
    #[serde(rename = "startLine", skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    pub side: String,
    pub body: String,
    #[serde(rename = "threadId", skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub source: DraftSource,
    #[serde(rename = "findingId", skip_serializing_if = "Option::is_none")]
    pub finding_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(default)]
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDraft {
    pub path: String,
    pub line: usize,
    #[serde(rename = "startLine")]
    pub start_line: Option<usize>,
    pub side: Option<String>,
    pub body: String,
    #[serde(rename = "threadId")]
    pub thread_id: Option<String>,
    pub source: Option<DraftSource>,
    #[serde(rename = "findingId")]
    pub finding_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftPatch {
    pub body: Option<String>,
    pub line: Option<usize>,
    #[serde(rename = "startLine")]
    pub start_line: Option<usize>,
    pub side: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewedState {
    pub viewed: bool,
    #[serde(rename = "atSha")]
    pub at_sha: Option<String>,
    #[serde(rename = "changedSince")]
    pub changed_since: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Round {
    #[serde(rename = "headSha")]
    pub head_sha: String,
    pub at: String,
    pub kind: String,
}

/// Error text of an unknown draft id (the HTTP layer maps it to 404; other
/// patch errors are validation, 400).
pub const NO_DRAFT: &str = "no such draft";

pub struct ReviewStore {
    dir: PathBuf,
    /// Serializes read-modify-write of the JSON files: handlers run
    /// concurrently, and two unlocked adds would each drop the other's draft.
    lock: std::sync::Mutex<()>,
}

impl Clone for ReviewStore {
    fn clone(&self) -> Self {
        Self {
            dir: self.dir.clone(),
        }
    }
}

impl ReviewStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            lock: std::sync::Mutex::new(()),
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read<T: Default + for<'de> Deserialize<'de>>(&self, name: &str) -> T {
        let p = self.dir.join(name);
        let Ok(text) = std::fs::read_to_string(&p) else {
            return T::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    fn write(&self, name: &str, v: &impl Serialize) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
        ferro_core::settings::atomic_write(&self.dir.join(name), bytes.as_bytes())
    }

    // -- drafts ------------------------------------------------------------

    pub fn drafts(&self) -> Vec<Draft> {
        self.read::<Vec<Draft>>("drafts.json")
    }

    pub fn add(&self, mut d: NewDraft) -> Result<Draft, String> {
        d.path = d.path.trim().to_string();
        if d.path.is_empty() || d.path.len() > 512 || d.line == 0 {
            return Err("path and line>0 required".into());
        }
        if d.body.trim().is_empty() || d.body.len() > 100_000 {
            return Err("body required (≤100 KiB)".into());
        }
        if let Some(sl) = d.start_line {
            if sl == 0 || sl > d.line {
                return Err("startLine must satisfy 0 < startLine <= line".into());
            }
        }
        let side = d.side.unwrap_or_else(|| "RIGHT".into());
        if side != "LEFT" && side != "RIGHT" {
            return Err("side must be LEFT or RIGHT".into());
        }
        let now = now_iso();
        let _guard = self.locked();
        let mut all = self.drafts();
        let draft = Draft {
            id: new_id("d"),
            path: d.path,
            line: d.line,
            start_line: d.start_line,
            side,
            body: d.body,
            thread_id: d.thread_id,
            source: d.source.unwrap_or(DraftSource::Human),
            finding_id: d.finding_id,
            created_at: now.clone(),
            updated_at: now,
            stale: false,
        };
        all.push(draft.clone());
        self.write("drafts.json", &all)?;
        Ok(draft)
    }

    pub fn patch(&self, id: &str, p: &DraftPatch) -> Result<Draft, String> {
        let _guard = self.locked();
        let mut all = self.drafts();
        let d = all
            .iter_mut()
            .find(|x| x.id == id)
            .ok_or_else(|| NO_DRAFT.to_string())?;
        if let Some(b) = &p.body {
            if b.trim().is_empty() {
                return Err("body required".into());
            }
            d.body = b.clone();
        }
        if let Some(l) = p.line {
            if l == 0 {
                return Err("line>0 required".into());
            }
            d.line = l;
        }
        if let Some(sl) = p.start_line {
            d.start_line = Some(sl);
        }
        // Checked after both ends may have moved (a patch of `line` alone
        // must not leave startLine past it).
        if d.start_line.is_some_and(|sl| sl == 0 || sl > d.line) {
            return Err("startLine must satisfy 0 < startLine <= line".into());
        }
        if let Some(s) = &p.side {
            if s != "LEFT" && s != "RIGHT" {
                return Err("side must be LEFT or RIGHT".into());
            }
            d.side = s.clone();
        }
        d.stale = false;
        d.updated_at = now_iso();
        let out = d.clone();
        self.write("drafts.json", &all)?;
        Ok(out)
    }

    pub fn remove(&self, id: &str) -> bool {
        let _guard = self.locked();
        let mut all = self.drafts();
        let n = all.len();
        all.retain(|x| x.id != id);
        if all.len() == n {
            return false;
        }
        self.write("drafts.json", &all).is_ok()
    }

    pub fn clear(&self) {
        let _guard = self.locked();
        let _ = self.write("drafts.json", &Vec::<Draft>::new());
    }

    /// Map drafts across a head move; unmappable lines become `stale`.
    /// Returns the number of drafts marked stale.
    pub fn remap(
        &self,
        repo: &ferro_core::git::GitRepo,
        old_head: &str,
        new_head: &str,
    ) -> Result<usize, String> {
        let _guard = self.locked();
        let mut all = self.drafts();
        // Group by (path, side-is-base?) — LEFT drafts map through the old
        // side, RIGHT through the new side. Base rarely moves; a LEFT draft
        // on a path touched by the head diff goes stale (conservative).
        let mut stale = 0;
        // Cache per-path diffs (one git call per touched path).
        let mut diffs: std::collections::HashMap<String, Option<Vec<Hunk>>> =
            std::collections::HashMap::new();
        for d in all.iter_mut() {
            // A failed diff (old head gone after a force-push + gc, bad
            // path) means the mapping is unknown: `None` marks the draft
            // stale rather than keeping lines that may have moved.
            let hunks = diffs.entry(d.path.clone()).or_insert_with(|| {
                repo.run_bytes(&[
                    "diff",
                    "-U0",
                    "--no-color",
                    "--no-ext-diff",
                    old_head,
                    new_head,
                    "--",
                    &d.path,
                ])
                .ok()
                .map(|out| parse_zero_hunks(&out))
            });
            let mapped = hunks
                .as_ref()
                .map(|h| map_line(h, d.line, &d.side))
                .unwrap_or(None);
            // Multi-line drafts map both ends; either end lost → stale.
            let mapped_start = match d.start_line {
                Some(sl) => hunks
                    .as_ref()
                    .map(|h| map_line(h, sl, &d.side))
                    .unwrap_or(None),
                None => mapped,
            };
            match (mapped, mapped_start) {
                (Some(l), Some(sl)) => {
                    d.line = l;
                    d.start_line = d.start_line.map(|_| sl);
                    d.stale = false;
                }
                _ => {
                    if !d.stale {
                        d.stale = true;
                        stale += 1;
                    }
                }
            }
            d.updated_at = now_iso();
        }
        self.write("drafts.json", &all)?;
        Ok(stale)
    }

    // -- viewed ------------------------------------------------------------

    pub fn viewed(&self, head_sha: &str) -> serde_json::Value {
        let v: serde_json::Value = self.read("viewed.json");
        let files = v
            .get("files")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        serde_json::json!({ "headSha": head_sha, "files": files })
    }

    /// Mark a file viewed/unviewed at `head_sha`. Blob-based invalidation
    /// (B4): a file viewed at blob X whose blob changed reads back as
    /// `{viewed: false, changedSince: true}` — computed by `viewed_state`.
    pub fn set_viewed(&self, path: &str, viewed: bool, at_blob: Option<String>) -> ViewedState {
        let _guard = self.locked();
        let mut v: serde_json::Value = self.read("viewed.json");
        if !v.is_object() {
            v = serde_json::json!({});
        }
        let files = v
            .as_object_mut()
            .unwrap()
            .entry("files")
            .or_insert_with(|| serde_json::json!({}));
        let st = ViewedState {
            viewed,
            at_sha: at_blob,
            changed_since: false,
        };
        files[path] =
            serde_json::json!({ "viewed": st.viewed, "atSha": st.at_sha, "changedSince": false });
        let _ = self.write("viewed.json", &v);
        st
    }

    /// Resolve viewed state against the current blob: changed blob →
    /// `{viewed: false, changedSince: true}`. No blob on either side (a
    /// deleted file, still deleted) is unchanged.
    pub fn viewed_state(&self, path: &str, current_blob: Option<&str>) -> ViewedState {
        let v: serde_json::Value = self.read("viewed.json");
        let f = v.get("files").and_then(|m| m.get(path));
        let viewed = f
            .and_then(|x| x.get("viewed"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let at = f
            .and_then(|x| x.get("atSha"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        if !viewed {
            return ViewedState {
                viewed: false,
                at_sha: at,
                changed_since: false,
            };
        }
        match (at.as_deref(), current_blob) {
            (a, c) if a == c => ViewedState {
                viewed: true,
                at_sha: at,
                changed_since: false,
            },
            _ => ViewedState {
                viewed: false,
                at_sha: at,
                changed_since: true,
            },
        }
    }

    // -- rounds ------------------------------------------------------------

    pub fn rounds(&self) -> Vec<Round> {
        self.read::<Vec<Round>>("rounds.json")
    }

    pub fn record_round(&self, head_sha: &str, kind: &str) {
        let _guard = self.locked();
        let mut all = self.rounds();
        // One round per head (latest wins for repeats).
        all.retain(|r| r.head_sha != head_sha);
        all.push(Round {
            head_sha: head_sha.into(),
            at: now_iso(),
            kind: kind.into(),
        });
        let _ = self.write("rounds.json", &all);
    }

    pub fn last_reviewed_sha(&self) -> Option<String> {
        self.rounds()
            .into_iter()
            .rev()
            .find(|r| r.kind == "submitted")
            .map(|r| r.head_sha)
    }

    // -- AI findings (B5) ----------------------------------------------------
    // Persisted per head SHA in findings.json; dismiss flags stay on the
    // record so re-reviews of the same head keep them.

    pub fn findings(&self, head_sha: Option<&str>) -> Vec<Finding> {
        let all = self.read::<Vec<Finding>>("findings.json");
        match head_sha {
            Some(h) => all.into_iter().filter(|f| f.head_sha == h).collect(),
            None => all,
        }
    }

    pub fn finding(&self, id: &str) -> Option<Finding> {
        self.read::<Vec<Finding>>("findings.json")
            .into_iter()
            .find(|f| f.id == id)
    }

    /// Replace this head's findings (re-reviews overwrite, other heads stay).
    pub fn save_findings(&self, head_sha: &str, findings: &[Finding]) -> Result<(), String> {
        let mut all = self.read::<Vec<Finding>>("findings.json");
        all.retain(|f| f.head_sha != head_sha);
        all.extend(findings.iter().cloned());
        self.write("findings.json", &all)
    }

    pub fn dismiss_finding(&self, id: &str, reason: Option<String>) -> bool {
        let mut all = self.read::<Vec<Finding>>("findings.json");
        let Some(f) = all.iter_mut().find(|f| f.id == id) else {
            return false;
        };
        f.dismissed = true;
        f.dismiss_reason = reason.filter(|r| !r.trim().is_empty());
        self.write("findings.json", &all).is_ok()
    }
}

/// AI review finding (API.md § 10.3). `body` is markdown; `bodyHtml` is
/// rendered server-side. `dismissed` findings stay for audit, filtered
/// from fresh results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    #[serde(rename = "headSha")]
    pub head_sha: String,
    pub path: String,
    pub line: usize,
    #[serde(rename = "startLine", skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    pub side: String,
    pub severity: String,
    pub category: String,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    pub confidence: f64,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(default)]
    pub dismissed: bool,
    #[serde(rename = "dismissReason", skip_serializing_if = "Option::is_none")]
    pub dismiss_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct Hunk {
    old_start: usize,
    old_lines: usize,
    #[allow(dead_code)]
    new_start: usize,
    new_lines: usize,
}

fn parse_zero_hunks(out: &[u8]) -> Vec<Hunk> {
    let text = String::from_utf8_lossy(out);
    let mut hunks = Vec::new();
    for line in text.lines() {
        if !line.starts_with("@@ ") {
            continue;
        }
        let mut parts = line.split("@@");
        parts.next();
        let ranges = parts.next().unwrap_or("").trim();
        let mut it = ranges.split_whitespace();
        let (Some(o), Some(n)) = (it.next(), it.next()) else {
            continue;
        };
        let (os, ol) = parse_range(o.trim_start_matches('-'));
        let (ns, nl) = parse_range(n.trim_start_matches('+'));
        hunks.push(Hunk {
            old_start: os,
            old_lines: ol,
            new_start: ns,
            new_lines: nl,
        });
    }
    hunks
}

fn parse_range(s: &str) -> (usize, usize) {
    match s.split_once(',') {
        Some((a, b)) => (a.parse().unwrap_or(1), b.parse().unwrap_or(0)),
        None => (s.parse().unwrap_or(1), 1),
    }
}

/// Map a 1-based line through -U0 hunks. `side` selects the coordinate
/// space: RIGHT (head) lines map forward, LEFT (base) lines map when the
/// path is untouched, else stale (None).
///
/// Hunk conventions (verified against live git): a pure-insert hunk
/// `(os, 0)` inserts *after* old line `os`, shifting only lines `> os`;
/// a del/change hunk `(os, ol>0)` consumes `[os, os+ol)`.
fn map_line(hunks: &[Hunk], line: usize, side: &str) -> Option<usize> {
    if side == "LEFT" {
        // Base-side drafts: any hunk touching the path invalidates.
        return if hunks.is_empty() { Some(line) } else { None };
    }
    let mut delta: i64 = 0;
    for h in hunks {
        let (rs, rl) = (h.old_start, h.old_lines);
        if rl == 0 {
            if line <= rs {
                break;
            }
            delta += h.new_lines as i64;
            continue;
        }
        if line < rs {
            break;
        }
        if line < rs + rl {
            return None;
        }
        delta += h.new_lines as i64 - rl as i64;
    }
    Some((line as i64 + delta).max(1) as usize)
}

fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{prefix}_{:x}{:x}",
        t & 0xffffff,
        (n ^ std::process::id() as u64) & 0xffff
    )
}

fn now_iso() -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ReviewStore) {
        let dir = tempfile::tempdir().unwrap();
        let st = ReviewStore::new(dir.path().join("r"));
        (dir, st)
    }

    #[test]
    fn crud_validates() {
        let (_d, st) = store();
        let mk = |path: &str, line: usize| NewDraft {
            path: path.into(),
            line,
            start_line: None,
            side: None,
            body: "note".into(),
            thread_id: None,
            source: None,
            finding_id: None,
        };
        assert!(st.add(mk("", 1)).is_err());
        assert!(st.add(mk("a.rs", 0)).is_err());
        let d = st.add(mk("a.rs", 5)).unwrap();
        assert!(d.id.starts_with("d_") && d.side == "RIGHT" && !d.stale);
        assert_eq!(st.drafts().len(), 1);
        let p = st
            .patch(
                &d.id,
                &DraftPatch {
                    body: Some("edit".into()),
                    line: Some(6),
                    start_line: None,
                    side: None,
                },
            )
            .unwrap();
        assert_eq!((p.body.as_str(), p.line), ("edit", 6));
        assert!(st
            .patch(
                "d_nope",
                &DraftPatch {
                    body: None,
                    line: None,
                    start_line: None,
                    side: None
                }
            )
            .is_err());
        assert!(st.remove(&d.id));
        assert!(!st.remove(&d.id));
        assert!(st.drafts().is_empty());
    }

    #[test]
    fn findings_persist_per_head_and_dismiss() {
        let (_d, st) = store();
        let mk = |id: &str, head: &str| Finding {
            id: id.into(),
            head_sha: head.into(),
            path: "a.rs".into(),
            line: 1,
            start_line: None,
            side: "RIGHT".into(),
            severity: "high".into(),
            category: "bug".into(),
            title: "t".into(),
            body: "b".into(),
            suggestion: None,
            confidence: 0.9,
            created_at: now_iso(),
            dismissed: false,
            dismiss_reason: None,
        };
        st.save_findings("h1", &[mk("f_1", "h1"), mk("f_2", "h1")])
            .unwrap();
        st.save_findings("h2", &[mk("f_3", "h2")]).unwrap();
        assert_eq!(st.findings(Some("h1")).len(), 2);
        assert_eq!(st.findings(None).len(), 3);
        // Re-review of h1 replaces only h1.
        st.save_findings("h1", &[mk("f_4", "h1")]).unwrap();
        assert_eq!(st.findings(Some("h1")).len(), 1);
        assert_eq!(st.findings(None).len(), 2);
        assert!(st.dismiss_finding("f_4", Some("wontfix".into())));
        let f = st.finding("f_4").unwrap();
        assert!(f.dismissed && f.dismiss_reason.as_deref() == Some("wontfix"));
        assert!(!st.dismiss_finding("f_nope", None));
    }

    #[test]
    fn stale_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let g = |a: &[&str]| {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(r)
                .args(a)
                .status()
                .unwrap()
                .success());
        };
        g(&["init", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        g(&["config", "commit.gpgsign", "false"]);
        std::fs::write(r.join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-m", "one"]);
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let old = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Delete line 2, append line 6.
        std::fs::write(r.join("a.txt"), "1\n3\n4\n5\n6\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-m", "two"]);
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let new = String::from_utf8_lossy(&out.stdout).trim().to_string();

        let (_d, st) = store();
        let mk = |line: usize| {
            st.add(NewDraft {
                path: "a.txt".into(),
                line,
                start_line: None,
                side: None,
                body: "x".into(),
                thread_id: None,
                source: None,
                finding_id: None,
            })
            .unwrap()
        };
        mk(1); // survives
        mk(2); // deleted region → stale
        mk(5); // shifts to 4... new file: 1,3,4,5,6 → old 5 (value "5") is new 4
        let repo = ferro_core::git::GitRepo::new(r.to_path_buf());
        assert_eq!(st.remap(&repo, &old, &new).unwrap(), 1);
        let all = st.drafts();
        assert_eq!((all[0].line, all[0].stale), (1, false));
        assert!(all[1].stale);
        assert_eq!((all[2].line, all[2].stale), (4, false));
        // Persists across instances.
        let st2 = ReviewStore::new(st.dir.clone());
        assert_eq!(st2.drafts().len(), 3);
    }

    #[test]
    fn hunk_conventions() {
        // Verified against `git diff -U0` output (see /tmp scratch 2026-09-25):
        // insert-after-1 @@ -1,0 +2 @@; append @@ -5,0 +6 @@;
        // prepend @@ -0,0 +1 @@; del-line-2 @@ -2 +1,0 @@.
        let ins_after_1 = vec![Hunk {
            old_start: 1,
            old_lines: 0,
            new_start: 2,
            new_lines: 1,
        }];
        assert_eq!(map_line(&ins_after_1, 1, "RIGHT"), Some(1));
        assert_eq!(map_line(&ins_after_1, 2, "RIGHT"), Some(3));
        let append = vec![Hunk {
            old_start: 5,
            old_lines: 0,
            new_start: 6,
            new_lines: 1,
        }];
        assert_eq!(map_line(&append, 5, "RIGHT"), Some(5));
        let prepend = vec![Hunk {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: 1,
        }];
        assert_eq!(map_line(&prepend, 1, "RIGHT"), Some(2));
        let del = vec![Hunk {
            old_start: 2,
            old_lines: 1,
            new_start: 1,
            new_lines: 0,
        }];
        assert_eq!(map_line(&del, 1, "RIGHT"), Some(1));
        assert_eq!(map_line(&del, 2, "RIGHT"), None);
        assert_eq!(map_line(&del, 3, "RIGHT"), Some(2));
        assert_eq!(map_line(&del, 3, "LEFT"), None);
        assert_eq!(map_line(&[], 7, "LEFT"), Some(7));
    }

    #[test]
    fn viewed_and_rounds() {
        let (_d, st) = store();
        let s = st.set_viewed("a.rs", true, Some("abc".into()));
        assert!(s.viewed);
        let same = st.viewed_state("a.rs", Some("abc"));
        assert!(same.viewed && !same.changed_since);
        let moved = st.viewed_state("a.rs", Some("def"));
        assert!(!moved.viewed && moved.changed_since);
        assert!(st.last_reviewed_sha().is_none());
        st.record_round("abc", "submitted");
        st.record_round("def", "all-viewed");
        assert_eq!(st.rounds().len(), 2);
        assert_eq!(st.last_reviewed_sha().as_deref(), Some("abc"));
    }
}
