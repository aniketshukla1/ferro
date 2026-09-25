//! Git v2 (B3): hardened runner, typed status/changes/log, mutations.
//! Every invocation uses `--no-optional-locks`, `-c core.quotepath=off
//! -c color.ui=false`, `LC_ALL=C`, `GIT_OPTIONAL_LOCKS=0`; exit status is
//! checked and stderr (4 KiB) is captured into [`GitError`]. Timeouts: 30 s
//! default, 120 s for push/pull. Legacy String wrappers stay for the old
//! routes and agent tools.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use thiserror::Error;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const NET_TIMEOUT: Duration = Duration::from_secs(120);
/// stderr kept for `git_failed` detail.
const STDERR_CAP: usize = 4096;

#[derive(Debug, Error, Clone)]
pub enum GitError {
    #[error("not a git repository")]
    NotRepo,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("git {args} failed: {stderr}")]
    Failed { args: String, stderr: String },
    #[error("git {args} timed out after {secs}s")]
    Timeout { args: String, secs: u64 },
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
            GitError::NotRepo => "not a git repository".into(),
            GitError::Timeout { args, secs } => format!("{args} timed out after {secs}s"),
            GitError::Cancelled => "cancelled".into(),
            GitError::Io(e) => e.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitRepo {
    pub root: PathBuf,
    batch: std::sync::Arc<std::sync::Mutex<CatFileBatch>>,
}

impl GitRepo {
    pub fn new(root: PathBuf) -> Self {
        let batch = CatFileBatch::new(root.clone());
        Self {
            root,
            batch: std::sync::Arc::new(std::sync::Mutex::new(batch)),
        }
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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
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
        let mut child = self
            .command(args)
            .spawn()
            .map_err(|e| GitError::Io(e.to_string()))?;
        // Drain pipes on threads: a child writing more than the pipe buffer
        // must never block while we poll (large diffs).
        let out_h = child.stdout.take().map(|mut h| {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = Vec::new();
                let _ = h.read_to_end(&mut buf);
                buf
            })
        });
        let err_h = child.stderr.take().map(|mut h| {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = Vec::new();
                let _ = h.read_to_end(&mut buf);
                buf
            })
        });
        let t0 = Instant::now();
        let status = loop {
            if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::Cancelled);
            }
            match child.try_wait().map_err(|e| GitError::Io(e.to_string()))? {
                Some(st) => break st,
                None => {
                    if t0.elapsed() > timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(GitError::Timeout {
                            args: args.join(" "),
                            secs: timeout.as_secs(),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        };
        let stdout = out_h.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = err_h.and_then(|h| h.join().ok()).unwrap_or_default();
        if status.success() {
            return Ok(String::from_utf8_lossy(&stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        if stderr.contains("not a git repository") {
            return Err(GitError::NotRepo);
        }
        let mut err = stderr;
        if err.len() > STDERR_CAP {
            err.truncate(STDERR_CAP);
        }
        Err(GitError::Failed {
            args: args.join(" "),
            stderr: err,
        })
    }

    /// Raw bytes (for `-z` outputs).
    pub fn run_bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        let out = self
            .command(args)
            .stdout(Stdio::piped())
            .output()
            .map_err(|e| GitError::Io(e.to_string()))?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        Err(Self::fail(args, &out.stderr))
    }

    /// Raw bytes for `git diff`: exit code 1 means "differences found" and
    /// still carries the full output on stdout.
    pub fn run_diff_bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        let out = self
            .command(args)
            .stdout(Stdio::piped())
            .output()
            .map_err(|e| GitError::Io(e.to_string()))?;
        if out.status.success() || out.status.code() == Some(1) {
            return Ok(out.stdout);
        }
        Err(Self::fail(args, &out.stderr))
    }

    fn fail(args: &[&str], stderr: &[u8]) -> GitError {
        let stderr = String::from_utf8_lossy(stderr).into_owned();
        if stderr.contains("not a git repository") {
            return GitError::NotRepo;
        }
        let mut err = stderr;
        if err.len() > STDERR_CAP {
            err.truncate(STDERR_CAP);
        }
        GitError::Failed {
            args: args.join(" "),
            stderr: err,
        }
    }

    /// Feed stdin (commit message via `-F -`, never argv).
    pub fn run_stdin(&self, args: &[&str], input: &[u8]) -> Result<String, GitError> {
        use std::io::Write;
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| GitError::Io(e.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input)
                .map_err(|e| GitError::Io(e.to_string()))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| GitError::Io(e.to_string()))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let mut err = stderr;
        if err.len() > STDERR_CAP {
            err.truncate(STDERR_CAP);
        }
        Err(GitError::Failed {
            args: args.join(" "),
            stderr: err,
        })
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

    /// Resolve a `base` form (`HEAD`, `merge-base`, `merge-base:<ref>`, any rev).
    pub fn resolve_base(&self, base: &str) -> Result<String, GitError> {
        if base == "HEAD" {
            return self
                .run(&["rev-parse", "HEAD"])
                .map(|s| s.trim().to_string());
        }
        if base == "merge-base" {
            return self.merge_base_head();
        }
        if let Some(r) = base.strip_prefix("merge-base:") {
            return self
                .run(&["merge-base", "HEAD", r])
                .map(|s| s.trim().to_string());
        }
        self.run(&["rev-parse", base]).map(|s| s.trim().to_string())
    }

    fn merge_base_head(&self) -> Result<String, GitError> {
        // Workspace default: the base is HEAD itself. PR mode (B4) resolves
        // the merge-base against the base ref at the route layer.
        self.run(&["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
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
                for (path, lines) in self.untracked_with_lines()? {
                    match files.iter_mut().find(|f| f.path == path) {
                        Some(f) => {
                            f.additions += lines;
                            if f.status.0 == ChangeStatus::Untracked {
                                f.additions = lines;
                            }
                        }
                        None => files.push(ChangedFile {
                            path,
                            old_path: None,
                            status: ChangeStatusStr(ChangeStatus::Untracked),
                            additions: lines,
                            deletions: 0,
                            binary: false,
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
                let sha = self
                    .run(&["rev-parse", rev])
                    .map(|s| s.trim().to_string())?;
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

    fn untracked_with_lines(&self) -> Result<Vec<(String, u64)>, GitError> {
        let raw = self.run_bytes(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        let mut out = Vec::new();
        for chunk in raw.split(|&b| b == 0) {
            if chunk.len() < 4 || &chunk[..3] != b"?? " {
                continue;
            }
            let path = String::from_utf8_lossy(&chunk[3..]).into_owned();
            let full = self.root.join(&path);
            let lines = std::fs::read(&full)
                .ok()
                .map(|b| {
                    if b.contains(&0) {
                        0
                    } else {
                        b.iter().filter(|&&c| c == b'\n').count() as u64
                    }
                })
                .unwrap_or(0);
            out.push((path, lines));
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
            if sha.len() != 40 {
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
        let root_canon = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        paths
            .iter()
            .map(|p| {
                let abs = crate::paths::resolve(&root_canon, p, crate::paths::Access::Write)
                    .map_err(|e| match e {
                        crate::paths::PathError::Escapes | crate::paths::PathError::Protected => {
                            GitError::Forbidden(e.to_string())
                        }
                        _ => GitError::Failed {
                            args: "resolve".into(),
                            stderr: e.to_string(),
                        },
                    })?;
                abs.strip_prefix(&root_canon)
                    .map(|r| r.to_string_lossy().to_string())
                    .map_err(|_| GitError::Forbidden("path escapes root".into()))
            })
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
        let mut args = vec!["restore", "--staged", "--"];
        args.extend(safe.iter().map(|s| s.as_str()));
        self.run(&args)
    }

    /// Destructive: tracked → restore worktree; untracked → delete.
    pub fn discard_paths(&self, paths: &[String]) -> Result<(), GitError> {
        let safe = self.write_paths(paths)?;
        // Partition via status to avoid deleting tracked content by mistake.
        let st = self.status_v2().unwrap_or_default();
        let mut tracked: Vec<&str> = Vec::new();
        let mut untracked: Vec<&str> = Vec::new();
        for p in &safe {
            match st.files.iter().find(|f| &f.path == p) {
                Some(f) if f.untracked => untracked.push(p),
                _ => tracked.push(p),
            }
        }
        if !tracked.is_empty() {
            let mut args = vec!["restore", "--source=HEAD", "--worktree", "--"];
            args.extend(tracked.iter().copied());
            self.run(&args)?;
        }
        for u in untracked {
            let abs = self.root.join(u);
            if abs.is_file() || abs.is_symlink() {
                std::fs::remove_file(&abs).map_err(|e| GitError::Io(e.to_string()))?;
            } else if abs.is_dir() {
                std::fs::remove_dir_all(&abs).map_err(|e| GitError::Io(e.to_string()))?;
            }
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
        if rev.len() > 256 || path.len() > 512 {
            return Err(GitError::Failed {
                args: "blob".into(),
                stderr: "bad rev/path".into(),
            });
        }
        let sha = self
            .run_bytes(&["rev-parse", "--verify", &format!("{rev}:{path}")])
            .map(|o| String::from_utf8_lossy(&o).trim().to_string())
            .map_err(|_| GitError::Failed {
                args: "blob".into(),
                stderr: "no such blob".into(),
            })?;
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(GitError::Failed {
                args: "blob".into(),
                stderr: "no such blob".into(),
            });
        }
        self.batch
            .lock()
            .map_err(|_| GitError::Io("batch lock".into()))?
            .read(&self.root, &sha)
            .ok_or_else(|| GitError::Failed {
                args: "blob".into(),
                stderr: "no such blob".into(),
            })
    }
}

/// Long-lived `git cat-file --batch` reader (one per workspace, shared by
/// clones). Respawns transparently after any failure.
#[derive(Debug)]
struct CatFileBatch {
    root: PathBuf,
    child: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<std::io::BufReader<std::process::ChildStdout>>,
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

    /// Read one blob; `None` on any failure (caller respawns next time).
    fn read(&mut self, root: &Path, sha: &str) -> Option<Vec<u8>> {
        use std::io::{BufRead, Read, Write};
        let _ = root;
        if !self.ensure() {
            return None;
        }
        let (stdin, stdout) = match (self.stdin.as_mut(), self.stdout.as_mut()) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                self.shutdown();
                return None;
            }
        };
        if writeln!(stdin, "{sha}").is_err() {
            self.shutdown();
            return None;
        }
        stdin.flush().ok()?;
        let mut header = String::new();
        if stdout.read_line(&mut header).is_err() {
            self.shutdown();
            return None;
        }
        // `<sha> <type> <size>`; missing objects reply `<sha> missing`.
        let mut parts = header.split_whitespace();
        let (Some(_), Some(ty), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            self.shutdown();
            return None;
        };
        if ty != "blob" {
            // Drain nothing: non-blob replies carry no body. Resync by restart.
            self.shutdown();
            return None;
        }
        let size: usize = size.parse().ok()?;
        if size > 256 * 1024 * 1024 {
            self.shutdown();
            return None;
        }
        let mut buf = vec![0u8; size];
        if stdout.read_exact(&mut buf).is_err() {
            self.shutdown();
            return None;
        }
        let mut nl = [0u8; 1];
        if stdout.read_exact(&mut nl).is_err() {
            self.shutdown();
            return None;
        }
        Some(buf)
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
            } else if let Some(v) = line.strip_prefix("# branch.abort ") {
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

/// Revert paths to HEAD and delete listed untracked files.
pub fn revert(root: &Path, tracked: &[String], untracked: &[String]) -> Result<(), String> {
    let repo = GitRepo::new(root.to_path_buf());
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut safe_tracked = Vec::with_capacity(tracked.len());
    for t in tracked {
        let p = crate::paths::resolve(&root_canon, t, crate::paths::Access::Write)
            .map_err(|e| e.to_string())?;
        let rel = p
            .strip_prefix(&root_canon)
            .map_err(|_| "path escapes root".to_string())?;
        safe_tracked.push(rel.to_string_lossy().to_string());
    }
    if !safe_tracked.is_empty() {
        let mut args = vec!["checkout", "HEAD", "--"];
        args.extend(safe_tracked.iter().map(|s| s.as_str()));
        run_check(root, &args)?;
    }
    for u in untracked {
        let p = crate::paths::resolve(&root_canon, u, crate::paths::Access::Write)
            .map_err(|e| e.to_string())?;
        if p.is_file() {
            std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        }
    }
    let _ = repo;
    Ok(())
}

fn resolve_write_args(root: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    paths
        .iter()
        .map(|p| {
            let abs = crate::paths::resolve(&root_canon, p, crate::paths::Access::Write)
                .map_err(|e| e.to_string())?;
            abs.strip_prefix(&root_canon)
                .map(|r| r.to_string_lossy().to_string())
                .map_err(|_| "path escapes root".to_string())
        })
        .collect()
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
