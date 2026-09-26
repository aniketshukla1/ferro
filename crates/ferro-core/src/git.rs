//! Git v2 (B3): hardened runner, typed status/changes/log, mutations.
//! Every invocation goes through one runner: `--no-optional-locks`,
//! `-c core.quotepath=off -c color.ui=false`, `LC_ALL=C`,
//! `GIT_OPTIONAL_LOCKS=0`, literal pathspecs, no terminal prompts; exit
//! status is checked, stderr (4 KiB) is captured into [`GitError`], stdout
//! is capped, and every call has a timeout (30 s default, 120 s for
//! push/pull, 10 min for commit hooks). Revs from clients pass
//! [`check_rev`] so nothing option-shaped reaches git's argv. Legacy String
//! wrappers stay for the old routes and agent tools.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const NET_TIMEOUT: Duration = Duration::from_secs(120);
/// `git commit` runs hooks (pre-commit test suites): generous but bounded.
const HOOK_TIMEOUT: Duration = Duration::from_secs(600);
/// stderr kept for `git_failed` detail.
const STDERR_CAP: usize = 4096;
/// Largest stdout kept from one git call; past it git is killed and the
/// call fails with [`GitError::TooLarge`] (huge diffs, runaway output).
const MAX_OUTPUT: usize = 256 * 1024 * 1024;
/// Largest blob the cat-file reader returns.
const BLOB_CAP: u64 = 256 * 1024 * 1024;

#[derive(Debug, Error, Clone)]
pub enum GitError {
    #[error("not a git repository")]
    NotRepo,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("bad revision: {0}")]
    BadRev(String),
    #[error("git {args} failed: {stderr}")]
    Failed { args: String, stderr: String },
    #[error("git {args} timed out after {secs}s")]
    Timeout { args: String, secs: u64 },
    #[error("output too large")]
    TooLarge,
    #[error("cancelled")]
    Cancelled,
    #[error("git error: {0}")]
    Io(String),
}

impl GitError {
    pub fn stderr(&self) -> String {
        match self {
            GitError::Failed { stderr, .. } => stderr.clone(),
            GitError::Forbidden(e) => e.clone(),
            GitError::BadRev(r) => format!("bad revision: {r}"),
            GitError::NotRepo => "not a git repository".into(),
            GitError::Timeout { args, secs } => format!("{args} timed out after {secs}s"),
            GitError::TooLarge => "output too large".into(),
            GitError::Cancelled => "cancelled".into(),
            GitError::Io(e) => e.clone(),
        }
    }
}

/// Revs and refs arrive from query strings. Anything option-shaped
/// (`--output=<file>`) must never reach git's argv, where `rev-parse` would
/// echo it and `git diff` would act on it.
pub fn check_rev(rev: &str) -> Result<(), GitError> {
    if rev.is_empty()
        || rev.len() > 256
        || rev.starts_with('-')
        || rev.chars().any(char::is_control)
    {
        return Err(GitError::BadRev(rev.chars().take(64).collect()));
    }
    Ok(())
}

/// Object ids: SHA-1 (40) or SHA-256 (64) hex.
fn is_object_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A repo-relative pathspec for git's argv (§ 5.1, symlinks not followed).
fn git_path(root: &Path, p: &str, access: crate::paths::Access) -> Result<String, GitError> {
    crate::paths::git_rel(root, p, access).map_err(|e| match e {
        crate::paths::PathError::Empty => GitError::Failed {
            args: "paths".into(),
            stderr: "empty path".into(),
        },
        other => GitError::Forbidden(other.to_string()),
    })
}

/// A child pipe drained on its own thread into a shared buffer. Keeps at
/// most `cap` bytes: with `drain` the rest is read and dropped (stderr must
/// never block git); otherwise reading stops and [`Pipe::overflowed`] turns
/// true so the caller can kill the child.
struct Pipe {
    buf: Arc<Mutex<Vec<u8>>>,
    overflow: Arc<AtomicBool>,
}

impl Pipe {
    fn spawn<R: std::io::Read + Send + 'static>(
        h: Option<R>,
        cap: usize,
        drain: bool,
        done: mpsc::Sender<()>,
    ) -> Self {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let overflow = Arc::new(AtomicBool::new(false));
        let (b2, o2) = (buf.clone(), overflow.clone());
        std::thread::spawn(move || {
            if let Some(mut h) = h {
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    let n = match h.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };
                    let mut b = b2.lock().unwrap_or_else(|e| e.into_inner());
                    let room = cap.saturating_sub(b.len());
                    b.extend_from_slice(&chunk[..n.min(room)]);
                    if n > room && !drain {
                        o2.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
            let _ = done.send(());
        });
        Self { buf, overflow }
    }

    fn overflowed(&self) -> bool {
        self.overflow.load(Ordering::Relaxed)
    }

    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.buf.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

fn kill(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Debug, Clone)]
pub struct GitRepo {
    pub root: PathBuf,
    batch: std::sync::Arc<std::sync::Mutex<CatFileBatch>>,
    /// Extra env for every child (e.g. forge HTTPS auth, never argv).
    pub extra_env: Vec<(String, String)>,
}

impl GitRepo {
    pub fn new(root: PathBuf) -> Self {
        let batch = CatFileBatch::new(root.clone());
        Self {
            root,
            batch: std::sync::Arc::new(std::sync::Mutex::new(batch)),
            extra_env: Vec::new(),
        }
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.extra_env = env;
        self
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.arg("--no-optional-locks")
            .arg("-c")
            .arg("core.quotepath=off")
            .arg("-c")
            .arg("color.ui=false")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            // Every pathspec ferro passes is a literal path: no globs and no
            // `:(top)` / `:/` magic (a stray `*.rs` must not discard all).
            .env("GIT_LITERAL_PATHSPECS", "1")
            // Credentials are never prompted for on ferro's terminal: a
            // push/pull without cached creds fails fast instead of hanging.
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        for (k, v) in &self.extra_env {
            c.env(k, v);
        }
        c
    }

    /// Run to completion with the default timeout.
    pub fn run(&self, args: &[&str]) -> Result<String, GitError> {
        self.run_cancel(args, DEFAULT_TIMEOUT, None)
    }

    /// Run with the network timeout (push/pull/fetch).
    pub fn run_net(&self, args: &[&str]) -> Result<String, GitError> {
        self.run_cancel(args, NET_TIMEOUT, None)
    }

    /// Run, killing the child when `stop` is set (client disconnect).
    pub fn run_cancel(
        &self,
        args: &[&str],
        timeout: Duration,
        stop: Option<&AtomicBool>,
    ) -> Result<String, GitError> {
        self.exec(args, None, timeout, stop, &[])
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// Raw bytes (for `-z` outputs).
    pub fn run_bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        self.exec(args, None, DEFAULT_TIMEOUT, None, &[])
    }

    /// Raw bytes for `git diff`: exit code 1 means "differences found"
    /// (`--no-index` implies `--exit-code`) and still carries the output.
    pub fn run_diff_bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        self.exec(args, None, DEFAULT_TIMEOUT, None, &[1])
    }

    /// Feed stdin (commit message via `-F -`, never argv). Commit hooks run
    /// here, hence the long timeout.
    pub fn run_stdin(&self, args: &[&str], input: &[u8]) -> Result<String, GitError> {
        self.exec(args, Some(input), HOOK_TIMEOUT, None, &[])
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// The one place git is spawned. stdout/stderr drain on their own
    /// threads; the call wakes when stdout closes (git exited) instead of
    /// polling on a fixed tick, and the child is killed on cancel, timeout
    /// or stdout past [`MAX_OUTPUT`]. `ok_codes` are extra successful exits.
    fn exec(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        timeout: Duration,
        stop: Option<&AtomicBool>,
        ok_codes: &[i32],
    ) -> Result<Vec<u8>, GitError> {
        let deadline = Instant::now() + timeout;
        let mut cmd = self.command(args);
        if input.is_some() {
            cmd.stdin(Stdio::piped());
        }
        let mut child = cmd.spawn().map_err(|e| GitError::Io(e.to_string()))?;
        if let (Some(data), Some(mut h)) = (input, child.stdin.take()) {
            // Own thread: a child that writes before it reads can't deadlock us.
            let data = data.to_vec();
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = h.write_all(&data);
            });
        }
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let out = Pipe::spawn(child.stdout.take(), MAX_OUTPUT, false, done_tx.clone());
        let err = Pipe::spawn(child.stderr.take(), STDERR_CAP, true, done_tx);
        let mut open_pipes = 2usize;
        let mut exited: Option<(std::process::ExitStatus, Instant)> = None;
        let status = loop {
            if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                kill(&mut child);
                return Err(GitError::Cancelled);
            }
            if out.overflowed() {
                kill(&mut child);
                return Err(GitError::TooLarge);
            }
            if exited.is_none() {
                if let Some(st) = child.try_wait().map_err(|e| GitError::Io(e.to_string()))? {
                    exited = Some((st, Instant::now()));
                }
            }
            if let Some((st, at)) = exited {
                // Pipes close with git; a hook's background process that
                // inherited them gets a short grace, not the whole timeout.
                if open_pipes == 0 || at.elapsed() > Duration::from_millis(250) {
                    break st;
                }
            }
            let now = Instant::now();
            if now >= deadline {
                kill(&mut child);
                return Err(GitError::Timeout {
                    args: args.join(" "),
                    secs: timeout.as_secs(),
                });
            }
            let tick = if open_pipes == 0 {
                Duration::from_millis(1)
            } else {
                Duration::from_millis(10)
            };
            if done_rx.recv_timeout(tick.min(deadline - now)).is_ok() {
                open_pipes = open_pipes.saturating_sub(1);
            }
        };
        if status.success() || status.code().is_some_and(|c| ok_codes.contains(&c)) {
            return Ok(out.take());
        }
        Err(Self::fail(args, &err.take()))
    }

    fn fail(args: &[&str], stderr: &[u8]) -> GitError {
        let stderr = String::from_utf8_lossy(stderr).into_owned();
        if stderr.contains("not a git repository") {
            return GitError::NotRepo;
        }
        let mut err = stderr;
        if err.len() > STDERR_CAP {
            let mut cut = STDERR_CAP;
            while !err.is_char_boundary(cut) {
                cut -= 1;
            }
            err.truncate(cut);
        }
        GitError::Failed {
            args: args.join(" "),
            stderr: err,
        }
    }

    // -- status v2 ------------------------------------------------------

    pub fn status_v2(&self) -> Result<GitStatus, GitError> {
        let raw = self.run_bytes(&[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
        ])?;
        Ok(parse_status_v2(&raw))
    }

    pub fn head_sha(&self) -> Option<String> {
        self.run(&["rev-parse", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn branch(&self) -> Option<String> {
        self.run(&["rev-parse", "--abbrev-ref", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s != "HEAD" && !s.is_empty())
    }

    // -- changes / log ---------------------------------------------------

    /// Resolve a `base` form (`HEAD`, `merge-base`, `merge-base:<ref>`, any
    /// rev) to an object id. On an unborn branch `HEAD` is the empty tree.
    pub fn resolve_base(&self, base: &str) -> Result<String, GitError> {
        if base == "HEAD" {
            return self.head_or_empty_tree();
        }
        if base == "merge-base" {
            // Workspace default: the base is HEAD itself. PR mode (B4)
            // resolves the merge-base against the base ref at the route layer.
            return self.head_or_empty_tree();
        }
        if let Some(r) = base.strip_prefix("merge-base:") {
            check_rev(r)?;
            return self
                .run(&["merge-base", "HEAD", r])
                .map(|s| s.trim().to_string());
        }
        self.rev_parse(base)
    }

    /// `rev-parse --verify` of a client-supplied rev: exactly one object,
    /// never an echoed flag.
    pub fn rev_parse(&self, rev: &str) -> Result<String, GitError> {
        check_rev(rev)?;
        self.run(&["rev-parse", "--verify", "--quiet", rev])
            .map(|s| s.trim().to_string())
            .map_err(|e| match e {
                GitError::Failed { .. } => GitError::BadRev(rev.chars().take(64).collect()),
                other => other,
            })
    }

    /// True on a fresh `git init` (HEAD names a branch with no commits yet).
    pub fn is_unborn(&self) -> bool {
        self.run(&["rev-parse", "--verify", "--quiet", "HEAD"])
            .is_err()
            && self.run(&["symbolic-ref", "-q", "HEAD"]).is_ok()
    }

    /// `HEAD`, or the empty tree on an unborn branch so a fresh repository
    /// shows every file as added instead of failing.
    fn head_or_empty_tree(&self) -> Result<String, GitError> {
        match self.run(&["rev-parse", "--verify", "--quiet", "HEAD"]) {
            Ok(s) => Ok(s.trim().to_string()),
            Err(GitError::NotRepo) => Err(GitError::NotRepo),
            Err(e) => {
                if self.run(&["symbolic-ref", "-q", "HEAD"]).is_err() {
                    return Err(e);
                }
                // The empty tree in this repository's object format.
                self.run_stdin(&["hash-object", "-t", "tree", "--stdin"], b"")
                    .map(|s| s.trim().to_string())
            }
        }
    }

    /// `--numstat -z -M` file stats between base and target (`worktree` /
    /// `index` / any rev). Untracked files count as additions.
    pub fn changes(&self, base: &str, target: &str) -> Result<ChangeSet, GitError> {
        let base_sha = self.resolve_base(base)?;
        let (target_sha, mut files) = match target {
            "worktree" => {
                // base..index (staged) + index..worktree (unstaged), merged by path.
                let staged = self.numstat(&[&base_sha, "--cached"])?;
                let unstaged = self.numstat(&[])?;
                let staged_letters = self.name_status(&[&base_sha, "--cached"])?;
                let unstaged_letters = self.name_status(&[])?;
                let mut files = merge_numstats(staged, unstaged, staged_letters, unstaged_letters);
                let at: std::collections::HashMap<String, usize> = files
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.path.clone(), i))
                    .collect();
                for (path, lines, binary) in self.untracked_with_lines()? {
                    match at.get(&path) {
                        // Still in the diff (e.g. `git rm --cached`): count the
                        // on-disk copy as re-added lines.
                        Some(&i) => files[i].additions += lines,
                        None => files.push(ChangedFile {
                            path,
                            old_path: None,
                            status: ChangeStatusStr(ChangeStatus::Untracked),
                            additions: lines,
                            deletions: 0,
                            binary,
                        }),
                    }
                }
                (None, files)
            }
            "index" => {
                let counts = self.numstat(&[&base_sha, "--cached"])?;
                let letters = self.name_status(&[&base_sha, "--cached"])?;
                (None, join_counts(counts, &letters))
            }
            rev => {
                let sha = self.rev_parse(rev)?;
                let range = format!("{base_sha}..{sha}");
                let counts = self.numstat(&[&range])?;
                let letters = self.name_status(&[&range])?;
                (Some(sha), join_counts(counts, &letters))
            }
        };
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let additions = files.iter().map(|f| f.additions).sum();
        let deletions = files.iter().map(|f| f.deletions).sum();
        Ok(ChangeSet {
            base: base.to_string(),
            base_sha,
            target: target.to_string(),
            target_sha,
            stats: ChangeStats {
                files: files.len(),
                additions,
                deletions,
            },
            files,
        })
    }

    /// Parse `git diff --numstat -z -M` (plus explicit extra args) into
    /// (path, old_path, additions, deletions, binary).
    fn numstat(&self, extra: &[&str]) -> Result<Vec<NumstatEntry>, GitError> {
        let mut args = vec![
            "diff",
            "--numstat",
            "-z",
            "-M",
            "--no-color",
            "--no-ext-diff",
        ];
        args.extend(extra);
        let raw = self.run_bytes(&args)?;
        Ok(parse_numstat(&raw))
    }

    /// Status letters + rename sources from `git diff --name-status -z -M`.
    pub(crate) fn name_status(
        &self,
        extra: &[&str],
    ) -> Result<std::collections::BTreeMap<String, (ChangeStatus, Option<String>)>, GitError> {
        let mut args = vec![
            "diff",
            "--name-status",
            "-z",
            "-M",
            "--no-color",
            "--no-ext-diff",
        ];
        args.extend(extra);
        let raw = self.run_bytes(&args)?;
        let mut map = std::collections::BTreeMap::new();
        // -z structure (verified): `<code>[score]\0<path>\0[<new>\0]`,
        // orig-then-new for renames/copies. No tabs involved.
        let chunks: Vec<&[u8]> = raw.split(|&b| b == 0).collect();
        let mut i = 0;
        while i < chunks.len() {
            let code = chunks[i];
            i += 1;
            if code.is_empty() {
                continue;
            }
            let letter = code[0] as char;
            if i >= chunks.len() {
                break;
            }
            let first = String::from_utf8_lossy(chunks[i]).into_owned();
            i += 1;
            match letter {
                'R' | 'C' => {
                    let st = if letter == 'R' {
                        ChangeStatus::Renamed
                    } else {
                        ChangeStatus::Copied
                    };
                    if i >= chunks.len() {
                        map.insert(first.clone(), (st, None));
                        break;
                    }
                    let new = String::from_utf8_lossy(chunks[i]).into_owned();
                    i += 1;
                    map.insert(new, (st, Some(first)));
                }
                'A' => {
                    map.insert(first, (ChangeStatus::Added, None));
                }
                'D' => {
                    map.insert(first, (ChangeStatus::Deleted, None));
                }
                'T' => {
                    map.insert(first, (ChangeStatus::Typechange, None));
                }
                'M' | 'U' => {
                    map.insert(first, (ChangeStatus::Modified, None));
                }
                _ => {}
            }
        }
        Ok(map)
    }

    /// Untracked files with their line counts (what `--numstat` would call
    /// additions) and a binary flag. Counting streams the file, so size never
    /// costs memory, and only regular files are opened: an untracked symlink
    /// (to `/dev/zero`, a FIFO, or outside the workspace) is never followed.
    fn untracked_with_lines(&self) -> Result<Vec<(String, u64, bool)>, GitError> {
        let raw = self.run_bytes(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        let mut out = Vec::new();
        for chunk in raw.split(|&b| b == 0) {
            if chunk.len() < 4 || &chunk[..3] != b"?? " {
                continue;
            }
            let path = String::from_utf8_lossy(&chunk[3..]).into_owned();
            let (lines, binary) = count_lines(&self.root.join(&path));
            out.push((path, lines, binary));
        }
        Ok(out)
    }

    pub fn log(&self, limit: usize, path: Option<&str>) -> Result<Vec<Commit>, GitError> {
        // Fields split on \x1f, records on NUL (-z). %s is one line already.
        let mut args = vec![
            "log".to_string(),
            format!("--max-count={}", limit.clamp(1, 500)),
            "--pretty=format:%H%x1f%h%x1f%an%x1f%aI%x1f%s".to_string(),
            "-z".to_string(),
        ];
        if let Some(p) = path {
            args.push("--".to_string());
            args.push(p.to_string());
        }
        let raw = self.run_bytes(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
        let mut out = Vec::new();
        for chunk in raw.split(|&b| b == 0) {
            if chunk.is_empty() {
                continue;
            }
            let text = String::from_utf8_lossy(chunk);
            let mut parts = text.split('\x1f');
            let (Some(sha), Some(short), Some(author), Some(date), Some(subject)) = (
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
                parts.next(),
            ) else {
                continue;
            };
            if !is_object_id(sha) {
                continue;
            }
            out.push(Commit {
                sha: sha.into(),
                short: short.into(),
                author: author.into(),
                date: date.into(),
                subject: subject.into(),
            });
        }
        Ok(out)
    }

    // -- mutations (every path through § 5.1) -----------------------------

    fn write_paths(&self, paths: &[String]) -> Result<Vec<String>, GitError> {
        if paths.is_empty() {
            return Err(GitError::Failed {
                args: "paths".into(),
                stderr: "no paths".into(),
            });
        }
        // Lexical + parent-containment only: a symlink is staged, restored or
        // deleted as the link itself, never as its target.
        paths
            .iter()
            .map(|p| git_path(&self.root, p, crate::paths::Access::Write))
            .collect()
    }

    pub fn stage_paths(&self, paths: &[String]) -> Result<String, GitError> {
        let safe = self.write_paths(paths)?;
        let mut args = vec!["add", "--"];
        args.extend(safe.iter().map(|s| s.as_str()));
        self.run(&args)
    }

    pub fn unstage_paths(&self, paths: &[String]) -> Result<String, GitError> {
        let safe = self.write_paths(paths)?;
        // Before the first commit there is no HEAD to restore the index
        // from; unstaging means dropping the entries from the index.
        let mut args = if self.is_unborn() {
            vec!["rm", "--cached", "-r", "-q", "--"]
        } else {
            vec!["restore", "--staged", "--"]
        };
        args.extend(safe.iter().map(|s| s.as_str()));
        self.run(&args)
    }

    /// Destructive: tracked → restore worktree; untracked → delete. A
    /// directory path covers the changed files below it (its untracked files
    /// are deleted one by one, as listed by git).
    pub fn discard_paths(&self, paths: &[String]) -> Result<(), GitError> {
        let safe = self.write_paths(paths)?;
        // Partition via status to avoid deleting tracked content by mistake.
        let st = self.status_v2().unwrap_or_default();
        let mut tracked: Vec<&str> = Vec::new();
        let mut untracked: Vec<&str> = Vec::new();
        let mut dirs: Vec<&str> = Vec::new();
        for p in &safe {
            if let Some(f) = st.files.iter().find(|f| &f.path == p) {
                if f.untracked {
                    untracked.push(&f.path);
                } else {
                    tracked.push(p);
                }
                continue;
            }
            let prefix = format!("{p}/");
            let below: Vec<&StatusFile> = st
                .files
                .iter()
                .filter(|f| f.path.starts_with(&prefix))
                .collect();
            if below.iter().any(|f| f.untracked) {
                untracked.extend(
                    below
                        .iter()
                        .filter(|f| f.untracked)
                        .map(|f| f.path.as_str()),
                );
                dirs.push(p);
            }
            // Unknown paths go to git too, which reports them.
            if below.is_empty() || below.iter().any(|f| !f.untracked) {
                tracked.push(p);
            }
        }
        // Git lists untracked files one by one (`-uall`); a directory entry
        // is a nested repository, which discard never deletes.
        if let Some(d) = untracked
            .iter()
            .find(|u| std::fs::symlink_metadata(self.root.join(u)).is_ok_and(|m| m.is_dir()))
        {
            return Err(GitError::Forbidden(format!(
                "{} is a nested repository; delete it by hand",
                d.trim_end_matches('/')
            )));
        }
        if !tracked.is_empty() {
            let mut args = vec!["restore", "--source=HEAD", "--worktree", "--"];
            args.extend(tracked.iter().copied());
            self.run(&args)?;
        }
        for u in untracked {
            // `symlink_metadata` above: a link is removed, its target never
            // touched.
            match std::fs::remove_file(self.root.join(u)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(GitError::Io(e.to_string()));
                }
                _ => {}
            }
        }
        // Directories the client asked to discard: drop the ones left empty.
        for d in dirs {
            prune_empty_dirs(&self.root.join(d));
        }
        Ok(())
    }

    pub fn commit_msg(&self, message: &str, amend: bool) -> Result<Commit, GitError> {
        if message.trim().is_empty() {
            return Err(GitError::Failed {
                args: "commit".into(),
                stderr: "empty message".into(),
            });
        }
        let staged = self.run(&["diff", "--cached", "--quiet"]).is_err();
        if !staged && !amend {
            return Err(GitError::Failed {
                args: "commit".into(),
                stderr: "nothing staged".into(),
            });
        }
        let mut args = vec!["commit", "-F", "-"];
        if amend {
            args.push("--amend");
        }
        self.run_stdin(&args, message.as_bytes())?;
        let sha = self
            .run(&["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())?;
        let summary = message.lines().next().unwrap_or("").to_string();
        Ok(Commit {
            sha: sha.clone(),
            short: sha[..7.min(sha.len())].into(),
            author: String::new(),
            date: String::new(),
            subject: summary,
        })
    }

    pub fn push(&self) -> Result<String, GitError> {
        self.run_net(&["push"])
    }

    /// Fast-forward only; diverged → error (route maps to 409).
    pub fn pull_ff(&self) -> Result<(String, bool), GitError> {
        let before = self.head_sha();
        let out = self.run_net(&["pull", "--ff-only"])?;
        let after = self.head_sha();
        Ok((out, before != after))
    }

    // -- cat-file batch ------------------------------------------------------

    /// Blob bytes at `rev:path` through the long-lived batch process.
    pub fn blob_bytes(&self, rev: &str, path: &str) -> Result<Vec<u8>, GitError> {
        self.blob_bytes_max(rev, path, BLOB_CAP)
    }

    /// Blob bytes at `rev:path`, or [`GitError::TooLarge`] past `max` bytes
    /// (checked from the object header, before the body is read). An empty
    /// `rev` reads the index (stage 0), like `git show :path`.
    pub fn blob_bytes_max(&self, rev: &str, path: &str, max: u64) -> Result<Vec<u8>, GitError> {
        let no_blob = || GitError::Failed {
            args: "blob".into(),
            stderr: "no such blob".into(),
        };
        if !rev.is_empty() {
            check_rev(rev)?;
        }
        if path.is_empty() || path.len() > 512 || path.chars().any(char::is_control) {
            return Err(GitError::Failed {
                args: "blob".into(),
                stderr: "bad path".into(),
            });
        }
        let sha = self
            .run_bytes(&["rev-parse", "--verify", "--quiet", &format!("{rev}:{path}")])
            .map(|o| String::from_utf8_lossy(&o).trim().to_string())
            .map_err(|e| match e {
                GitError::NotRepo => GitError::NotRepo,
                _ => no_blob(),
            })?;
        if !is_object_id(&sha) {
            return Err(no_blob());
        }
        let mut batch = self
            .batch
            .lock()
            .map_err(|_| GitError::Io("batch lock".into()))?;
        match batch.read(&sha, max.min(BLOB_CAP)) {
            Blob::Bytes(b) => Ok(b),
            Blob::TooLarge => Err(GitError::TooLarge),
            Blob::Missing => Err(no_blob()),
        }
    }
}

enum Blob {
    Bytes(Vec<u8>),
    TooLarge,
    Missing,
}

/// Long-lived `git cat-file --batch` reader (one per workspace, shared by
/// clones). Respawns transparently after any failure; killed and reaped on
/// drop (workspace switch), so no process outlives its workspace.
#[derive(Debug)]
struct CatFileBatch {
    root: PathBuf,
    child: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<std::io::BufReader<std::process::ChildStdout>>,
}

impl Drop for CatFileBatch {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl CatFileBatch {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            child: None,
            stdin: None,
            stdout: None,
        }
    }

    fn ensure(&mut self) -> bool {
        if self
            .child
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_none())
        {
            return true;
        }
        self.shutdown();
        let mut cmd = Command::new("git");
        cmd.arg("--no-optional-locks")
            .arg("-c")
            .arg("core.quotepath=off")
            .arg("-c")
            .arg("color.ui=false")
            .arg("-C")
            .arg(&self.root)
            .arg("cat-file")
            .arg("--batch")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(std::io::BufReader::new);
        if stdin.is_none() || stdout.is_none() {
            return false;
        }
        self.child = Some(child);
        self.stdin = stdin;
        self.stdout = stdout;
        true
    }

    fn shutdown(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.stdin = None;
        self.stdout = None;
    }

    /// Read one blob of at most `max` bytes. Any protocol surprise (missing
    /// object, non-blob, oversized body) shuts the process down so the next
    /// read starts in sync on a fresh one.
    fn read(&mut self, sha: &str, max: u64) -> Blob {
        use std::io::{BufRead, Read, Write};
        if !self.ensure() {
            return Blob::Missing;
        }
        let (stdin, stdout) = match (self.stdin.as_mut(), self.stdout.as_mut()) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                self.shutdown();
                return Blob::Missing;
            }
        };
        if writeln!(stdin, "{sha}").is_err() || stdin.flush().is_err() {
            self.shutdown();
            return Blob::Missing;
        }
        let mut header = String::new();
        if stdout.read_line(&mut header).is_err() {
            self.shutdown();
            return Blob::Missing;
        }
        // `<sha> <type> <size>`; missing objects reply `<sha> missing`.
        let mut parts = header.split_whitespace();
        let (Some(_), Some(ty), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            self.shutdown();
            return Blob::Missing;
        };
        let Ok(size) = size.parse::<u64>() else {
            self.shutdown();
            return Blob::Missing;
        };
        if ty != "blob" {
            // Non-blob bodies are not drained: resync by restart.
            self.shutdown();
            return Blob::Missing;
        }
        if size > max {
            // Skipping the body would mean reading it; a respawn is cheaper.
            self.shutdown();
            return Blob::TooLarge;
        }
        let mut buf = vec![0u8; size as usize];
        let mut nl = [0u8; 1];
        if stdout.read_exact(&mut buf).is_err() || stdout.read_exact(&mut nl).is_err() {
            self.shutdown();
            return Blob::Missing;
        }
        Blob::Bytes(buf)
    }
}

// -- status v2 types -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GitCode {
    M,
    A,
    D,
    R,
    C,
    T,
    U,
    Untracked,
}

impl GitCode {
    pub fn as_str(self) -> &'static str {
        match self {
            GitCode::M => "M",
            GitCode::A => "A",
            GitCode::D => "D",
            GitCode::R => "R",
            GitCode::C => "C",
            GitCode::T => "T",
            GitCode::U => "U",
            GitCode::Untracked => "?",
        }
    }

    fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'M' => Some(GitCode::M),
            b'A' => Some(GitCode::A),
            b'D' => Some(GitCode::D),
            b'R' => Some(GitCode::R),
            b'C' => Some(GitCode::C),
            b'T' => Some(GitCode::T),
            b'U' => Some(GitCode::U),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusFile {
    pub path: String,
    #[serde(rename = "origPath", skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
    pub index: Option<&'static str>,
    pub worktree: Option<&'static str>,
    pub untracked: bool,
    pub conflicted: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StatusCounts {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub detached: bool,
    #[serde(rename = "headSha")]
    pub head_sha: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<StatusFile>,
    pub counts: StatusCounts,
}

/// Hash of the status payload; the watcher emits `git` only when it changes.
pub fn status_hash(st: &GitStatus) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    st.branch.hash(&mut h);
    st.head_sha.hash(&mut h);
    st.upstream.hash(&mut h);
    st.ahead.hash(&mut h);
    st.behind.hash(&mut h);
    for f in &st.files {
        f.path.hash(&mut h);
        f.orig_path.hash(&mut h);
        f.index.hash(&mut h);
        f.worktree.hash(&mut h);
    }
    h.finish()
}

fn parse_status_v2(raw: &[u8]) -> GitStatus {
    let mut st = GitStatus::default();
    let chunks: Vec<&[u8]> = raw.split(|&b| b == 0).collect();
    let mut i = 0;
    while i < chunks.len() {
        let c = chunks[i];
        i += 1;
        if c.is_empty() {
            continue;
        }
        if c[0] == b'#' {
            let line = String::from_utf8_lossy(c);
            if let Some(v) = line.strip_prefix("# branch.oid ") {
                st.head_sha = (v != "(initial)").then(|| v.to_string());
            } else if let Some(v) = line.strip_prefix("# branch.head ") {
                if v == "(detached)" {
                    st.detached = true;
                    st.branch = None;
                } else {
                    st.branch = Some(v.to_string());
                }
            } else if let Some(v) = line.strip_prefix("# branch.upstream ") {
                st.upstream = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("# branch.ab ") {
                for part in v.split_whitespace() {
                    if let Some(a) = part.strip_prefix('+') {
                        st.ahead = a.parse().unwrap_or(0);
                    } else if let Some(b) = part.strip_prefix('-') {
                        st.behind = b.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        match c[0] {
            b'1' => {
                // 1 XY subm mH mI mW hH hI path
                if c.len() < 6 {
                    continue;
                }
                let (x, y) = (c[2], c[3]);
                let path = path_after_fields(c, 8);
                push_change(&mut st, path, None, x, y, false);
            }
            b'2' => {
                // 2 XY subm mH mI mW hH hI Xscore newpath \0 origpath
                if c.len() < 6 {
                    continue;
                }
                let (x, y) = (c[2], c[3]);
                let new_path = path_after_fields(c, 9);
                let orig = chunks
                    .get(i)
                    .map(|b| String::from_utf8_lossy(b).into_owned());
                i += 1;
                push_change(&mut st, new_path, orig, x, y, false);
            }
            b'u' => {
                // u XY subm m1 m2 m3 mW h1 h2 h3 path
                if c.len() < 6 {
                    continue;
                }
                let (x, y) = (c[2], c[3]);
                let path = path_after_fields(c, 10);
                push_change(&mut st, path, None, x, y, true);
            }
            b'?' => {
                let path = String::from_utf8_lossy(c.get(2..).unwrap_or_default()).into_owned();
                st.files.push(StatusFile {
                    path,
                    orig_path: None,
                    index: None,
                    worktree: None,
                    untracked: true,
                    conflicted: false,
                });
                st.counts.untracked += 1;
            }
            _ => {}
        }
    }
    st.files.sort_by(|a, b| a.path.cmp(&b.path));
    st
}

/// Path is the last space-separated field of the header line.
fn path_after_fields(line: &[u8], n_fields_before_path: usize) -> String {
    // Fields are space-separated; the path is everything after the nth space.
    // (Paths are verbatim under -z, and may contain spaces — hence count.)
    let mut spaces = 0;
    for (idx, &b) in line.iter().enumerate() {
        if b == b' ' {
            spaces += 1;
            if spaces == n_fields_before_path {
                return String::from_utf8_lossy(&line[idx + 1..]).into_owned();
            }
        }
    }
    String::from_utf8_lossy(line).into_owned()
}

fn push_change(
    st: &mut GitStatus,
    path: String,
    orig: Option<String>,
    x: u8,
    y: u8,
    unmerged: bool,
) {
    let index = GitCode::from_byte(x).map(|c| c.as_str());
    let worktree = GitCode::from_byte(y).map(|c| c.as_str());
    let conflicted = unmerged || x == b'U' || y == b'U';
    if !conflicted {
        if index.is_some() {
            st.counts.staged += 1;
        }
        if worktree.is_some() {
            st.counts.unstaged += 1;
        }
    } else {
        st.counts.conflicted += 1;
    }
    st.files.push(StatusFile {
        path,
        orig_path: orig,
        index,
        worktree,
        untracked: false,
        conflicted,
    });
}

// -- changes types ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Typechange,
    Untracked,
}

impl ChangeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeStatus::Added => "A",
            ChangeStatus::Modified => "M",
            ChangeStatus::Deleted => "D",
            ChangeStatus::Renamed => "R",
            ChangeStatus::Copied => "C",
            ChangeStatus::Typechange => "T",
            ChangeStatus::Untracked => "?",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    pub path: String,
    #[serde(rename = "oldPath", skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: ChangeStatusStr,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

/// Serialize `status` as the single-letter code API.md expects.
#[derive(Debug, Clone, Copy)]
pub struct ChangeStatusStr(pub ChangeStatus);

impl Serialize for ChangeStatusStr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0.as_str())
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChangeStats {
    pub files: usize,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangeSet {
    pub base: String,
    #[serde(rename = "baseSha")]
    pub base_sha: String,
    pub target: String,
    #[serde(rename = "targetSha")]
    pub target_sha: Option<String>,
    pub stats: ChangeStats,
    pub files: Vec<ChangedFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

fn parse_numstat(raw: &[u8]) -> Vec<NumstatEntry> {
    // -z: records are `<add>\t<del>\t<orig>\0<new>\0` for renames
    // (plain mode prints `orig => new`), else `<add>\t<del>\t<path>\0`.
    // A chunk is a record only if the first two tab-fields are counts;
    // anything else is the NEW path continuing the previous rename record.
    let mut out: Vec<NumstatEntry> = Vec::new();
    for c in raw.split(|&b| b == 0) {
        if c.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(c);
        let mut parts = text.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            // Continuation chunk: NEW path of the previous rename record;
            // the inline path was the orig.
            if let Some(prev) = out.last_mut() {
                prev.old_path = Some(std::mem::take(&mut prev.path));
                prev.path = text.into_owned();
            }
            continue;
        };
        let counts = add == "-"
            || del == "-"
            || (add.bytes().all(|b| b.is_ascii_digit()) && del.bytes().all(|b| b.is_ascii_digit()));
        if !counts {
            if let Some(prev) = out.last_mut() {
                prev.old_path = Some(std::mem::take(&mut prev.path));
                prev.path = text.into_owned();
            }
            continue;
        }
        let binary = add == "-" || del == "-";
        let (add_n, del_n) = if binary {
            (0, 0)
        } else {
            (add.parse().unwrap_or(0), del.parse().unwrap_or(0))
        };
        out.push(NumstatEntry {
            path: path.to_string(),
            old_path: None,
            additions: add_n,
            deletions: del_n,
            binary,
        });
    }
    out
}

#[derive(Debug, Clone)]
struct NumstatEntry {
    path: String,
    old_path: Option<String>,
    additions: u64,
    deletions: u64,
    binary: bool,
}

fn join_counts(
    counts: Vec<NumstatEntry>,
    letters: &std::collections::BTreeMap<String, (ChangeStatus, Option<String>)>,
) -> Vec<ChangedFile> {
    counts
        .into_iter()
        .map(|e| {
            let (status, old) = letters
                .get(&e.path)
                .cloned()
                .unwrap_or((ChangeStatus::Modified, None));
            ChangedFile {
                path: e.path,
                old_path: old.or(e.old_path),
                status: ChangeStatusStr(status),
                additions: e.additions,
                deletions: e.deletions,
                binary: e.binary,
            }
        })
        .collect()
}

fn merge_numstats(
    staged: Vec<NumstatEntry>,
    unstaged: Vec<NumstatEntry>,
    staged_letters: std::collections::BTreeMap<String, (ChangeStatus, Option<String>)>,
    unstaged_letters: std::collections::BTreeMap<String, (ChangeStatus, Option<String>)>,
) -> Vec<ChangedFile> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, ChangedFile> = BTreeMap::new();
    for e in staged {
        let (status, old) = staged_letters
            .get(&e.path)
            .cloned()
            .unwrap_or((ChangeStatus::Modified, None));
        map.insert(
            e.path.clone(),
            ChangedFile {
                path: e.path,
                old_path: old.or(e.old_path),
                status: ChangeStatusStr(status),
                additions: e.additions,
                deletions: e.deletions,
                binary: e.binary,
            },
        );
    }
    for e in unstaged {
        match map.get_mut(&e.path) {
            Some(f) => {
                f.additions += e.additions;
                f.deletions += e.deletions;
                f.binary = f.binary || e.binary;
                // Staged letter wins; fill rename source if the staged side
                // lacked one.
                if let Some((_, old)) = unstaged_letters.get(&e.path) {
                    if f.old_path.is_none() {
                        f.old_path = old.clone();
                    }
                }
            }
            None => {
                let (status, old) = unstaged_letters
                    .get(&e.path)
                    .cloned()
                    .unwrap_or((ChangeStatus::Modified, None));
                map.insert(
                    e.path.clone(),
                    ChangedFile {
                        path: e.path,
                        old_path: old.or(e.old_path),
                        status: ChangeStatusStr(status),
                        additions: e.additions,
                        deletions: e.deletions,
                        binary: e.binary,
                    },
                );
            }
        }
    }
    map.into_values().collect()
}

// -- legacy String wrappers (old routes + agent tools) ----------------------

pub fn status(root: &Path) -> String {
    GitRepo::new(root.to_path_buf())
        .run(&["status", "--porcelain=v1", "-b"])
        .unwrap_or_default()
}

pub fn diff_head(root: &Path, rel: Option<&str>) -> String {
    let repo = GitRepo::new(root.to_path_buf());
    match rel {
        Some(r) => repo.run(&["diff", "HEAD", "--", r]).unwrap_or_default(),
        None => repo.run(&["diff", "HEAD"]).unwrap_or_default(),
    }
}

#[allow(dead_code)]
pub fn merge_base(root: &Path, target: &str) -> String {
    if check_rev(target).is_err() {
        return String::new();
    }
    GitRepo::new(root.to_path_buf())
        .run(&["merge-base", "HEAD", target])
        .unwrap_or_default()
}

fn run_check(root: &Path, args: &[&str]) -> Result<String, String> {
    GitRepo::new(root.to_path_buf())
        .run(args)
        .map_err(|e| e.stderr())
}

/// Dry-run a unified diff against the working tree. Ok(()) means it applies cleanly.
pub fn apply_check(root: &Path, patch: &str) -> Result<(), String> {
    let mut f = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    use std::io::Write;
    f.write_all(patch.as_bytes()).map_err(|e| e.to_string())?;
    run_check(
        root,
        &["apply", "--check", f.path().to_string_lossy().as_ref()],
    )
    .map(|_| ())
    .map_err(|e| format!("patch does not apply: {e}"))
}

/// Apply a unified diff to the working tree (not committed).
pub fn apply(root: &Path, patch: &str) -> Result<(), String> {
    apply_check(root, patch)?;
    let mut f = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    use std::io::Write;
    f.write_all(patch.as_bytes()).map_err(|e| e.to_string())?;
    run_check(root, &["apply", f.path().to_string_lossy().as_ref()]).map(|_| ())
}

/// Revert paths to HEAD and delete listed untracked files. Symlinks are
/// reverted or deleted as links; their targets are never touched.
pub fn revert(root: &Path, tracked: &[String], untracked: &[String]) -> Result<(), String> {
    let safe_tracked = resolve_write_args(root, tracked)?;
    if !safe_tracked.is_empty() {
        let mut args = vec!["checkout", "HEAD", "--"];
        args.extend(safe_tracked.iter().map(|s| s.as_str()));
        run_check(root, &args)?;
    }
    for u in resolve_write_args(root, untracked)? {
        let p = root.join(&u);
        if std::fs::symlink_metadata(&p).is_ok_and(|m| !m.is_dir()) {
            std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn resolve_write_args(root: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    paths
        .iter()
        .map(|p| git_path(root, p, crate::paths::Access::Write).map_err(|e| e.stderr()))
        .collect()
}

/// Lines in a regular file as `--numstat` counts additions (a last line
/// without a newline counts), plus git's binary test (NUL in the first
/// 8 KiB). Streams the file, so size costs no memory. A symlink is one line
/// (git records its target path); FIFOs and devices count as nothing.
fn count_lines(full: &Path) -> (u64, bool) {
    use std::io::Read;
    let Ok(md) = std::fs::symlink_metadata(full) else {
        return (0, false);
    };
    if md.file_type().is_symlink() {
        return (1, false);
    }
    if !md.is_file() {
        return (0, false);
    }
    let Ok(mut f) = std::fs::File::open(full) else {
        return (0, false);
    };
    let mut buf = vec![0u8; 64 * 1024];
    let (mut lines, mut first, mut last) = (0u64, true, b'\n');
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if first {
            if buf[..n.min(8192)].contains(&0) {
                return (0, true);
            }
            first = false;
        }
        lines += memchr::memchr_iter(b'\n', &buf[..n]).count() as u64;
        last = buf[n - 1];
    }
    if last != b'\n' {
        lines += 1;
    }
    (lines, false)
}

/// Remove `dir` and its subdirectories when empty, bottom-up. Never
/// follows symlinks and never removes anything that still has entries.
fn prune_empty_dirs(dir: &Path) {
    let Ok(md) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if !md.is_dir() {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                prune_empty_dirs(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

pub fn stage(root: &Path, paths: &[String]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("no paths".into());
    }
    let safe = resolve_write_args(root, paths)?;
    let mut args = vec!["add", "--"];
    args.extend(safe.iter().map(|s| s.as_str()));
    run_check(root, &args)
}

pub fn unstage(root: &Path, paths: &[String]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("no paths".into());
    }
    let safe = resolve_write_args(root, paths)?;
    let mut args = vec!["reset", "HEAD", "--"];
    args.extend(safe.iter().map(|s| s.as_str()));
    run_check(root, &args)
}

pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    GitRepo::new(root.to_path_buf())
        .commit_msg(message, false)
        .map(|c| c.sha)
        .map_err(|e| e.stderr())
}

pub fn push(root: &Path) -> Result<String, String> {
    GitRepo::new(root.to_path_buf())
        .push()
        .map_err(|e| e.stderr())
}

pub fn pull_ff(root: &Path) -> Result<String, String> {
    GitRepo::new(root.to_path_buf())
        .pull_ff()
        .map(|(o, _)| o)
        .map_err(|e| e.stderr())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let repo = GitRepo::new(r.to_path_buf());
        repo.run(&["init", "-b", "main"]).unwrap();
        repo.run(&["config", "user.email", "t@t"]).unwrap();
        repo.run(&["config", "user.name", "t"]).unwrap();
        repo.run(&["config", "commit.gpgsign", "false"]).unwrap();
        std::fs::write(r.join("a.txt"), "one\n").unwrap();
        repo.run(&["add", "."]).unwrap();
        repo.run(&["commit", "-m", "init"]).unwrap();
        dir
    }

    #[test]
    fn stage_unstage_commit() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        stage(dir.path(), &["a.txt".to_string()]).unwrap();
        assert!(status(dir.path()).contains('M'));
        unstage(dir.path(), &["a.txt".to_string()]).unwrap();
        commit(dir.path(), "should fail").unwrap_err();
        stage(dir.path(), &["a.txt".to_string()]).unwrap();
        let out = commit(dir.path(), "second").unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn status_v2_shapes() {
        let dir = repo();
        let repo = GitRepo::new(dir.path().to_path_buf());
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        // Rename: commit first so the rename is staged.
        std::fs::write(dir.path().join("r.txt"), "r\n").unwrap();
        repo.run(&["add", "."]).unwrap();
        repo.run(&["commit", "-m", "second"]).unwrap();
        repo.run(&["mv", "r.txt", "renamed.txt"]).unwrap();
        let st = repo.status_v2().unwrap();
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert!(!st.detached);
        assert!(st.head_sha.is_some_and(|s| s.len() == 40));
        let ren = st.files.iter().find(|f| f.path == "renamed.txt").unwrap();
        assert_eq!(ren.orig_path.as_deref(), Some("r.txt"));
        assert_eq!(ren.index, Some("R"));
        // Fresh untracked file.
        std::fs::write(dir.path().join("fresh.txt"), "f\n").unwrap();
        let st = repo.status_v2().unwrap();
        let fresh = st.files.iter().find(|f| f.path == "fresh.txt").unwrap();
        assert!(fresh.untracked);
        assert_eq!(st.counts.untracked, 1);
    }

    #[test]
    fn not_a_repo_and_bad_rev() {
        let dir = tempfile::tempdir().unwrap();
        let gr = GitRepo::new(dir.path().to_path_buf());
        assert!(matches!(gr.status_v2(), Err(GitError::NotRepo)));
        let dir = repo();
        let gr = GitRepo::new(dir.path().to_path_buf());
        let err = gr.run(&["rev-parse", "no-such-rev"]).unwrap_err();
        assert!(matches!(err, GitError::Failed { .. }));
        assert!(err.stderr().len() <= STDERR_CAP);
    }

    #[test]
    fn status_poll_during_commits_has_no_lock_errors() {
        // External commits in a loop while polling status: --no-optional-locks
        // must keep index.lock out of the picture.
        let dir = repo();
        let root = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            let r = GitRepo::new(root);
            for i in 0..10 {
                std::fs::write(r.root.join("tick.txt"), format!("{i}\n")).unwrap();
                r.run(&["add", "tick.txt"]).unwrap();
                r.run(&["commit", "-m", &format!("tick {i}")]).unwrap();
            }
        });
        let r = GitRepo::new(dir.path().to_path_buf());
        for _ in 0..50 {
            let st = r.status_v2().expect("status poll must succeed");
            let _ = status_hash(&st);
        }
        handle.join().unwrap();
    }

    #[test]
    fn changes_numstat_and_log() {
        let dir = repo();
        let repo = GitRepo::new(dir.path().to_path_buf());
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.path().join("u.txt"), "l1\nl2\n").unwrap();
        let cs = repo.changes("HEAD", "worktree").unwrap();
        assert_eq!(cs.base, "HEAD");
        assert_eq!(cs.target, "worktree");
        let a = cs.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!((a.additions, a.deletions), (2, 0));
        let u = cs.files.iter().find(|f| f.path == "u.txt").unwrap();
        assert_eq!(u.status.0, ChangeStatus::Untracked);
        assert_eq!(u.additions, 2);
        let log = repo.log(10, None).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].subject, "init");
    }
}
