//! B3 review regressions: the git runner, path handling, status parsing,
//! and the watcher's agreement with the index walk.

use ferro_core::git::{GitError, GitRepo};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn sh(dir: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "{args:?}");
}

fn init(dir: &Path) {
    sh(dir, &["init", "-q", "-b", "main"]);
    sh(dir, &["config", "user.email", "t@t"]);
    sh(dir, &["config", "user.name", "t"]);
    sh(dir, &["config", "commit.gpgsign", "false"]);
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "bee\n").unwrap();
    sh(dir.path(), &["add", "."]);
    sh(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn read(dir: &Path, p: &str) -> String {
    std::fs::read_to_string(dir.join(p)).unwrap()
}

#[test]
fn option_shaped_revs_are_refused() {
    let dir = repo();
    let g = GitRepo::new(dir.path().to_path_buf());
    std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
    let victim = dir.path().join("victim.out");
    let flag = format!("--output={}", victim.display());
    assert!(matches!(
        g.changes(&flag, "worktree"),
        Err(GitError::BadRev(_))
    ));
    assert!(matches!(g.changes("HEAD", &flag), Err(GitError::BadRev(_))));
    assert!(matches!(
        g.diff_raw("a.txt", &flag, "worktree", 3, false),
        Err(GitError::BadRev(_))
    ));
    assert!(matches!(
        g.diff_raw("a.txt", "HEAD", &flag, 3, false),
        Err(GitError::BadRev(_))
    ));
    assert!(matches!(
        g.resolve_base(&format!("merge-base:{flag}")),
        Err(GitError::BadRev(_))
    ));
    assert!(matches!(
        g.blob_bytes(&flag, "a.txt"),
        Err(GitError::BadRev(_))
    ));
    assert!(!victim.exists());
    // Unknown revs are client errors too, not git failures.
    assert!(matches!(
        g.rev_parse("no-such-branch"),
        Err(GitError::BadRev(_))
    ));
    // Ordinary rev syntax is untouched.
    assert!(g.rev_parse("HEAD~0").is_ok());
    assert!(g.changes("HEAD", "HEAD").is_ok());
}

#[test]
fn mutation_paths_are_literal() {
    let dir = repo();
    let g = GitRepo::new(dir.path().to_path_buf());
    std::fs::write(dir.path().join("a.txt"), "edit a\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "edit b\n").unwrap();
    for spec in ["*.txt", ":/", ":(top)a.txt", "[ab].txt"] {
        assert!(g.discard_paths(&[spec.to_string()]).is_err(), "{spec}");
    }
    assert_eq!(
        (read(dir.path(), "a.txt"), read(dir.path(), "b.txt")),
        ("edit a\n".into(), "edit b\n".into())
    );
    assert!(g.stage_paths(&["*.txt".to_string()]).is_err());
    assert_eq!(g.status_v2().unwrap().counts.staged, 0);
    // A directory path still covers what is below it.
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/x.rs"), "x\n").unwrap();
    std::fs::write(dir.path().join("src/y.rs"), "y\n").unwrap();
    g.stage_paths(&["src".to_string()]).unwrap();
    assert_eq!(g.status_v2().unwrap().counts.staged, 2);
}

#[cfg(unix)]
#[test]
fn symlinks_are_staged_and_discarded_as_links() {
    let dir = repo();
    let g = GitRepo::new(dir.path().to_path_buf());
    std::fs::write(dir.path().join("a.txt"), "precious edit\n").unwrap();
    std::os::unix::fs::symlink("a.txt", dir.path().join("link.txt")).unwrap();
    // Staging the link stages the link, not its modified target.
    g.stage_paths(&["link.txt".to_string()]).unwrap();
    let st = g.status_v2().unwrap();
    let staged: Vec<&str> = st
        .files
        .iter()
        .filter(|f| f.index.is_some())
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(staged, vec!["link.txt"]);
    g.unstage_paths(&["link.txt".to_string()]).unwrap();
    // Discarding the untracked link deletes it and leaves the target alone.
    g.discard_paths(&["link.txt".to_string()]).unwrap();
    assert!(std::fs::symlink_metadata(dir.path().join("link.txt")).is_err());
    assert_eq!(read(dir.path(), "a.txt"), "precious edit\n");
    // A link that points outside the repository is still a link git can
    // stage and diff; its target's content is never read.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "top secret\n").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("out")).unwrap();
    g.stage_paths(&["out".to_string()]).unwrap();
    let d = g.diff_raw("out", "HEAD", "index", 3, false).unwrap();
    let text: Vec<&str> = d
        .hunks
        .iter()
        .flat_map(|h| &h.rows)
        .map(|r| r.text.as_str())
        .collect();
    assert!(text.iter().all(|t| !t.contains("top secret")));
    let side = g
        .side_bytes("out", &ferro_core::diff::DiffSide::Worktree, 1 << 20)
        .unwrap();
    assert_eq!(
        String::from_utf8(side).unwrap(),
        outside.path().join("secret").display().to_string()
    );
}

#[test]
fn discard_of_an_untracked_directory() {
    let dir = repo();
    let g = GitRepo::new(dir.path().to_path_buf());
    std::fs::create_dir_all(dir.path().join("scratch/deep")).unwrap();
    std::fs::write(dir.path().join("scratch/a.tmp"), "a\n").unwrap();
    std::fs::write(dir.path().join("scratch/deep/b.tmp"), "b\n").unwrap();
    g.discard_paths(&["scratch".to_string()]).unwrap();
    assert!(!dir.path().join("scratch").exists());
    // A nested repository is never deleted wholesale.
    let nested = dir.path().join("vendor/lib");
    std::fs::create_dir_all(&nested).unwrap();
    init(&nested);
    std::fs::write(nested.join("x"), "x\n").unwrap();
    assert!(matches!(
        g.discard_paths(&["vendor".to_string()]),
        Err(GitError::Forbidden(_))
    ));
    assert!(nested.join(".git").exists());
}

#[test]
fn ahead_and_behind_are_parsed() {
    let up = repo();
    let dir = tempfile::tempdir().unwrap();
    let clone = dir.path().join("c");
    let st = std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(up.path())
        .arg(&clone)
        .status()
        .unwrap();
    assert!(st.success());
    sh(&clone, &["config", "user.email", "t@t"]);
    sh(&clone, &["config", "user.name", "t"]);
    sh(&clone, &["config", "commit.gpgsign", "false"]);
    std::fs::write(clone.join("c.txt"), "c\n").unwrap();
    sh(&clone, &["add", "."]);
    sh(&clone, &["commit", "-q", "-m", "local"]);
    std::fs::write(up.path().join("u.txt"), "u\n").unwrap();
    sh(up.path(), &["add", "."]);
    sh(up.path(), &["commit", "-q", "-m", "upstream"]);
    sh(&clone, &["fetch", "-q"]);
    let st = GitRepo::new(clone).status_v2().unwrap();
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!((st.ahead, st.behind), (1, 1));
}

#[test]
fn fresh_repository_before_the_first_commit() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(dir.path().join("first.txt"), "hello\nworld").unwrap();
    let g = GitRepo::new(dir.path().to_path_buf());
    assert!(g.is_unborn());
    let cs = g.changes("HEAD", "worktree").unwrap();
    assert_eq!(cs.files.len(), 1);
    // A last line without a newline still counts, as in `--numstat`.
    assert_eq!(cs.files[0].additions, 2);
    let d = g
        .diff_raw("first.txt", "HEAD", "worktree", 3, false)
        .unwrap();
    assert_eq!(d.new_path, "first.txt");
    g.stage_paths(&["first.txt".to_string()]).unwrap();
    g.unstage_paths(&["first.txt".to_string()]).unwrap();
    g.stage_paths(&["first.txt".to_string()]).unwrap();
    let d = g.diff_raw("first.txt", "HEAD", "index", 3, false).unwrap();
    assert_eq!(d.hunks.len(), 1);
    g.commit_msg("first", false).unwrap();
    assert!(!g.is_unborn());
}

#[test]
fn untracked_diff_uses_the_requested_path() {
    let dir = repo();
    let g = GitRepo::new(dir.path().to_path_buf());
    std::fs::write(dir.path().join("new.txt"), "x\ny\n").unwrap();
    let d = g.diff_raw("new.txt", "HEAD", "worktree", 3, false).unwrap();
    assert_eq!(d.new_path, "new.txt");
    assert!(d.old_path.is_none());
}

#[test]
fn diff_output_ignores_prefix_config() {
    let dir = repo();
    sh(dir.path(), &["config", "diff.noprefix", "true"]);
    sh(dir.path(), &["config", "diff.mnemonicPrefix", "true"]);
    let g = GitRepo::new(dir.path().to_path_buf());
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
    for target in ["worktree", "index"] {
        if target == "index" {
            g.stage_paths(&["a.txt".to_string()]).unwrap();
        }
        let d = g.diff_raw("a.txt", "HEAD", target, 3, false).unwrap();
        assert_eq!(d.new_path, "a.txt", "{target}");
    }
}

#[test]
fn blobs_are_capped_before_reading() {
    let dir = repo();
    let g = GitRepo::new(dir.path().to_path_buf());
    assert_eq!(g.blob_bytes("HEAD", "a.txt").unwrap(), b"one\n");
    assert!(matches!(
        g.blob_bytes_max("HEAD", "a.txt", 2),
        Err(GitError::TooLarge)
    ));
    // The batch reader resyncs after a refused body.
    assert_eq!(g.blob_bytes_max("HEAD", "b.txt", 64).unwrap(), b"bee\n");
    // Index blobs through the same reader.
    assert_eq!(g.blob_bytes_max("", "a.txt", 64).unwrap(), b"one\n");
    std::fs::write(dir.path().join("a.txt"), "x".repeat(4096)).unwrap();
    assert!(matches!(
        g.side_bytes("a.txt", &ferro_core::diff::DiffSide::Worktree, 1024),
        Err(GitError::TooLarge)
    ));
}

#[cfg(unix)]
#[test]
fn git_never_prompts_on_the_terminal() {
    use std::os::unix::fs::PermissionsExt;
    let dir = repo();
    let hook = dir.path().join(".git/hooks/pre-commit");
    let seen = dir.path().join("seen.txt");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf '%s %s' \"$GIT_TERMINAL_PROMPT\" \"$GIT_LITERAL_PATHSPECS\" > '{}'\n",
            seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let g = GitRepo::new(dir.path().to_path_buf());
    std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
    g.stage_paths(&["a.txt".to_string()]).unwrap();
    g.commit_msg("with hook", false).unwrap();
    assert_eq!(std::fs::read_to_string(seen).unwrap(), "0 1");
}

// -- watcher -----------------------------------------------------------------

fn collect(
    rx: &mpsc::Receiver<ferro_core::watch::WatchEvent>,
    for_: Duration,
) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + for_;
    let (mut ups, mut dels) = (Vec::new(), Vec::new());
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if let Ok(ferro_core::watch::WatchEvent::Fs {
            upserts, deletes, ..
        }) = rx.recv_timeout(left)
        {
            ups.extend(upserts.into_iter().map(|(p, _, _)| p));
            dels.extend(deletes);
        }
    }
    (ups, dels)
}

fn watched(
    dir: &Path,
) -> (
    ferro_core::watch::WatchHandle,
    mpsc::Receiver<ferro_core::watch::WatchEvent>,
) {
    let (tx, rx) = mpsc::channel();
    let h = ferro_core::watch::watch_root(dir, tx).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    while rx.try_recv().is_ok() {}
    (h, rx)
}

#[test]
fn watcher_skips_git_internals_and_ignored_trees() {
    let dir = repo();
    std::fs::write(dir.path().join(".gitignore"), "dist/\n").unwrap();
    std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
    std::fs::write(dir.path().join("pkg/.gitignore"), "gen/\n").unwrap();
    let (_h, rx) = watched(dir.path());
    std::fs::create_dir_all(dir.path().join("dist/js")).unwrap();
    std::fs::write(dir.path().join("dist/js/app.js"), "x\n").unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/gen")).unwrap();
    std::fs::write(dir.path().join("pkg/gen/out.rs"), "x\n").unwrap();
    std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    sh(dir.path(), &["commit", "-q", "-am", "second"]);
    let (ups, _) = collect(&rx, Duration::from_secs(3));
    assert!(ups.contains(&"a.txt".to_string()), "{ups:?}");
    assert!(!ups.iter().any(|p| p.starts_with(".git/")), "{ups:?}");
    assert!(!ups.iter().any(|p| p.starts_with("dist/")), "{ups:?}");
    assert!(!ups.iter().any(|p| p.starts_with("pkg/gen/")), "{ups:?}");
}

#[test]
fn watcher_reindexes_moved_directories() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
    std::fs::write(dir.path().join("src/deep/x.txt"), "x\n").unwrap();
    std::fs::write(dir.path().join("src/y.txt"), "y\n").unwrap();
    let (_h, rx) = watched(dir.path());
    std::fs::rename(dir.path().join("src"), dir.path().join("lib")).unwrap();
    let (ups, dels) = collect(&rx, Duration::from_secs(3));
    assert!(ups.contains(&"lib/deep/x.txt".to_string()), "{ups:?}");
    assert!(ups.contains(&"lib/y.txt".to_string()), "{ups:?}");
    // The old directory arrives as one delete; the server drops its subtree.
    assert!(dels.contains(&"src".to_string()), "{dels:?}");
    assert!(!dels.contains(&"lib".to_string()), "{dels:?}");
}

#[test]
fn watcher_flushes_under_a_steady_writer() {
    let dir = tempfile::tempdir().unwrap();
    let (_h, rx) = watched(dir.path());
    let root = dir.path().to_path_buf();
    let writer = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_millis(2500);
        let mut i = 0;
        while Instant::now() < end {
            std::fs::write(root.join("dev.log"), format!("{i}\n")).unwrap();
            i += 1;
            std::thread::sleep(Duration::from_millis(30));
        }
    });
    let t0 = Instant::now();
    std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
    let mut seen_at = None;
    while t0.elapsed() < Duration::from_secs(5) && seen_at.is_none() {
        if let Ok(ferro_core::watch::WatchEvent::Fs { upserts, .. }) =
            rx.recv_timeout(Duration::from_millis(200))
        {
            if upserts.iter().any(|(p, _, _)| p == "b.txt") {
                seen_at = Some(t0.elapsed());
            }
        }
    }
    writer.join().unwrap();
    let at = seen_at.expect("b.txt must be reported");
    // Well before the writer stops (2.5 s): the batch is capped at 500 ms.
    assert!(at < Duration::from_millis(1500), "flushed after {at:?}");
}
