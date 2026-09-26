//! Filesystem watcher (B3): `notify` recommended watcher with a hand-rolled
//! debouncer — a batch flushes after 100 ms of quiet, and at the latest
//! 500 ms after its first change, so a steady writer (a log, a build) can't
//! starve it. Incremental snapshot updates must agree with full rebuilds, so
//! every path passes the rules `index::walk` applies: the shared skip list
//! (`.git`, `.ferro`, `target`, `node_modules`), gitignore semantics
//! (nested `.gitignore`/`.ignore`, `info/exclude`, global excludes, ignored
//! ancestor directories), regular files only (symlinks are never listed),
//! ≤ 8 MiB. Git control files (`index`, `HEAD`, `packed-refs`, `refs/**`)
//! raise [`WatchEvent::GitControl`]; the rest of `.git/**` is dropped on
//! arrival. A directory that appears (created or moved in) is walked, since
//! its files arrive as a single event; one that disappears is reported as a
//! delete, which the server applies to everything below it. An edited ignore
//! file can change any path's fate, so it triggers a full rebuild.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::Watcher as _;

use crate::index::{is_skipped, mtime_secs, walk_builder, MAX_LISTED_BYTES};

const DEBOUNCE: Duration = Duration::from_millis(100);
/// Upper bound on how long a busy batch may keep growing before it flushes.
const MAX_WAIT: Duration = Duration::from_millis(500);
const OVERFLOW_CAP: usize = 20_000;

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

fn in_git_dir(rel: &Path) -> bool {
    rel.components()
        .next()
        .is_some_and(|c| c.as_os_str() == ".git")
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
            // refs/** covers heads, remotes, tags and ferro's PR refs.
            s == "index" || s == "HEAD" || s == "packed-refs" || s == "refs"
        }
    }
}

fn is_ignore_file(rel: &Path) -> bool {
    matches!(
        rel.file_name().and_then(|n| n.to_str()),
        Some(".gitignore" | ".ignore")
    )
}

/// One directory's ignore files.
struct DirRules {
    /// `.ignore` (applies with or without git).
    ignore: Option<Gitignore>,
    /// `.gitignore` (inside a git repository only, like the walker).
    git: Option<Gitignore>,
}

/// Ignore decisions for single paths, matching what the `ignore` crate's
/// walker (and so `index::walk`) yields. Precedence follows the walker:
/// `.ignore` files deepest-first, then `.gitignore` files deepest-first,
/// then `info/exclude`, then the global excludes file; and a path below an
/// ignored directory is ignored whatever deeper rules say, because git never
/// looks inside such directories.
struct IgnoreRules {
    root: PathBuf,
    in_repo: bool,
    dirs: HashMap<PathBuf, DirRules>,
    exclude: Option<Gitignore>,
    global: Option<Gitignore>,
}

impl IgnoreRules {
    fn new(root: &Path) -> Self {
        let repo = root
            .ancestors()
            .find(|a| a.join(".git").exists())
            .map(Path::to_path_buf);
        let exclude = repo
            .as_deref()
            .and_then(|r| Self::load(&r.join(".git/info/exclude"), r));
        let global = repo
            .is_some()
            .then(|| GitignoreBuilder::new(root).build_global().0)
            .filter(|g| !g.is_empty());
        Self {
            root: root.to_path_buf(),
            in_repo: repo.is_some(),
            dirs: HashMap::new(),
            exclude,
            global,
        }
    }

    fn load(file: &Path, dir: &Path) -> Option<Gitignore> {
        if !file.is_file() {
            return None;
        }
        let mut b = GitignoreBuilder::new(dir);
        let _ = b.add(file);
        b.build().ok().filter(|g| !g.is_empty())
    }

    fn dir_rules(&mut self, rel_dir: &Path) -> &DirRules {
        let (root, in_repo) = (&self.root, self.in_repo);
        self.dirs.entry(rel_dir.to_path_buf()).or_insert_with(|| {
            let abs = root.join(rel_dir);
            DirRules {
                ignore: Self::load(&abs.join(".ignore"), &abs),
                git: if in_repo {
                    Self::load(&abs.join(".gitignore"), &abs)
                } else {
                    None
                },
            }
        })
    }

    /// The deciding rule for one path: `Some(true)` ignored, `Some(false)`
    /// whitelisted (`!pattern`), `None` when nothing matches.
    fn decide(&mut self, rel: &Path, is_dir: bool) -> Option<bool> {
        let full = self.root.join(rel);
        let dirs: Vec<PathBuf> = rel
            .parent()
            .map(|p| p.ancestors().map(Path::to_path_buf).collect())
            .unwrap_or_default();
        for git_pass in [false, true] {
            for d in &dirs {
                let rules = self.dir_rules(d);
                let gi = if git_pass { &rules.git } else { &rules.ignore };
                if let Some(gi) = gi {
                    let m = gi.matched(&full, is_dir);
                    if !m.is_none() {
                        return Some(m.is_ignore());
                    }
                }
            }
        }
        for gi in [&self.exclude, &self.global].into_iter().flatten() {
            let m = gi.matched(&full, is_dir);
            if !m.is_none() {
                return Some(m.is_ignore());
            }
        }
        None
    }

    fn is_ignored(&mut self, rel: &Path, is_dir: bool) -> bool {
        let comps: Vec<_> = rel.components().collect();
        let mut prefix = PathBuf::new();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            prefix.push(c.as_os_str());
            if self.decide(&prefix, true) == Some(true) {
                return true;
            }
        }
        self.decide(rel, is_dir) == Some(true)
    }
}

/// Pending changes between flushes.
struct Batcher {
    root: PathBuf,
    tx: mpsc::Sender<WatchEvent>,
    rules: IgnoreRules,
    /// Relative path → saw a structural event (create/remove/rename), which
    /// is what makes an existing directory worth walking.
    pending: HashMap<PathBuf, bool>,
    git_control: bool,
    overflowed: bool,
    rules_changed: bool,
    first: Option<Instant>,
    last: Instant,
}

impl Batcher {
    fn new(root: PathBuf, tx: mpsc::Sender<WatchEvent>) -> Self {
        Self {
            rules: IgnoreRules::new(&root),
            root,
            tx,
            pending: HashMap::new(),
            git_control: false,
            overflowed: false,
            rules_changed: false,
            first: None,
            last: Instant::now(),
        }
    }

    /// How long the next receive may block.
    fn wait(&self) -> Duration {
        match self.first {
            None => Duration::from_secs(3600),
            Some(first) => DEBOUNCE
                .saturating_sub(self.last.elapsed())
                .min(MAX_WAIT.saturating_sub(first.elapsed())),
        }
    }

    fn due(&self) -> bool {
        self.first
            .is_some_and(|first| self.last.elapsed() >= DEBOUNCE || first.elapsed() >= MAX_WAIT)
    }

    fn touch(&mut self) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = now;
    }

    fn note(&mut self, ev: notify::Event) {
        use notify::event::ModifyKind;
        use notify::EventKind::*;
        if !matches!(ev.kind, Create(_) | Modify(_) | Remove(_) | Any) {
            return;
        }
        let structural = matches!(
            ev.kind,
            Create(_) | Remove(_) | Modify(ModifyKind::Name(_)) | Any
        );
        for p in ev.paths {
            let Ok(rel) = p.strip_prefix(&self.root) else {
                continue;
            };
            if rel.as_os_str().is_empty() {
                continue;
            }
            // Filter noise on arrival (object writes, build output) so it
            // neither delays the flush nor counts towards overflow.
            if in_git_dir(rel) {
                if is_git_control(rel) {
                    self.git_control = true;
                    self.touch();
                }
                continue;
            }
            if is_skipped(rel) {
                continue;
            }
            if is_ignore_file(rel) {
                self.rules_changed = true;
            }
            *self.pending.entry(rel.to_path_buf()).or_insert(false) |= structural;
            self.touch();
            if self.pending.len() > OVERFLOW_CAP {
                self.overflowed = true;
            }
        }
    }

    fn send_overflow(&mut self) {
        let _ = self.tx.send(WatchEvent::Fs {
            upserts: vec![],
            deletes: vec![],
            overflow: true,
        });
    }

    fn flush(&mut self) {
        self.first = None;
        let git_control = std::mem::take(&mut self.git_control);
        if self.overflowed || self.rules_changed {
            if self.rules_changed {
                self.rules = IgnoreRules::new(&self.root);
            }
            self.pending.clear();
            self.overflowed = false;
            self.rules_changed = false;
            self.send_overflow();
        } else {
            let mut upserts = Vec::new();
            let mut deletes = Vec::new();
            for (rel, structural) in std::mem::take(&mut self.pending) {
                let full = self.root.join(&rel);
                let rel_s = rel.to_string_lossy().into_owned();
                match std::fs::symlink_metadata(&full) {
                    Ok(md) if md.is_file() => {
                        if md.len() > MAX_LISTED_BYTES || self.rules.is_ignored(&rel, false) {
                            // Newly ignored or grown too big: drop it (the
                            // server ignores deletes of unlisted paths).
                            deletes.push(rel_s);
                        } else {
                            upserts.push((rel_s, md.len(), mtime_secs(&md)));
                        }
                    }
                    Ok(md) if md.is_dir() => {
                        // A directory that appeared carries its files in one
                        // event; metadata-only directory events carry none.
                        if structural
                            && !self.rules.is_ignored(&rel, true)
                            && !self.walk_dir(&full, &mut upserts)
                        {
                            self.send_overflow();
                            upserts.clear();
                            deletes.clear();
                            break;
                        }
                    }
                    // Gone, or a symlink / special file (never listed): the
                    // server drops the path and anything below it.
                    _ => deletes.push(rel_s),
                }
            }
            upserts.sort();
            upserts.dedup_by(|a, b| a.0 == b.0);
            deletes.sort();
            deletes.dedup();
            if !upserts.is_empty() || !deletes.is_empty() {
                let _ = self.tx.send(WatchEvent::Fs {
                    upserts,
                    deletes,
                    overflow: false,
                });
            }
        }
        if git_control {
            let _ = self.tx.send(WatchEvent::GitControl);
        }
    }

    /// List a directory's files with the index walker. False when it holds
    /// more than the overflow cap (the caller falls back to a rebuild).
    fn walk_dir(&self, dir: &Path, upserts: &mut Vec<(String, u64, i64)>) -> bool {
        let mut n = 0usize;
        for entry in walk_builder(dir).build().flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(&self.root) else {
                continue;
            };
            if is_skipped(rel) {
                continue;
            }
            let Ok(md) = entry.metadata() else {
                continue;
            };
            if md.len() > MAX_LISTED_BYTES {
                continue;
            }
            upserts.push((
                rel.to_string_lossy().into_owned(),
                md.len(),
                mtime_secs(&md),
            ));
            n += 1;
            if n > OVERFLOW_CAP {
                return false;
            }
        }
        true
    }
}

/// Watch `root` recursively, batching into [`WatchEvent`] on `tx`.
/// The batcher thread exits when the handle drops.
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
    let thread = std::thread::spawn(move || {
        let mut b = Batcher::new(root, tx);
        loop {
            match raw_rx.recv_timeout(b.wait()) {
                Ok(Ok(ev)) => b.note(ev),
                Ok(Err(e)) => {
                    // Watcher-level failure (e.g. inotify exhaustion): tell
                    // the server to fall back to adaptive polling.
                    let _ = b.tx.send(WatchEvent::Error(e.to_string()));
                    return;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if b.due() {
                b.flush();
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

    fn rules_over(files: &[(&str, &str)]) -> (tempfile::TempDir, IgnoreRules) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        for (p, body) in files {
            let full = dir.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        let rules = IgnoreRules::new(&dir.path().canonicalize().unwrap());
        (dir, rules)
    }

    #[test]
    fn ignore_rules_follow_the_walker() {
        let (_d, mut r) = rules_over(&[
            (".gitignore", "dist/\n*.log\n!keep.log\n"),
            ("pkg/.gitignore", "gen/\n"),
            ("docs/.ignore", "drafts/\n"),
        ]);
        let p = |s: &str| PathBuf::from(s);
        // Directory pattern hides everything below it.
        assert!(r.is_ignored(&p("dist/app.js"), false));
        assert!(r.is_ignored(&p("dist/deep/x.js"), false));
        // File patterns, including whitelist.
        assert!(r.is_ignored(&p("x.log"), false));
        assert!(!r.is_ignored(&p("keep.log"), false));
        // Nested .gitignore and .ignore apply below their directory only.
        assert!(r.is_ignored(&p("pkg/gen/out.rs"), false));
        assert!(!r.is_ignored(&p("gen/out.rs"), false));
        assert!(r.is_ignored(&p("docs/drafts/a.md"), false));
        assert!(!r.is_ignored(&p("src/main.rs"), false));
    }
}
