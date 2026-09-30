//! External harness registry, dispatch, and git snapshot (B7, Appendix D).
//!
//! A harness runs only after an explicit opt-in. Dispatch closes stdin,
//! puts the child in its own process group, kills that group on cancel,
//! and stops the run at [`HARNESS_TIMEOUT`]. stdout and stderr are kept as
//! a 32 KiB tail. The pre-run snapshot uses a temporary index so the
//! user's real index is never written.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Production cap for one harness edit. Tests pass a shorter duration
/// into [`dispatch`]; the server uses this value.
pub const HARNESS_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Tail kept from each of stdout and stderr.
pub const TAIL_BYTES: usize = 32 * 1024;

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("{0}")]
    Message(String),
    #[error("not a git repository")]
    NotRepo,
    #[error("harness not installed: {0}")]
    NotInstalled(String),
    #[error("unknown harness: {0}")]
    Unknown(String),
}

fn msg(s: impl Into<String>) -> HarnessError {
    HarnessError::Message(s.into())
}

#[derive(Debug, Clone)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    /// Whitespace-separated argv. `{prompt}` is its own argument.
    pub template: &'static str,
}

/// Appendix D. Model flags are not baked in; [`command_argv`] inserts
/// `--model` only when the caller picked one.
pub static PRESETS: &[Preset] = &[
    Preset {
        id: "claude",
        label: "Claude Code",
        template: "claude --permission-mode acceptEdits -p {prompt}",
    },
    Preset {
        id: "codex",
        label: "Codex",
        template: "codex exec --ask-for-approval never {prompt}",
    },
    Preset {
        id: "gemini",
        label: "Gemini",
        template: "gemini --approval-mode auto_edit -p {prompt}",
    },
    Preset {
        id: "cursor-agent",
        label: "Cursor Agent",
        template: "cursor-agent --force -p {prompt}",
    },
    Preset {
        id: "opencode",
        label: "OpenCode",
        template: "opencode run {prompt}",
    },
    Preset {
        id: "aider",
        label: "Aider",
        template: "aider --yes-always --no-auto-commits --message {prompt}",
    },
    Preset {
        id: "goose",
        label: "Goose",
        template: "goose run --no-session -t {prompt}",
    },
    Preset {
        id: "agy",
        label: "Antigravity",
        template: "agy --mode accept-edits -p {prompt}",
    },
];

/// Where to look for harness binaries, and how long a run may last.
#[derive(Debug, Clone)]
pub struct HarnessHost {
    pub timeout: Duration,
    /// `PATH` used for detection. `None` reads the process environment.
    pub path_env: Option<String>,
    /// Home used for the common install directories. `None` reads `$HOME`.
    pub home: Option<PathBuf>,
    /// Extra environment for the child only (never git).
    pub extra_env: Vec<(String, String)>,
    /// Also search `/usr/local/bin`, `/opt/homebrew/bin`, and `/usr/bin`.
    pub scan_system_dirs: bool,
}

impl Default for HarnessHost {
    fn default() -> Self {
        Self {
            timeout: HARNESS_TIMEOUT,
            path_env: None,
            home: None,
            extra_env: Vec::new(),
            scan_system_dirs: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HarnessInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub installed: bool,
    pub models: Vec<String>,
    pub default_model: String,
    pub binary: Option<PathBuf>,
}

#[derive(Debug)]
pub struct DispatchOutput {
    pub exit_code: i32,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub ms: u64,
}

#[derive(Clone)]
struct EditClaim {
    job_id: String,
    path: String,
    start: u32,
    end: u32,
}

/// Running edit ranges. Same path and overlapping inclusive line ranges conflict.
#[derive(Clone, Default)]
pub struct ActiveEdits {
    inner: Arc<Mutex<Vec<EditClaim>>>,
}

impl ActiveEdits {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<EditClaim>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claim `start..=end` on `path`. On conflict, returns the other job id.
    pub fn try_claim(&self, job_id: &str, path: &str, start: u32, end: u32) -> Result<(), String> {
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let mut g = self.lock();
        if let Some(other) = g
            .iter()
            .find(|c| c.path == path && c.start <= end && start <= c.end)
        {
            return Err(other.job_id.clone());
        }
        g.push(EditClaim {
            job_id: job_id.to_string(),
            path: path.to_string(),
            start,
            end,
        });
        Ok(())
    }

    pub fn release(&self, job_id: &str) {
        self.lock().retain(|c| c.job_id != job_id);
    }
}

pub fn detect(host: &HarnessHost) -> Vec<HarnessInfo> {
    PRESETS
        .iter()
        .map(|p| {
            let binary = find_bin(p.id, host);
            HarnessInfo {
                id: p.id,
                label: p.label,
                installed: binary.is_some(),
                models: Vec::new(),
                default_model: String::new(),
                binary,
            }
        })
        .collect()
}

pub fn edit_prompt(path: &str, start: u32, end: u32, instruction: &str) -> String {
    format!("Edit {path} lines {start}-{end}.\n\n{instruction}")
}

/// Resolve a preset to argv. `{prompt}` becomes one argument. `--model` is
/// inserted in front of it only when `model` is non-empty. No shell.
pub fn command_argv(
    id: &str,
    prompt: &str,
    model: Option<&str>,
    host: &HarnessHost,
) -> Result<Vec<String>, HarnessError> {
    let preset = PRESETS
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| HarnessError::Unknown(id.to_string()))?;
    let mut parts: Vec<String> = preset
        .template
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let pos = parts
        .iter()
        .position(|p| p == "{prompt}")
        .ok_or_else(|| msg(format!("template for {id} has no {{prompt}}")))?;
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        parts.insert(pos, model.to_string());
        parts.insert(pos, "--model".to_string());
    }
    let pos = parts
        .iter()
        .position(|p| p == "{prompt}")
        .ok_or_else(|| msg("prompt placeholder disappeared"))?;
    parts[pos] = prompt.to_string();
    let bin =
        find_bin(&parts[0], host).ok_or_else(|| HarnessError::NotInstalled(id.to_string()))?;
    parts[0] = bin.to_string_lossy().into_owned();
    Ok(parts)
}

/// Run `argv` in `cwd`. Stdin is `/dev/null` (immediate EOF). On Unix the
/// child is a new process-group leader; cancel and timeout signal the group.
pub fn dispatch(
    argv: &[String],
    cwd: &Path,
    timeout: Duration,
    cancel: &CancellationToken,
    extra_env: &[(String, String)],
) -> Result<DispatchOutput, HarnessError> {
    if argv.is_empty() {
        return Err(msg("empty command"));
    }
    if cancel.is_cancelled() {
        return Ok(empty_output(137, false, true, 0));
    }
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("GIT_INDEX_FILE");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| msg(format!("spawn {}: {e}", argv[0])))?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || read_tail(stdout));
    let err_thread = std::thread::spawn(move || read_tail(stderr));
    let started = Instant::now();
    let finish = loop {
        if cancel.is_cancelled() {
            stop_child(&mut child, pid);
            break Finish::Cancelled;
        }
        if started.elapsed() >= timeout {
            stop_child(&mut child, pid);
            break Finish::Timeout;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Finish::Exited(status.code().unwrap_or(137)),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                stop_child(&mut child, pid);
                let _ = out_thread.join();
                let _ = err_thread.join();
                return Err(msg(format!("wait: {e}")));
            }
        }
    };
    let (stdout_tail, stdout_truncated) = out_thread.join().unwrap_or_default();
    let (stderr_tail, stderr_truncated) = err_thread.join().unwrap_or_default();
    let (stdout_tail, _) = crate::redact::redact_text(&stdout_tail);
    let (stderr_tail, _) = crate::redact::redact_text(&stderr_tail);
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (exit_code, timed_out, cancelled) = match finish {
        Finish::Cancelled => (137, false, true),
        Finish::Timeout => (124, true, false),
        Finish::Exited(code) => (code, false, false),
    };
    Ok(DispatchOutput {
        exit_code,
        stdout_tail,
        stderr_tail,
        stdout_truncated,
        stderr_truncated,
        timed_out,
        cancelled,
        ms,
    })
}

enum Finish {
    Exited(i32),
    Timeout,
    Cancelled,
}

fn empty_output(exit_code: i32, timed_out: bool, cancelled: bool, ms: u64) -> DispatchOutput {
    DispatchOutput {
        exit_code,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        timed_out,
        cancelled,
        ms,
    }
}

fn stop_child(child: &mut std::process::Child, pid: u32) {
    kill_group(pid);
    let _ = child.kill();
    let _ = child.wait();
}

fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        if pid <= 0 {
            return;
        }
        unsafe {
            extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            // SAFETY: `pid` is a child this process spawned as a process-group
            // leader. A negative pid delivers the signal to that group.
            // 9 is SIGKILL.
            let _ = kill(-pid, 9);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

struct TailBuf {
    buf: Vec<u8>,
    truncated: bool,
}

impl TailBuf {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            truncated: false,
        }
    }

    fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if self.buf.len() + data.len() <= TAIL_BYTES {
            self.buf.extend_from_slice(data);
            return;
        }
        self.truncated = true;
        if data.len() >= TAIL_BYTES {
            let start = data.len() - TAIL_BYTES;
            self.buf.clear();
            self.buf.extend_from_slice(&data[start..]);
        } else {
            let overflow = self.buf.len() + data.len() - TAIL_BYTES;
            self.buf.drain(..overflow);
            self.buf.extend_from_slice(data);
        }
    }

    fn into_string(self) -> (String, bool) {
        let mut start = 0;
        while start < self.buf.len() && self.buf[start] & 0b1100_0000 == 0b1000_0000 {
            start += 1;
        }
        let s = String::from_utf8_lossy(&self.buf[start..]).into_owned();
        (s, self.truncated)
    }
}

fn read_tail(pipe: Option<impl Read>) -> (String, bool) {
    let Some(mut pipe) = pipe else {
        return (String::new(), false);
    };
    let mut tail = TailBuf::new();
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => tail.push(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    tail.into_string()
}

fn path_env_of(host: &HarnessHost) -> String {
    host.path_env
        .clone()
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default())
}

fn home_of(host: &HarnessHost) -> PathBuf {
    if let Some(h) = &host.home {
        return h.clone();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn candidate_dirs(home: &Path, scan_system: bool) -> Vec<PathBuf> {
    let mut dirs = vec![
        home.join(".local/bin"),
        home.join("bin"),
        home.join(".cargo/bin"),
        home.join(".npm-global/bin"),
        home.join(".claude/local"),
        home.join(".claude/bin"),
        home.join(".cursor/bin"),
        home.join(".local/share/cursor-agent"),
        home.join(".opencode/bin"),
        home.join(".goose/bin"),
        home.join(".codex/bin"),
        home.join(".gemini/bin"),
        home.join(".antigravity/bin"),
        home.join(".agy/bin"),
        home.join(".local/pipx/venvs/aider/bin"),
    ];
    if scan_system {
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/bin"));
    }
    dirs
}

fn is_exec(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    executable(&meta)
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    true
}

fn find_bin(name: &str, host: &HarnessHost) -> Option<PathBuf> {
    for dir in std::env::split_paths(&path_env_of(host)) {
        let candidate = dir.join(name);
        if is_exec(&candidate) {
            return Some(candidate);
        }
    }
    let home = home_of(host);
    for dir in candidate_dirs(&home, host.scan_system_dirs) {
        let candidate = dir.join(name);
        if is_exec(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_hex_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// `GIT_INDEX_FILE=<tmp> git add -A`, then `git write-tree`.
/// The repository's real index is not the target of either command.
pub fn snapshot_tree(root: &Path) -> Result<String, HarnessError> {
    let tmp = tempfile::tempdir().map_err(|e| msg(e.to_string()))?;
    let index = tmp.path().join("index");
    git(root, &["add", "-A"], Some(&index))?;
    let out = git(root, &["write-tree"], Some(&index))?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if !is_hex_sha(&sha) {
        return Err(msg(format!("write-tree returned {sha:?}")));
    }
    Ok(sha)
}

/// Paths that differ between `base_tree` and a fresh worktree snapshot.
/// Includes files the run created or deleted. Sorted, unique.
pub fn changed_paths(root: &Path, base_tree: &str) -> Result<Vec<String>, HarnessError> {
    if !is_hex_sha(base_tree) {
        return Err(msg("bad snapshot"));
    }
    let after = snapshot_tree(root)?;
    if after == base_tree {
        return Ok(Vec::new());
    }
    let out = git(
        root,
        &["diff-tree", "-r", "--name-only", "-z", base_tree, &after],
        None,
    )?;
    let mut paths = Vec::new();
    for rec in out.split(|b| *b == 0) {
        if rec.is_empty() {
            continue;
        }
        let path = String::from_utf8_lossy(rec).into_owned();
        if path.is_empty() || path.contains('\n') || path.contains('\0') {
            continue;
        }
        paths.push(path);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Restore `paths` from `tree`. Files absent from the snapshot are deleted.
/// The user's index is not checked out.
pub fn revert_paths(
    root: &Path,
    tree: &str,
    paths: &[String],
) -> Result<Vec<String>, HarnessError> {
    if !is_hex_sha(tree) {
        return Err(msg("bad snapshot"));
    }
    let mut reverted = Vec::with_capacity(paths.len());
    for path in paths {
        let rel = ferro_core::paths::git_rel(root, path, ferro_core::paths::Access::Write)
            .map_err(|e| msg(e.to_string()))?;
        match tree_entry(root, tree, &rel)? {
            Some((mode, sha)) => {
                let bytes = git(root, &["cat-file", "blob", &sha], None)?;
                write_entry(root, &rel, &mode, &bytes)?;
            }
            None => delete_entry(root, &rel)?,
        }
        reverted.push(rel);
    }
    Ok(reverted)
}

fn tree_entry(
    root: &Path,
    tree: &str,
    rel: &str,
) -> Result<Option<(String, String)>, HarnessError> {
    let out = git(root, &["ls-tree", "-z", tree, "--", rel], None)?;
    if out.is_empty() {
        return Ok(None);
    }
    let rec = out.split(|b| *b == 0).next().unwrap_or(&[]);
    if rec.is_empty() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(rec);
    let (meta, _) = text.split_once('\t').unwrap_or((text.as_ref(), ""));
    let mut parts = meta.split_whitespace();
    let mode = parts.next().unwrap_or("").to_string();
    let _kind = parts.next();
    let sha = parts.next().unwrap_or("").to_string();
    if !is_hex_sha(&sha) {
        return Ok(None);
    }
    Ok(Some((mode, sha)))
}

fn write_entry(root: &Path, rel: &str, mode: &str, bytes: &[u8]) -> Result<(), HarnessError> {
    let dest = root.join(rel);
    if mode == "120000" {
        #[cfg(unix)]
        {
            let target = String::from_utf8_lossy(bytes);
            let target = target.trim_end_matches('\n');
            if dest.symlink_metadata().is_ok() {
                std::fs::remove_file(&dest).map_err(|e| msg(e.to_string()))?;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| msg(e.to_string()))?;
            }
            std::os::unix::fs::symlink(target, &dest).map_err(|e| msg(e.to_string()))?;
            return Ok(());
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| msg(e.to_string()))?;
    }
    if dest
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        std::fs::remove_file(&dest).map_err(|e| msg(e.to_string()))?;
    }
    // Unlink first when the worktree entry is a symlink, then create the
    // regular file with `O_NOFOLLOW` so a link planted in between cannot
    // redirect the write.
    ferro_core::paths::write_nofollow(&dest, bytes).map_err(|e| msg(e.to_string()))?;
    #[cfg(unix)]
    if mode == "100755" {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)
            .map_err(|e| msg(e.to_string()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms).map_err(|e| msg(e.to_string()))?;
    }
    Ok(())
}

fn delete_entry(root: &Path, rel: &str) -> Result<(), HarnessError> {
    let dest = root.join(rel);
    let Ok(meta) = dest.symlink_metadata() else {
        return Ok(());
    };
    if meta.is_dir() && !meta.file_type().is_symlink() {
        return Err(msg(format!("refusing to delete directory {rel}")));
    }
    std::fs::remove_file(&dest).map_err(|e| msg(e.to_string()))
}

fn git(root: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>, HarnessError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .arg("-c")
        .arg("core.quotepath=off")
        .arg("-c")
        .arg("core.autocrlf=false")
        // A repository `core.fsmonitor` or hook would run during `git add`
        // even though this index is temporary. Point both at nothing.
        .arg("-c")
        .arg("core.fsmonitor=")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .args(args)
        // Drop the process environment. A repository clean filter runs
        // inside `git add` and would otherwise see provider tokens.
        .env_clear()
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(std::env::var_os("HOME").map(|v| ("HOME", v)))
        .envs(std::env::var_os("TMPDIR").map(|v| ("TMPDIR", v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    } else {
        cmd.env_remove("GIT_INDEX_FILE");
    }
    let out = cmd.output().map_err(|e| msg(e.to_string()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        if stderr.contains("not a git repository") {
            return Err(HarnessError::NotRepo);
        }
        let stderr = clip(stderr, 500);
        let (stderr, _) = crate::redact::redact_text(&stderr);
        return Err(msg(format!("git {} failed: {stderr}", args.join(" "))));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn host_at(home: &Path, path: &Path) -> HarnessHost {
        HarnessHost {
            timeout: Duration::from_millis(200),
            path_env: Some(path.to_string_lossy().into_owned()),
            home: Some(home.to_path_buf()),
            extra_env: Vec::new(),
            scan_system_dirs: false,
        }
    }

    fn write_exec(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn init_repo(dir: &Path) {
        let git = |args: &[&str]| {
            let st = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap();
            assert!(st.success(), "{args:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["config", "core.autocrlf", "false"]);
        git(&["config", "advice.statusHints", "false"]);
    }

    fn git_status(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["status"])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn index_bytes(dir: &Path) -> Vec<u8> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--git-path", "index"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let path = if Path::new(&rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            dir.join(rel)
        };
        std::fs::read(path).unwrap()
    }

    fn pid_alive(pid: i32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn timeout_constant_is_ten_minutes() {
        assert_eq!(HARNESS_TIMEOUT, Duration::from_secs(600));
        assert_eq!(TAIL_BYTES, 32 * 1024);
    }

    #[test]
    fn presets_carry_a_prompt_placeholder() {
        assert_eq!(PRESETS.len(), 8);
        for p in PRESETS {
            let tokens: Vec<_> = p.template.split_whitespace().collect();
            assert_eq!(tokens[0], p.id, "{}", p.id);
            assert!(tokens.contains(&"{prompt}"), "{}", p.template);
        }
    }

    #[test]
    fn detects_path_and_common_dirs_only_when_executable() {
        let home = tempfile::tempdir().unwrap();
        let path = tempfile::tempdir().unwrap();
        write_exec(&path.path().join("claude"), "#!/bin/sh\nexit 0\n");
        std::fs::create_dir_all(home.path().join(".local/bin")).unwrap();
        std::fs::write(home.path().join(".local/bin/aider"), "nope").unwrap();
        let host = host_at(home.path(), path.path());
        let found = detect(&host);
        let claude = found.iter().find(|h| h.id == "claude").unwrap();
        assert!(claude.installed);
        assert!(claude.models.is_empty());
        assert!(claude.default_model.is_empty());
        let aider = found.iter().find(|h| h.id == "aider").unwrap();
        assert!(!aider.installed, "non-executable files are not installed");

        write_exec(&home.path().join(".local/bin/goose"), "#!/bin/sh\nexit 0\n");
        let host = host_at(home.path(), path.path());
        let goose = detect(&host).into_iter().find(|h| h.id == "goose").unwrap();
        assert!(goose.installed);
        assert!(goose.binary.unwrap().ends_with("goose"));
    }

    #[test]
    fn model_flag_is_added_only_when_chosen() {
        let home = tempfile::tempdir().unwrap();
        let path = tempfile::tempdir().unwrap();
        write_exec(&path.path().join("claude"), "#!/bin/sh\nexit 0\n");
        let host = host_at(home.path(), path.path());
        let plain = command_argv("claude", "hello; rm -rf /", None, &host).unwrap();
        assert!(plain[0].ends_with("claude"));
        assert_eq!(
            &plain[1..],
            ["--permission-mode", "acceptEdits", "-p", "hello; rm -rf /"]
        );
        let picked = command_argv("claude", "p", Some("demo-model"), &host).unwrap();
        assert_eq!(&picked[picked.len() - 3..], ["--model", "demo-model", "p"]);
        assert!(command_argv("nope", "p", None, &host).is_err());
    }

    #[test]
    fn line_ranges_conflict_only_when_they_overlap() {
        let edits = ActiveEdits::default();
        edits.try_claim("j1", "a.txt", 1, 10).unwrap();
        assert!(edits.try_claim("j2", "a.txt", 10, 12).is_err());
        assert!(edits.try_claim("j3", "a.txt", 11, 20).is_ok());
        assert!(edits.try_claim("j4", "b.txt", 1, 10).is_ok());
        edits.release("j1");
        assert!(edits.try_claim("j5", "a.txt", 1, 4).is_ok());
    }

    #[test]
    fn snapshot_leaves_git_status_and_the_index_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap()
                .success());
        };
        git(&["add", "a.txt"]);
        git(&["commit", "-m", "init"]);
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        git(&["add", "b.txt"]);
        std::fs::write(dir.path().join("c.txt"), "untracked\n").unwrap();
        // `git status` may refresh the index. Compare the index from after
        // that refresh to the index after the snapshot.
        let status_before = git_status(dir.path());
        let index_before = index_bytes(dir.path());
        let sha = snapshot_tree(dir.path()).unwrap();
        assert_eq!(sha.len(), 40);
        let index_after = index_bytes(dir.path());
        let status_after = git_status(dir.path());
        assert_eq!(index_before, index_after, "snapshot rewrote the real index");
        assert_eq!(status_before, status_after, "git status output changed");
        assert!(status_after.contains("a.txt"));
        assert!(status_after.contains("b.txt"));
        assert!(status_after.contains("c.txt"));
    }

    #[test]
    fn changed_is_exact_and_revert_restores_bytes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "old\n").unwrap();
        std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
        std::fs::write(dir.path().join("gone.txt"), "gone-bytes\n").unwrap();
        std::fs::write(dir.path().join("dirty.txt"), "base\n").unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap()
                .success());
        };
        git(&["add", "."]);
        git(&["commit", "-m", "init"]);
        std::fs::write(dir.path().join("dirty.txt"), "base\ndirty\n").unwrap();
        let dirty = std::fs::read(dir.path().join("dirty.txt")).unwrap();
        let old_a = std::fs::read(dir.path().join("a.txt")).unwrap();
        let old_gone = std::fs::read(dir.path().join("gone.txt")).unwrap();

        let bin = tempfile::tempdir().unwrap();
        write_exec(
            &bin.path().join("claude"),
            "#!/bin/sh\nprintf 'agent\\n' > a.txt\nprintf 'created\\n' > created.txt\nrm -f gone.txt\n",
        );
        let host = host_at(dir.path(), bin.path());
        let tree = snapshot_tree(dir.path()).unwrap();
        let argv = command_argv("claude", "edit", None, &host).unwrap();
        let cancel = CancellationToken::new();
        let out = dispatch(&argv, dir.path(), Duration::from_secs(5), &cancel, &[]).unwrap();
        assert_eq!(out.exit_code, 0, "{}", out.stderr_tail);
        let changed = changed_paths(dir.path(), &tree).unwrap();
        assert_eq!(
            changed,
            vec![
                "a.txt".to_string(),
                "created.txt".to_string(),
                "gone.txt".to_string()
            ]
        );
        let reverted = revert_paths(dir.path(), &tree, &changed).unwrap();
        assert_eq!(reverted, changed);
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), old_a);
        assert_eq!(
            std::fs::read(dir.path().join("gone.txt")).unwrap(),
            old_gone
        );
        assert!(!dir.path().join("created.txt").exists());
        assert_eq!(std::fs::read(dir.path().join("dirty.txt")).unwrap(), dirty);
        assert_eq!(
            std::fs::read(dir.path().join("keep.txt")).unwrap(),
            b"keep\n"
        );
    }

    #[test]
    fn tails_are_truncated_and_say_so() {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut script = String::from("#!/bin/sh\nprintf '%s' 'HEAD");
        script.push_str(&"A".repeat(40_000));
        script.push_str("TAIL'\nprintf '%s' 'HEAD");
        script.push_str(&"B".repeat(40_000));
        script.push_str("TAIL' >&2\n");
        write_exec(&bin.path().join("claude"), &script);
        let host = host_at(home.path(), bin.path());
        let argv = command_argv("claude", "p", None, &host).unwrap();
        let cancel = CancellationToken::new();
        let out = dispatch(&argv, home.path(), Duration::from_secs(5), &cancel, &[]).unwrap();
        assert!(out.stdout_truncated);
        assert!(out.stderr_truncated);
        assert!(out.stdout_tail.ends_with("TAIL"));
        assert!(out.stderr_tail.ends_with("TAIL"));
        assert!(!out.stdout_tail.contains("HEAD"));
        assert!(!out.stderr_tail.contains("HEAD"));
        assert_eq!(out.stdout_tail.len(), TAIL_BYTES);
        assert_eq!(out.stderr_tail.len(), TAIL_BYTES);
        assert!(out.stderr_tail.contains('B'));
        assert!(!out.stdout_tail.contains('B'));
    }

    #[test]
    fn stdin_is_closed() {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        write_exec(
            &bin.path().join("claude"),
            "#!/bin/sh\nif IFS= read -r _x; then exit 3; fi\nexit 0\n",
        );
        let host = host_at(home.path(), bin.path());
        let argv = command_argv("claude", "p", None, &host).unwrap();
        let out = dispatch(
            &argv,
            home.path(),
            Duration::from_secs(5),
            &CancellationToken::new(),
            &[],
        )
        .unwrap();
        assert_eq!(
            out.exit_code, 0,
            "stdin delivered data: {}",
            out.stderr_tail
        );
    }

    #[test]
    fn timeout_kills_the_process() {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let pid_file = home.path().join("pid");
        write_exec(
            &bin.path().join("claude"),
            &format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
                pid_file.display()
            ),
        );
        let host = host_at(home.path(), bin.path());
        let argv = command_argv("claude", "p", None, &host).unwrap();
        let out = dispatch(
            &argv,
            home.path(),
            Duration::from_secs(2),
            &CancellationToken::new(),
            &[],
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, 124);
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut alive = true;
        for _ in 0..20 {
            if !pid_alive(pid) {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!alive, "timed-out process {pid} is still running");
    }

    #[test]
    fn cancel_kills_the_process_group() {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let child_pid = home.path().join("child.pid");
        write_exec(
            &bin.path().join("claude"),
            &format!(
                "#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n",
                child_pid.display()
            ),
        );
        let host = host_at(home.path(), bin.path());
        let argv = command_argv("claude", "p", None, &host).unwrap();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let cwd = home.path().to_path_buf();
        let started = Arc::new(AtomicBool::new(false));
        let flag = started.clone();
        let handle = std::thread::spawn(move || {
            flag.store(true, Ordering::SeqCst);
            dispatch(&argv, &cwd, HARNESS_TIMEOUT, &cancel2, &[])
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(&child_pid) {
                if let Ok(pid) = text.trim().parse::<i32>() {
                    break pid;
                }
            }
            assert!(Instant::now() < deadline, "child pid file never appeared");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(pid_alive(pid));
        cancel.cancel();
        let out = handle.join().unwrap().unwrap();
        assert!(out.cancelled);
        let mut alive = true;
        for _ in 0..20 {
            if !pid_alive(pid) {
                alive = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!alive, "child {pid} survived cancel");
        assert!(started.load(Ordering::SeqCst));
    }

    #[test]
    fn dispatch_redacts_secrets_and_does_not_shell_the_prompt() {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let marker = home.path().join("pwned");
        write_exec(
            &bin.path().join("claude"),
            "#!/bin/sh\nprintf '%s\\n' 'AKIAIOSFODNN7EXAMPLE'\nprintf '%s\\n' 'api_key: \"abcdefghijklmnop\"' >&2\nexit 0\n",
        );
        let host = host_at(home.path(), bin.path());
        let prompt = format!("hello; touch '{}'", marker.display());
        let argv = command_argv("claude", &prompt, None, &host).unwrap();
        assert_eq!(argv.last().map(String::as_str), Some(prompt.as_str()));
        let out = dispatch(
            &argv,
            home.path(),
            Duration::from_secs(5),
            &CancellationToken::new(),
            &[],
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "{}", out.stderr_tail);
        assert!(
            !out.stdout_tail.contains("AKIAIOSFODNN7EXAMPLE"),
            "{}",
            out.stdout_tail
        );
        assert!(out.stdout_tail.contains("[REDACTED]"));
        assert!(
            !out.stderr_tail.contains("abcdefghijklmnop"),
            "{}",
            out.stderr_tail
        );
        assert!(out.stderr_tail.contains("[REDACTED]"));
        assert!(!marker.exists(), "prompt was passed to a shell");
    }

    #[test]
    fn snapshot_does_not_run_fsmonitor() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let marker = dir.path().join("fsmon-ran");
        let script = dir.path().join("fsm.sh");
        write_exec(
            &script,
            &format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
        );
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["config", "core.fsmonitor"])
            .arg(&script)
            .status()
            .unwrap()
            .success());
        snapshot_tree(dir.path()).unwrap();
        assert!(!marker.exists(), "snapshot ran core.fsmonitor");
    }

    #[test]
    fn snapshot_filters_do_not_see_process_secrets() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let marker = dir.path().join("leaked");
        let script = dir.path().join("filt.sh");
        write_exec(
            &script,
            &format!(
                "#!/bin/sh\nprintenv FERRO_HARNESS_SECRET > '{}' || true\ncat\n",
                marker.display()
            ),
        );
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["config", "filter.pwn.clean", &script.to_string_lossy()])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["config", "filter.pwn.required", "true"])
            .status()
            .unwrap()
            .success());
        std::fs::write(dir.path().join(".gitattributes"), "* filter=pwn\n").unwrap();
        std::fs::write(dir.path().join("a.txt"), "b\n").unwrap();
        let key = "FERRO_HARNESS_SECRET";
        let value = "not-a-pattern-value";
        // SAFETY: the variable is test-only and removed before return.
        // No other test reads it.
        unsafe { std::env::set_var(key, value) };
        let ran = snapshot_tree(dir.path());
        unsafe { std::env::remove_var(key) };
        ran.expect("snapshot");
        let leaked = std::fs::read_to_string(&marker).unwrap_or_default();
        assert!(
            marker.exists(),
            "clean filter did not run; cannot tell whether the environment was hidden"
        );
        assert!(
            !leaked.contains(value),
            "clean filter saw the process environment: {leaked}"
        );
    }

    #[test]
    fn revert_restores_pre_snapshot_dirty_bytes_and_drops_later_edits() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "base\n").unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", "a.txt"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["commit", "-m", "init"])
            .status()
            .unwrap()
            .success());
        std::fs::write(dir.path().join("a.txt"), "base\ndirty\n").unwrap();
        let tree = snapshot_tree(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "base\ndirty\nlater\n").unwrap();
        revert_paths(dir.path(), &tree, &["a.txt".into()]).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).unwrap(),
            b"base\ndirty\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn revert_paths_cannot_write_outside() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "old\n").unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", "a.txt"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["commit", "-m", "init"])
            .status()
            .unwrap()
            .success());
        let tree = snapshot_tree(dir.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let created = outside.path().join("created.txt");
        let abs = created.to_string_lossy().to_string();
        assert!(revert_paths(dir.path(), &tree, &[abs]).is_err());
        assert!(revert_paths(dir.path(), &tree, &["../created.txt".into()]).is_err());
        assert!(revert_paths(dir.path(), &tree, &["a/../../created.txt".into()]).is_err());
        assert!(revert_paths(dir.path(), &tree, &[".GIT/hooks/pwn".into()]).is_err());
        assert!(!created.exists());
        assert!(!dir.path().join(".git/hooks/pwn").exists());

        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        assert!(revert_paths(dir.path(), &tree, &["out/created.txt".into()]).is_err());
        assert!(!created.exists());

        // Fullwidth dots are not a `..` component. A missing snapshot entry
        // is a delete, so this must not create a file outside the root.
        let weird = "\u{ff0e}\u{ff0e}/pwned.txt";
        revert_paths(dir.path(), &tree, &[weird.into()]).unwrap();
        assert!(!created.exists());
        assert!(!outside.path().join("pwned.txt").exists());

        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        std::os::unix::fs::symlink(&created, dir.path().join("a.txt")).unwrap();
        let reverted = revert_paths(dir.path(), &tree, &["a.txt".into()]).unwrap();
        assert_eq!(reverted, vec!["a.txt".to_string()]);
        assert!(!created.exists(), "revert followed a symlink");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"old\n");
        assert!(!std::fs::symlink_metadata(dir.path().join("a.txt"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
