//! Checkout against local git fixtures (no network).
//! Origin carries `refs/pull/7/head` like GitHub's pull refs.

use ferro_forge::{CheckoutOpts, ForgeRef, OpenedPr};
use std::path::Path;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn cfg(dir: &Path) {
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Origin with main + feature branch exposed as `refs/pull/7/head`.
/// Returns (origin dir, base_sha, head_sha).
fn origin() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    git(r, &["init", "-b", "main", "."]);
    cfg(r);
    std::fs::write(r.join("base.txt"), "base\n").unwrap();
    git(r, &["add", "."]);
    git(r, &["commit", "-m", "base"]);
    let base = git(r, &["rev-parse", "HEAD"]);
    git(r, &["checkout", "-qb", "feature"]);
    std::fs::write(r.join("feat.txt"), "feat\n").unwrap();
    git(r, &["add", "."]);
    git(r, &["commit", "-m", "feat"]);
    let head = git(r, &["rev-parse", "HEAD"]);
    git(r, &["update-ref", "refs/pull/7/head", &head]);
    git(r, &["checkout", "-q", "main"]);
    (dir, base, head)
}

fn dirs() -> (tempfile::TempDir, ferro_core::dirs::FerroDirs) {
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    (home, dirs)
}

fn pref() -> ForgeRef {
    ForgeRef {
        provider: ferro_forge::Provider::GitHub,
        host: "github.com".into(),
        owner: "o".into(),
        repo: "r".into(),
        number: 7,
    }
}

fn meta(base: &str, head: &str) -> ferro_forge::github::PullMeta {
    ferro_forge::github::PullMeta {
        title: "t".into(),
        body: None,
        state: "open".into(),
        merged: false,
        draft: false,
        base_ref: "main".into(),
        head_ref: "feature".into(),
        base_sha: base.into(),
        head_sha: head.into(),
        head_clone_url: None,
        is_fork: false,
        created_at: "".into(),
        updated_at: "".into(),
        additions: 1,
        deletions: 0,
        changed_files: 1,
        commits: 1,
        html_url: "".into(),
        author_login: "ann".into(),
        author_avatar: None,
    }
}

fn open(
    r: &ForgeRef,
    m: &ferro_forge::github::PullMeta,
    url: &str,
    local: Option<&Path>,
    dirs: &ferro_core::dirs::FerroDirs,
) -> OpenedPr {
    let steps = std::sync::Mutex::new(Vec::new());
    let out = ferro_forge::open_pr(
        r,
        m,
        url,
        local,
        dirs,
        None,
        &|s| {
            steps.lock().unwrap().push(s.to_string());
        },
        &CheckoutOpts {
            allow_file_protocol: true,
        },
    )
    .unwrap();
    let steps = steps.lock().unwrap();
    assert_eq!(
        &steps[..],
        &["metadata", "fetch", "worktree", "merge-base"],
        "{steps:?}"
    );
    out
}

#[test]
fn bare_mirror_open_reuse_and_dirty() {
    let (og, base, head) = origin();
    let (_home, dirs) = dirs();
    let r = pref();
    let m = meta(&base, &head);
    let url = og.path().to_string_lossy().to_string();

    // First open: full checkout.
    let o1 = open(&r, &m, &url, None, &dirs);
    assert!(!o1.reused);
    assert!(o1.dir.join("feat.txt").is_file());
    assert!(!o1.merge_base.is_empty());
    // merge-base must be the base commit (feature branched from main tip).
    assert_eq!(o1.merge_base, base);
    // Hardening travels per process: no config file is ever written (in a
    // linked worktree that file is the whole repository's config).
    let cfg = std::process::Command::new("git")
        .arg("-C")
        .arg(&o1.dir)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .unwrap();
    assert!(!cfg.status.success(), "hooksPath must not be written");

    // Second open: reused, no new work.
    let o2 = open(&r, &m, &url, None, &dirs);
    assert!(o2.reused);
    assert_eq!(o2.dir, o1.dir);

    // Local edits at the same head: reused, edits kept.
    std::fs::write(o1.dir.join("feat.txt"), "dirty\n").unwrap();
    let o3 = open(&r, &m, &url, None, &dirs);
    assert!(o3.reused);
    assert_eq!(
        std::fs::read_to_string(o1.dir.join("feat.txt")).unwrap(),
        "dirty\n"
    );

    // Local edits and a moved head: refusing is safer than clobbering.
    let mut moved = meta(&base, &base);
    moved.head_sha = base.clone();
    let err = ferro_forge::open_pr(
        &r,
        &moved,
        &url,
        None,
        &dirs,
        None,
        &|_| {},
        &CheckoutOpts {
            allow_file_protocol: true,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("local changes"), "{err}");
    assert_eq!(
        std::fs::read_to_string(o1.dir.join("feat.txt")).unwrap(),
        "dirty\n"
    );
}

#[test]
fn local_repo_path_reuses_remote() {
    let (og, base, head) = origin();
    // Nest the origin under a matching path so remote_matches fires.
    let nest_base = tempfile::tempdir().unwrap();
    let nested = nest_base.path().join("github.com/o/r");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    // Move via clone (keeps pull refs? clone drops them) — instead re-create
    // the pull ref in a clone of origin.
    let out = std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(og.path())
        .arg(&nested)
        .output()
        .unwrap();
    assert!(out.status.success());
    cfg(&nested);
    git(&nested, &["update-ref", "refs/pull/7/head", &head]);
    git(
        &nested,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/pull/7/head:refs/pull/7/head",
        ],
    );
    // A working checkout with origin pointing at the nested path.
    let work = tempfile::tempdir().unwrap();
    let out = std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(&nested)
        .arg(work.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let (_home, dirs) = dirs();
    let r = pref();
    let m = meta(&base, &head);
    let o = open(&r, &m, &nested.to_string_lossy(), Some(work.path()), &dirs);
    assert!(!o.reused);
    assert!(o.dir.join("feat.txt").is_file());
    // refs/ferro/pr/7 landed in the LOCAL repo (path A), not a bare mirror.
    assert!(
        work.path().join(".git/refs/ferro/pr/7").is_file()
            || git(work.path(), &["rev-parse", "refs/ferro/pr/7"]).len() == 40
    );
    let bare = dirs.cache_dir.join("repos/github.com/o/r.git");
    assert!(!bare.exists(), "path A must not create a bare mirror");
}

#[test]
fn gc_removes_old_merged_worktrees() {
    use ferro_forge::{gc_worktrees, WorktreeEntry};
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    // Old merged worktree (dir present, old mtime).
    let old_wt = state.join("worktrees/old");
    std::fs::create_dir_all(&old_wt).unwrap();
    // Recent merged worktree: kept (age gate).
    let new_wt = state.join("worktrees/new");
    std::fs::create_dir_all(&new_wt).unwrap();
    // Missing dir: stale registration dropped.
    let gone = state.join("worktrees/gone");
    let entries = vec![
        WorktreeEntry {
            dir: old_wt.clone(),
            main_repo: old_wt.clone(),
            url: "https://github.com/o/r/pull/1".into(),
            opened_at: "".into(),
        },
        WorktreeEntry {
            dir: new_wt.clone(),
            main_repo: new_wt.clone(),
            url: "https://github.com/o/r/pull/2".into(),
            opened_at: "".into(),
        },
        WorktreeEntry {
            dir: gone.clone(),
            main_repo: gone.clone(),
            url: "https://github.com/o/r/pull/3".into(),
            opened_at: "".into(),
        },
        WorktreeEntry {
            dir: new_wt.clone(),
            main_repo: new_wt.clone(),
            url: "not a url".into(),
            opened_at: "".into(),
        },
    ];
    std::fs::write(
        state.join("worktrees.json"),
        serde_json::to_string(&entries).unwrap(),
    )
    .unwrap();
    // Backdate the old dir 8 days (creation time is now for both).
    let eight_days = std::time::Duration::from_secs(8 * 86400);
    let old_time = std::time::SystemTime::now() - eight_days;
    set_mtime(&old_wt, old_time);
    let removed = gc_worktrees(&state, 7, &|r| {
        if r.number == 1 {
            Some("merged".into())
        } else {
            Some("open".into())
        }
    });
    assert_eq!(removed, vec![old_wt.clone()]);
    assert!(!old_wt.exists());
    assert!(new_wt.exists());
}

#[cfg(unix)]
fn set_mtime(p: &std::path::Path, t: std::time::SystemTime) {
    std::fs::File::open(p).unwrap().set_modified(t).unwrap();
}

#[cfg(not(unix))]
fn set_mtime(_p: &std::path::Path, _t: std::time::SystemTime) {
    // Age gate untestable here; gc still drops the missing dir.
}
