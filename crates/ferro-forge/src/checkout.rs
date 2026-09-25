//! PR checkout (B4): persistent bare partial clones + reusable worktrees.
//! Path A (local repo has a matching remote): fetch `pull/<n>/head` into
//! `refs/ferro/pr/<n>` and attach a detached worktree under the state dir.
//! Path B (anything else): a bare `--filter=blob:none` mirror in the cache
//! dir, worktrees beside path A. Reopening reuses the worktree; a moved head
//! is checked out in place. Every git call on an untrusted checkout runs
//! with [`untrusted_env`]; tokens travel via env.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::error::ForgeError;
use super::github::PullMeta;
use super::parse::ForgeRef;

const NET_TIMEOUT: Duration = Duration::from_secs(120);
/// First clone of a large repository (partial, but full history).
const CLONE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// `core.hooksPath` that resolves every hook to nothing.
const NO_HOOKS: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// Config for git calls that touch an untrusted PR checkout: no hooks (a
/// repo whose `core.hooksPath` is in-tree would otherwise run the PR's own
/// hooks), no fsmonitor, no submodule recursion, no file transport. Passed
/// per process, never written: in a linked worktree, `git config` writes
/// the user's own repository config.
pub fn untrusted_config(allow_file_protocol: bool) -> Vec<(String, String)> {
    let file = if allow_file_protocol {
        "always"
    } else {
        "never"
    };
    vec![
        ("core.hooksPath".into(), NO_HOOKS.into()),
        ("core.fsmonitor".into(), "false".into()),
        ("submodule.recurse".into(), "false".into()),
        ("protocol.file.allow".into(), file.into()),
    ]
}

/// [`untrusted_config`] plus `extra` entries (e.g. auth) as one
/// `GIT_CONFIG_*` env set for a child process.
pub fn untrusted_env(
    extra: &[(String, String)],
    allow_file_protocol: bool,
) -> Vec<(String, String)> {
    let mut entries = untrusted_config(allow_file_protocol);
    entries.extend(extra.iter().cloned());
    super::token::config_env(&entries)
}

/// First line of git's stderr for error messages (never carries tokens:
/// those travel in env headers, not URLs).
fn git_msg(e: &ferro_core::git::GitError) -> String {
    e.stderr()
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct CheckoutOpts {
    /// Tests only: allow `file://` / local-path transport in hardened config.
    pub allow_file_protocol: bool,
}

#[derive(Debug, Clone)]
pub struct OpenedPr {
    pub dir: PathBuf,
    pub base_ref: String,
    pub base_sha: String,
    pub head_sha: String,
    pub merge_base: String,
    pub reused: bool,
    /// Remote that serves `pull/<n>/head` for this checkout (`origin` for
    /// the mirror; whichever remote matched in a local repo).
    pub remote: String,
}

/// Central registry of PR worktrees (drives `ferro gc`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorktreeEntry {
    pub dir: PathBuf,
    pub main_repo: PathBuf,
    pub url: String,
    pub opened_at: String,
}

fn registry_path(state_dir: &Path) -> PathBuf {
    state_dir.join("worktrees.json")
}

fn read_registry(state_dir: &Path) -> Vec<WorktreeEntry> {
    let Ok(text) = std::fs::read_to_string(registry_path(state_dir)) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_registry(state_dir: &Path, entries: &[WorktreeEntry]) {
    if std::fs::create_dir_all(state_dir).is_err() {
        return;
    }
    if let Ok(bytes) = serde_json::to_string_pretty(entries) {
        let _ = ferro_core::settings::atomic_write(&registry_path(state_dir), bytes.as_bytes());
    }
}

fn now_iso() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Remove worktrees of merged/closed PRs older than `older_than_days`.
/// `state_of` returns "merged"/"closed"/"open" (None = unknown, skipped).
/// Returns removed dirs. Stale registry entries (dir gone) are dropped.
pub fn gc_worktrees(
    state_dir: &Path,
    older_than_days: u64,
    state_of: &dyn Fn(&ForgeRef) -> Option<String>,
) -> Vec<PathBuf> {
    let entries = read_registry(state_dir);
    let mut kept = Vec::new();
    let mut removed = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for e in entries {
        let age_ok = std::fs::metadata(&e.dir)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| now.saturating_sub(d.as_secs()) > older_than_days * 86400)
            .unwrap_or(false);
        let Some(url) = super::parse::parse_pr_url(&e.url) else {
            if e.dir.exists() {
                kept.push(e);
            }
            continue;
        };
        if !e.dir.exists() {
            // Stale registration; also prune the main repo's list.
            let main = ferro_core::git::GitRepo::new(e.main_repo.clone());
            let _ = main.run(&["worktree", "prune"]);
            continue;
        }
        match state_of(&url).as_deref() {
            Some("merged") | Some("closed") if age_ok => {
                let main = ferro_core::git::GitRepo::new(e.main_repo.clone());
                let _ = main.run(&["worktree", "remove", "--force", &e.dir.to_string_lossy()]);
                let _ = std::fs::remove_dir_all(&e.dir);
                let _ = main.run(&["worktree", "prune"]);
                removed.push(e.dir);
            }
            _ => kept.push(e),
        }
    }
    write_registry(state_dir, &kept);
    removed
}

pub fn worktree_dir(state_dir: &Path, r: &ForgeRef) -> PathBuf {
    state_dir
        .join("worktrees")
        .join(format!("{}-{}-{}-{}", r.host, r.owner, r.repo, r.number))
}

pub fn bare_dir(cache_dir: &Path, r: &ForgeRef) -> PathBuf {
    cache_dir
        .join("repos")
        .join(&r.host)
        .join(&r.owner)
        .join(format!("{}.git", r.repo))
}

/// Does `remote_url` point at the PR's repo? Normalizes schemes, userinfo,
/// ports, scp syntax and `.git` suffixes, then requires the final segments
/// to be exactly `host/owner/repo` (so absolute mirror paths like
/// `/srv/mirrors/github.com/o/r` match too).
pub fn remote_matches(remote_url: &str, r: &ForgeRef) -> bool {
    let mut s = remote_url.trim().to_string();
    for prefix in [
        "https://", "http://", "ssh://", "git://", "ftp://", "ftps://",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
    // scp syntax: git@host:owner/repo.git
    if let Some(at) = s.find('@') {
        let after = &s[at + 1..];
        if after.contains(':') && !after.starts_with('[') {
            s = after.to_string();
        }
    }
    // userinfo without @-split above (https://user@host/...)
    if let Some(slash) = s.find('/') {
        if let Some(at) = s[..slash].find('@') {
            s = s[at + 1..].to_string();
        }
    }
    s = s.trim_end_matches('/').to_string();
    s = s.strip_suffix(".git").unwrap_or(&s).to_string();
    // scp `host:owner/repo` → `host/owner/repo`, but leave `host:port/...`
    // alone (a leading all-digit segment means port, not scp).
    if let Some(colon) = s.find(':') {
        if !s[..colon].contains('/') {
            let first = s[colon + 1..].split('/').next().unwrap_or("");
            if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
                s.replace_range(colon..=colon, "/");
            }
        }
    }
    let segs: Vec<&str> = s.split('/').filter(|x| !x.is_empty()).collect();
    if segs.len() < 3 {
        return false;
    }
    let (host_seg, owner, repo) = (
        segs[segs.len() - 3],
        segs[segs.len() - 2],
        segs[segs.len() - 1],
    );
    // Strip :port from the host segment.
    let host_seg = host_seg.split(':').next().unwrap_or(host_seg);
    host_seg.eq_ignore_ascii_case(&r.host) && owner == r.owner && repo == r.repo
}

/// Open (or reuse) a worktree for the PR.
/// `clone_url` is the base repository URL (tests pass a local path).
/// `local_repo` is a checkout whose remotes may already reach the PR repo.
#[allow(clippy::too_many_arguments)]
pub fn open_pr(
    r: &ForgeRef,
    meta: &PullMeta,
    clone_url: &str,
    local_repo: Option<&Path>,
    dirs: &ferro_core::dirs::FerroDirs,
    token: Option<&str>,
    progress: &dyn Fn(&str),
    opts: &CheckoutOpts,
) -> Result<OpenedPr, ForgeError> {
    progress("metadata");
    let auth_env = token
        .map(|t| super::token::git_auth_env(clone_url, t))
        .unwrap_or_default();
    // Path A: local repo with a matching remote — fetch from that remote,
    // whatever it is called (fork workflows: origin = fork, upstream = PR).
    if let Some(local) = local_repo {
        let probe = ferro_core::git::GitRepo::new(local.to_path_buf()).with_env(auth_env.clone());
        if let Ok(remotes) = probe.run_cancel(&["remote", "-v"], Duration::from_secs(30), None) {
            let matched = remotes.lines().find_map(|l| {
                let mut fields = l.split_whitespace();
                let (name, url) = (fields.next()?, fields.next()?);
                remote_matches(url, r).then(|| name.to_string())
            });
            if let Some(remote) = matched {
                progress("fetch");
                fetch_refs(&probe, &remote, r, &meta.base_ref)?;
                return attach_worktree(r, meta, &probe, &remote, dirs, progress, opts);
            }
        }
    }
    // Path B: persistent bare partial mirror.
    progress("fetch");
    let bare = bare_dir(&dirs.cache_dir, r);
    ensure_mirror(&bare, clone_url, &auth_env)?;
    let mirror = ferro_core::git::GitRepo::new(bare.clone()).with_env(auth_env);
    fetch_refs(&mirror, "origin", r, &meta.base_ref)?;
    attach_worktree(r, meta, &mirror, "origin", dirs, progress, opts)
}

/// Clone the bare partial mirror once, through the hardened runner
/// (timeout, no terminal prompts). The clone lands in a sibling temp dir
/// and is renamed into place, so an interrupted clone never leaves a
/// half-made mirror that later opens would trust.
fn ensure_mirror(
    bare: &Path,
    clone_url: &str,
    auth_env: &[(String, String)],
) -> Result<(), ForgeError> {
    let io = |e: std::io::Error| ForgeError::Schema(e.to_string());
    if bare.exists() {
        let valid = ferro_core::git::GitRepo::new(bare.to_path_buf())
            .run(&["rev-parse", "--is-bare-repository"])
            .is_ok_and(|s| s.trim() == "true");
        if valid {
            return Ok(());
        }
        // Left behind by an interrupted clone (ferro's own cache dir).
        std::fs::remove_dir_all(bare).map_err(io)?;
    }
    let parent = bare
        .parent()
        .ok_or_else(|| ForgeError::Schema("bad mirror path".into()))?;
    std::fs::create_dir_all(parent).map_err(io)?;
    let name = bare
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let runner = ferro_core::git::GitRepo::new(parent.to_path_buf()).with_env(auth_env.to_vec());
    let cloned = runner.run_cancel(
        &[
            "clone",
            "--bare",
            "--filter=blob:none",
            "--",
            clone_url,
            &tmp.to_string_lossy(),
        ],
        CLONE_TIMEOUT,
        None,
    );
    if let Err(e) = cloned {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(ForgeError::Network(format!(
            "clone failed: {}",
            git_msg(&e)
        )));
    }
    std::fs::rename(&tmp, bare).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        io(e)
    })
}

/// Fetch the PR head and base into ferro's refs. `+`: PR heads are
/// force-pushed (rebase, amend) all the time. Full history for a correct
/// merge-base (no depth cap, B4).
fn fetch_refs(
    repo: &ferro_core::git::GitRepo,
    remote: &str,
    r: &ForgeRef,
    base_ref: &str,
) -> Result<(), ForgeError> {
    repo.run_cancel(
        &[
            "fetch",
            "--",
            remote,
            &r.pull_ref(),
            &format!("+{base_ref}:refs/ferro/base"),
        ],
        NET_TIMEOUT,
        None,
    )
    .map_err(|e| ForgeError::Network(format!("fetch PR refs failed: {}", git_msg(&e))))?;
    Ok(())
}

/// Forced refspec for a PR head (shared with PR-mode pull).
pub fn pr_refspec(number: u64) -> String {
    format!("+pull/{number}/head:refs/ferro/pr/{number}")
}

/// Attach (or reuse) the detached worktree of `repo` (local or mirror).
/// An existing worktree is never deleted — it may be the workspace the
/// server is serving: at the same head it is reused as is (local edits
/// kept); at a moved head it is checked out in place when clean, and left
/// alone with an error when it has local changes.
#[allow(clippy::too_many_arguments)]
fn attach_worktree(
    r: &ForgeRef,
    meta: &PullMeta,
    repo: &ferro_core::git::GitRepo,
    remote: &str,
    dirs: &ferro_core::dirs::FerroDirs,
    progress: &dyn Fn(&str),
    opts: &CheckoutOpts,
) -> Result<OpenedPr, ForgeError> {
    progress("worktree");
    let wt = worktree_dir(&dirs.state_dir, r);
    if let Some(parent) = wt.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ForgeError::Schema(e.to_string()))?;
    }
    let head_ref = format!("refs/ferro/pr/{}", r.number);
    // Every git call on the untrusted checkout itself runs hardened.
    let hard = untrusted_env(&[], opts.allow_file_protocol);
    let cur = ferro_core::git::GitRepo::new(wt.clone()).with_env(hard.clone());
    // A worktree git can still read (its repository may have been removed,
    // e.g. a cleared cache, which leaves the directory orphaned).
    let current = wt
        .join(".git")
        .exists()
        .then(|| cur.run_cancel(&["rev-parse", "HEAD"], Duration::from_secs(30), None))
        .and_then(Result::ok)
        .map(|s| s.trim().to_string());
    let reused = if let Some(head) = current {
        if head == meta.head_sha {
            true
        } else {
            let clean = cur
                .status_v2()
                .map(|st| st.files.is_empty())
                .unwrap_or(false);
            if !clean {
                return Err(ForgeError::Schema(
                    "worktree has local changes; stash or discard first".into(),
                ));
            }
            cur.run_cancel(
                &["checkout", "--quiet", "--detach", &head_ref],
                Duration::from_secs(120),
                None,
            )
            .map_err(|e| ForgeError::Schema(format!("checkout failed: {}", git_msg(&e))))?;
            false
        }
    } else {
        // Not a usable worktree (never created, debris of a crashed add, or
        // orphaned from its repository) in ferro's own state dir: start
        // fresh.
        if wt.exists() {
            std::fs::remove_dir_all(&wt).map_err(|e| ForgeError::Schema(e.to_string()))?;
        }
        let adder = repo.clone().with_env(hard);
        let _ = adder.run_cancel(&["worktree", "prune"], Duration::from_secs(30), None);
        adder
            .run_cancel(
                &[
                    "worktree",
                    "add",
                    "--detach",
                    "--",
                    wt.to_str()
                        .ok_or_else(|| ForgeError::Schema("bad path".into()))?,
                    &head_ref,
                ],
                Duration::from_secs(120),
                None,
            )
            .map_err(|e| ForgeError::Schema(format!("worktree add failed: {}", git_msg(&e))))?;
        register_worktree(dirs, r, repo, &wt);
        false
    };
    progress("merge-base");
    let mb = merge_base(&cur, &meta.base_sha)?;
    Ok(OpenedPr {
        dir: wt,
        base_ref: meta.base_ref.clone(),
        base_sha: meta.base_sha.clone(),
        head_sha: meta.head_sha.clone(),
        merge_base: mb,
        reused,
        remote: remote.to_string(),
    })
}

fn merge_base(wt: &ferro_core::git::GitRepo, base_sha: &str) -> Result<String, ForgeError> {
    // The base commit rides along via refs/ferro/base; fall back to the
    // PR's recorded base sha when the ref is missing.
    let mb = wt
        .run_cancel(
            &["merge-base", "HEAD", "refs/ferro/base"],
            Duration::from_secs(30),
            None,
        )
        .or_else(|_| {
            wt.run_cancel(
                &["merge-base", "HEAD", base_sha],
                Duration::from_secs(30),
                None,
            )
        })
        .map_err(|_| ForgeError::Schema("merge-base failed".into()))?;
    Ok(mb.trim().to_string())
}

/// Record the worktree for `ferro gc` (idempotent per dir).
fn register_worktree(
    dirs: &ferro_core::dirs::FerroDirs,
    r: &ForgeRef,
    repo: &ferro_core::git::GitRepo,
    wt: &Path,
) {
    let mut entries = read_registry(&dirs.state_dir);
    if !entries.iter().any(|e| e.dir == wt) {
        entries.push(WorktreeEntry {
            dir: wt.to_path_buf(),
            main_repo: repo.root.clone(),
            url: r.html_url(),
            opened_at: now_iso(),
        });
        write_registry(&dirs.state_dir, &entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_shapes() {
        let r = ForgeRef {
            provider: crate::Provider::GitHub,
            host: "github.com".into(),
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
        };
        for good in [
            "https://github.com/o/r.git",
            "https://github.com/o/r/",
            "http://github.com/o/r",
            "git@github.com:o/r.git",
            "ssh://git@github.com/o/r.git",
            "https://user@github.com/o/r",
            "github.com/o/r",
        ] {
            assert!(remote_matches(good, &r), "{good}");
        }
        for bad in [
            "https://github.com/o/other.git",
            "https://github.com/other/r.git",
            "https://gitlab.com/o/r.git",
            "https://github.com/o/r/pull/1",
            "https://github.com/o",
            "",
        ] {
            assert!(!remote_matches(bad, &r), "{bad}");
        }
        let ghe = ForgeRef {
            provider: crate::Provider::GitHub,
            host: "ghe.corp.example".into(),
            owner: "o".into(),
            repo: "r".into(),
            number: 1,
        };
        assert!(remote_matches("https://ghe.corp.example/o/r.git", &ghe));
        assert!(remote_matches(
            "https://ghe.corp.example:8443/o/r.git",
            &ghe
        ));
    }
}
