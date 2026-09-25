//! B4 PR-mode push/pull over local fixtures (fork-aware, never forced).

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn git(dir: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "{args:?}");
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn cfg(dir: &Path) {
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn authed(uri: &str, method: &str, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(Body::from(body.unwrap_or("").to_string())).unwrap()
}

async fn req(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let res = router.oneshot(authed(uri, method, body)).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, v)
}

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    fork: std::path::PathBuf,
    origin: std::path::PathBuf,
}

/// Work repo = PR worktree at `head`; fork = bare "head repo"; origin has
/// `refs/pull/7/head`. Session token present (push allowed).
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let og = tmp.path().join("origin");
    std::fs::create_dir_all(&og).unwrap();
    git(&og, &["init", "-b", "main", "."]);
    cfg(&og);
    std::fs::write(og.join("base.txt"), "base\n").unwrap();
    git(&og, &["add", "."]);
    git(&og, &["commit", "-m", "base"]);
    git(&og, &["checkout", "-qb", "feature"]);
    std::fs::write(og.join("feat.txt"), "feat\n").unwrap();
    git(&og, &["add", "."]);
    git(&og, &["commit", "-m", "feat"]);
    let head = git_out(&og, &["rev-parse", "HEAD"]);
    git(&og, &["update-ref", "refs/pull/7/head", &head]);
    git(&og, &["checkout", "-q", "main"]);

    // Fork = bare clone holding the feature branch (push target).
    let fork = tmp.path().join("fork.git");
    assert!(std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg("--bare")
        .arg(&og)
        .arg(&fork)
        .status()
        .unwrap()
        .success());

    // Work repo: clone + checkout feature (like an attached worktree).
    let work = tmp.path().join("work");
    assert!(std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(&og)
        .arg(&work)
        .status()
        .unwrap()
        .success());
    cfg(&work);
    git(&work, &["checkout", "-q", "feature"]);
    // Rename origin → base (worktree remotes point at base), add fork remote?
    // pull_pr fetches `origin pull/7/head` — keep origin, ensure the ref.
    git(
        &work,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/pull/7/head:refs/pull/7/head",
        ],
    );

    let home = tmp.path().join("home");
    let dirs = ferro_core::dirs::FerroDirs::new(home.join("c"), home.join("s"), home.join("h"));
    let st = server::build_state(
        work.clone(),
        dirs.clone(),
        ferro_server::Host::Cli,
        "test".into(),
    );
    let pref = ferro_forge::ForgeRef {
        host: "github.com".into(),
        owner: "o".into(),
        repo: "r".into(),
        number: 7,
    };
    let store = ferro_forge::store::ReviewStore::new(home.join("reviews"));
    let meta = ferro_forge::github::PullMeta {
        title: "T".into(),
        body: None,
        state: "open".into(),
        merged: false,
        draft: false,
        base_ref: "main".into(),
        head_ref: "feature".into(),
        base_sha: head.clone(),
        head_sha: head.clone(),
        head_clone_url: Some(fork.to_string_lossy().to_string()),
        is_fork: true,
        created_at: "".into(),
        updated_at: "".into(),
        additions: 1,
        deletions: 0,
        changed_files: 1,
        commits: 1,
        html_url: "".into(),
        author_login: "a".into(),
        author_avatar: None,
    };
    let opened = ferro_forge::OpenedPr {
        dir: work.clone(),
        base_ref: "main".into(),
        base_sha: head.clone(),
        head_sha: head.clone(),
        merge_base: head.clone(),
        reused: false,
    };
    let session = Arc::new(ferro_server::state::PrSession {
        pr_ref: pref,
        meta: parking_lot::RwLock::new(meta),
        github: Arc::new(ferro_forge::GitHub::new(
            "http://127.0.0.1:9/unused".into(),
            "http://127.0.0.1:9/unused".into(),
            Some("tok".into()),
        )),
        token: Some("tok".into()),
        token_source: None,
        worktree: parking_lot::RwLock::new(opened),
        store,
        checks: parking_lot::RwLock::new(None),
        can_push: parking_lot::RwLock::new(None),
        can_push_known: std::sync::atomic::AtomicBool::new(false),
    });
    let ws = ferro_server::state::Workspace::pr(work, session, &dirs);
    st.ws.store(ws);
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let app = server::build_router(st, guard, last, true, None);
    Fixture {
        _tmp: tmp,
        app,
        fork,
        origin: og,
    }
}

#[tokio::test]
async fn pr_push_to_fork_and_records() {
    let f = fixture();
    // canPushHead unknown before any attempt.
    let (s, v) = req(f.app.clone(), "GET", "/api/v1/pr", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["pr"]["auth"]["canPushHead"].is_null());
    // New commit on the worktree, then push.
    let work = f.fork.parent().unwrap().join("work");
    std::fs::write(work.join("feat.txt"), "feat\nmore\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "more"]);
    let (s, v) = req(f.app.clone(), "POST", "/api/v1/git/push", Some("{}")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Fork's feature branch advanced (fork-aware, not base).
    let tip = git_out(&f.fork, &["rev-parse", "feature"]);
    let wt = git_out(&work, &["rev-parse", "HEAD"]);
    assert_eq!(tip, wt);
    let (s, v) = req(f.app.clone(), "GET", "/api/v1/pr", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["pr"]["auth"]["canPushHead"], true);
}

#[tokio::test]
async fn pr_pull_fast_forward_and_conflicts() {
    let f = fixture();
    let work = f.fork.parent().unwrap().join("work");
    // Up-to-date: no-op pull.
    let (s, v) = req(f.app.clone(), "POST", "/api/v1/git/pull", Some("{}")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["updated"], false);
    // Advance the remote head: new commit on origin feature + pull ref.
    git(&f.origin, &["checkout", "-q", "feature"]);
    std::fs::write(f.origin.join("feat.txt"), "feat\nremote\n").unwrap();
    git(&f.origin, &["add", "."]);
    git(&f.origin, &["commit", "-qm", "remote"]);
    let new_head = git_out(&f.origin, &["rev-parse", "HEAD"]);
    git(&f.origin, &["update-ref", "refs/pull/7/head", &new_head]);
    git(&f.origin, &["checkout", "-q", "main"]);
    git(
        &work,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/pull/7/head:refs/pull/7/head",
        ],
    );
    let (s, v) = req(f.app.clone(), "POST", "/api/v1/git/pull", Some("{}")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["updated"], true);
    assert_eq!(git_out(&work, &["rev-parse", "HEAD"]), new_head);
    // Dirty tree with a pending update: 409.
    std::fs::write(work.join("feat.txt"), "dirty\n").unwrap();
    git(&f.origin, &["checkout", "-q", "feature"]);
    std::fs::write(f.origin.join("feat.txt"), "feat\nremote\nagain\n").unwrap();
    git(&f.origin, &["add", "."]);
    git(&f.origin, &["commit", "-qm", "remote2"]);
    let new_head2 = git_out(&f.origin, &["rev-parse", "HEAD"]);
    git(&f.origin, &["update-ref", "refs/pull/7/head", &new_head2]);
    git(&f.origin, &["checkout", "-q", "main"]);
    git(
        &work,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/pull/7/head:refs/pull/7/head",
        ],
    );
    let (s, _) = req(f.app.clone(), "POST", "/api/v1/git/pull", Some("{}")).await;
    assert_eq!(s, StatusCode::CONFLICT);
}
