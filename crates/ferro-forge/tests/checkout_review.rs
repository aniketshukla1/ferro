//! B4 review regressions: PR checkout safety (user repo untouched, hooks
//! never run, worktree kept on refresh), force-pushes, fork remotes, mirror
//! recovery, and review-store integrity.

use ferro_forge::store::{DraftPatch, NewDraft, ReviewStore};
use ferro_forge::{CheckoutOpts, ForgeRef, OpenedPr};
use std::path::{Path, PathBuf};

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

fn git_ok(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn clone(from: &Path, to: &Path, bare: bool) {
    let mut c = std::process::Command::new("git");
    c.args(["clone", "-q"]);
    if bare {
        c.arg("--bare");
    }
    assert!(c.arg(from).arg(to).status().unwrap().success());
}

fn cfg(dir: &Path) {
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Origin at `<tmp>/github.com/o/r` (so remote_matches fires for path A),
/// with main + feature exposed as `refs/pull/7/head`.
struct Origin {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    base: String,
    head: String,
}

impl Origin {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("github.com/o/r");
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main", "."]);
        cfg(&dir);
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "base"]);
        let base = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["checkout", "-qb", "feature"]);
        std::fs::write(dir.join("feat.txt"), "feat\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "feat"]);
        let head = git(&dir, &["rev-parse", "HEAD"]);
        git(&dir, &["update-ref", "refs/pull/7/head", &head]);
        git(&dir, &["checkout", "-q", "main"]);
        Self {
            _tmp: tmp,
            dir,
            base,
            head,
        }
    }

    fn url(&self) -> String {
        self.dir.to_string_lossy().into_owned()
    }

    /// Move the PR head: a new commit (`amend` rewrites instead).
    fn push(&mut self, file: &str, amend: bool) -> String {
        git(&self.dir, &["checkout", "-q", "feature"]);
        std::fs::write(self.dir.join(file), format!("{file}\n")).unwrap();
        git(&self.dir, &["add", "."]);
        if amend {
            git(&self.dir, &["commit", "-qm", "rewritten", "--amend"]);
        } else {
            git(&self.dir, &["commit", "-qm", file]);
        }
        let head = git(&self.dir, &["rev-parse", "HEAD"]);
        git(&self.dir, &["update-ref", "refs/pull/7/head", &head]);
        git(&self.dir, &["checkout", "-q", "main"]);
        self.head = head.clone();
        head
    }
}

fn dirs() -> (tempfile::TempDir, ferro_core::dirs::FerroDirs) {
    let home = tempfile::tempdir().unwrap();
    let d = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    (home, d)
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
    og: &Origin,
    local: Option<&Path>,
    d: &ferro_core::dirs::FerroDirs,
) -> Result<OpenedPr, ferro_forge::ForgeError> {
    ferro_forge::open_pr(
        &pref(),
        &meta(&og.base, &og.head),
        &og.url(),
        local,
        d,
        None,
        &|_| {},
        &CheckoutOpts {
            allow_file_protocol: true,
        },
    )
}

#[cfg(unix)]
#[test]
fn opening_a_pr_never_touches_the_users_repo_or_runs_its_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let og = Origin::new();
    // The PR ships a post-checkout hook in an in-tree hooks dir.
    git(&og.dir, &["checkout", "-q", "feature"]);
    let marker = og._tmp.path().join("pwned");
    std::fs::create_dir_all(og.dir.join(".githooks")).unwrap();
    let hook = og.dir.join(".githooks/post-checkout");
    std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&og.dir, &["add", "."]);
    git(&og.dir, &["commit", "-qm", "hooks"]);
    let head = git(&og.dir, &["rev-parse", "HEAD"]);
    git(&og.dir, &["update-ref", "refs/pull/7/head", &head]);
    git(&og.dir, &["checkout", "-q", "main"]);
    let og = Origin { head, ..og };
    // The user's clone uses an in-tree hooks dir (a common setup).
    let work = tempfile::tempdir().unwrap();
    clone(&og.dir, work.path(), false);
    git(work.path(), &["config", "core.hooksPath", ".githooks"]);
    let before = std::fs::read_to_string(work.path().join(".git/config")).unwrap();
    let (_h, d) = dirs();
    let o = open(&og, Some(work.path()), &d).unwrap();
    assert!(o.dir.join(".githooks/post-checkout").is_file());
    assert!(!marker.exists(), "the PR's post-checkout hook ran");
    let after = std::fs::read_to_string(work.path().join(".git/config")).unwrap();
    assert_eq!(before, after, "the user's repo config was modified");
    // Refresh checks out inside the worktree, where the in-tree hooks dir
    // resolves to the PR's own: still nothing may run.
    let mut og = og;
    og.push("more.txt", false);
    let o2 = open(&og, Some(&o.dir), &d).unwrap();
    assert!(o2.dir.join("more.txt").is_file());
    assert!(!marker.exists(), "the PR's hook ran on refresh");
}

#[test]
fn refresh_moves_the_worktree_in_place() {
    let mut og = Origin::new();
    let (_h, d) = dirs();
    let o1 = open(&og, None, &d).unwrap();
    std::fs::write(o1.dir.join("scratch.log"), "x\n").unwrap();
    std::fs::remove_file(o1.dir.join("scratch.log")).unwrap();
    og.push("more.txt", false);
    // What /pr/refresh does: local = the PR worktree itself.
    let o2 = open(&og, Some(&o1.dir), &d).unwrap();
    assert_eq!(o2.dir, o1.dir);
    assert!(!o2.reused);
    assert!(o1.dir.join("more.txt").is_file());
    assert_eq!(git(&o1.dir, &["rev-parse", "HEAD"]), og.head);
}

#[test]
fn force_pushed_prs_reopen() {
    let mut og = Origin::new();
    let (_h, d) = dirs();
    open(&og, None, &d).unwrap();
    og.push("feat.txt", true);
    let o = open(&og, None, &d).unwrap();
    assert_eq!(o.head_sha, og.head);
    assert_eq!(
        std::fs::read_to_string(o.dir.join("feat.txt")).unwrap(),
        "feat.txt\n"
    );
}

#[test]
fn fork_workflow_fetches_from_the_matching_remote() {
    let og = Origin::new();
    // origin = the user's fork (no pull refs); upstream = the PR's repo.
    let fork_tmp = tempfile::tempdir().unwrap();
    let fork = fork_tmp.path().join("github.com/me/r");
    std::fs::create_dir_all(fork.parent().unwrap()).unwrap();
    clone(&og.dir, &fork, true);
    let work = tempfile::tempdir().unwrap();
    clone(&fork, work.path(), false);
    git(work.path(), &["remote", "add", "upstream", &og.url()]);
    let (_h, d) = dirs();
    let o = open(&og, Some(work.path()), &d).unwrap();
    assert_eq!(o.remote, "upstream");
    assert!(o.dir.join("feat.txt").is_file());
}

#[test]
fn a_half_made_mirror_is_replaced() {
    let og = Origin::new();
    let (_h, d) = dirs();
    // An interrupted clone left a directory that is not a repository.
    let bare = ferro_forge::checkout::bare_dir(&d.cache_dir, &pref());
    std::fs::create_dir_all(bare.join("objects")).unwrap();
    std::fs::write(bare.join("HEAD.lock"), "").unwrap();
    let o = open(&og, None, &d).unwrap();
    assert!(o.dir.join("feat.txt").is_file());
    assert_eq!(
        git_ok(&bare, &["rev-parse", "--is-bare-repository"]).as_deref(),
        Some("true")
    );
}

fn new_draft(body: &str) -> NewDraft {
    NewDraft {
        path: "a.txt".into(),
        line: 1,
        start_line: None,
        side: None,
        body: body.into(),
        thread_id: None,
        source: None,
        finding_id: None,
    }
}

#[test]
fn concurrent_draft_writes_keep_every_draft() {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(ReviewStore::new(dir.path().join("r")));
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let s = store.clone();
            std::thread::spawn(move || {
                for i in 0..10 {
                    s.add(new_draft(&format!("t{t}-{i}"))).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(store.drafts().len(), 80);
}

#[test]
fn patch_keeps_ranges_valid() {
    let dir = tempfile::tempdir().unwrap();
    let store = ReviewStore::new(dir.path().join("r"));
    let mut nd = new_draft("x");
    nd.line = 5;
    nd.start_line = Some(3);
    let d = store.add(nd).unwrap();
    // Moving `line` above startLine alone must be refused.
    let p = DraftPatch {
        body: None,
        line: Some(2),
        start_line: None,
        side: None,
    };
    assert!(store.patch(&d.id, &p).is_err());
    assert_eq!(store.drafts()[0].line, 5);
}

#[test]
fn remap_marks_drafts_stale_when_the_diff_fails() {
    let dir = tempfile::tempdir().unwrap();
    let store = ReviewStore::new(dir.path().join("r"));
    store.add(new_draft("x")).unwrap();
    let og = Origin::new();
    let repo = ferro_core::git::GitRepo::new(og.dir.clone());
    // An old head that no longer exists: the mapping is unknown.
    let stale = store.remap(&repo, &"0".repeat(40), &og.head).unwrap();
    assert_eq!(stale, 1);
    assert!(store.drafts()[0].stale);
}
