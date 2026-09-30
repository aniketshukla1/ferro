//! B4b MR open flow: mock GitLab + local git fixture with a
//! `merge-requests/9/head` ref, subgroup namespace, job → workspace swap.

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

const MR: &str = "/api/v4/projects/group%2Fsub%2Fproj/merge_requests/9";

type Log = Arc<std::sync::Mutex<Vec<String>>>;

/// Mock GitLab v4 as served: the MR JSON (with `head_pipeline`, without
/// line counts), diffs, commits, versions, the source project (`fork`),
/// user and membership; lists empty, everything else `{}`. Draft notes get
/// ids 100, 101, …; bulk publish fails (500). Logs `METHOD path`.
async fn mock_gitlab(base: &str, head: &str, fork: &str, log: Log) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (base, head, fork) = (base.to_string(), head.to_string(), fork.to_string());
    let ids = Arc::new(std::sync::atomic::AtomicU64::new(100));
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            let (base, head, fork) = (base.clone(), head.clone(), fork.clone());
            let (log, ids) = (log.clone(), ids.clone());
            tokio::spawn(async move {
                let (rh, mut wh) = sock.into_split();
                let mut rd = tokio::io::BufReader::new(rh);
                loop {
                    let mut first = String::new();
                    if rd.read_line(&mut first).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let mut len = 0usize;
                    loop {
                        let mut line = String::new();
                        if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        if line.to_lowercase().starts_with("content-length:") {
                            len = line
                                .split(':')
                                .nth(1)
                                .unwrap_or("0")
                                .trim()
                                .parse()
                                .unwrap_or(0);
                        }
                    }
                    if len > 0 {
                        let mut b = vec![0u8; len];
                        if rd.read_exact(&mut b).await.is_err() {
                            return;
                        }
                    }
                    let method = first.split_whitespace().next().unwrap_or("");
                    let target = first.split_whitespace().nth(1).unwrap_or("");
                    let path = target.split('?').next().unwrap_or("");
                    log.lock().unwrap().push(format!("{method} {path}"));
                    let rest = path.strip_prefix(MR);
                    let (status, body) = match (method, rest) {
                        ("POST", Some("/draft_notes")) => (
                            201,
                            serde_json::json!({"id": ids.fetch_add(1, std::sync::atomic::Ordering::SeqCst)}),
                        ),
                        ("POST", Some("/draft_notes/bulk_publish")) => (500, serde_json::json!({})),
                        ("DELETE", Some(r)) if r.starts_with("/draft_notes/") => {
                            (204, serde_json::Value::Null)
                        }
                        _ => (200, get_body(&base, &head, &fork, path)),
                    };
                    let bytes = if status == 204 {
                        Vec::new()
                    } else {
                        serde_json::to_vec(&body).unwrap()
                    };
                    let head = format!(
                        "HTTP/1.1 {status} x\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                        bytes.len()
                    );
                    if wh.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    if wh.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    format!("http://{addr}/api/v4")
}

fn get_body(base: &str, head: &str, fork: &str, path: &str) -> serde_json::Value {
    match path.strip_prefix(MR) {
        Some("") => serde_json::json!({
            "title": "GL feat", "description": "mr body", "state": "opened",
            "draft": false, "source_branch": "feat", "target_branch": "main",
            "sha": head,
            "diff_refs": {"base_sha": base, "head_sha": head, "start_sha": base},
            "source_project_id": 2, "target_project_id": 1,
            "author": {"username": "ann", "avatar_url": null},
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
            "web_url": "https://git.example.com/group/sub/proj/-/merge_requests/9",
            "changes_count": "1",
            "head_pipeline": {"status": "success", "sha": head, "web_url": "https://ci/p/1"},
        }),
        Some("/diffs") => serde_json::json!([{"old_path": "feat.txt",
                            "new_path": "feat.txt", "diff": "@@ -0,0 +1 @@\n+feat\n"}]),
        Some("/commits") => serde_json::json!([{"id": head}]),
        Some("/versions") => serde_json::json!([{"id": 1,
                            "head_commit_sha": head, "base_commit_sha": base, "start_commit_sha": base}]),
        Some("/approvals") => serde_json::json!({"approved": false,
                            "user_has_approved": false, "user_can_approve": true}),
        Some("/discussions") => serde_json::json!([]),
        _ if path == "/api/v4/projects/2" => {
            serde_json::json!({"http_url_to_repo": fork, "web_url": "https://x"})
        }
        _ if path.contains("/members/") => serde_json::json!({"access_level": 40}),
        _ if path == "/api/v4/user" => serde_json::json!({"id": 7, "username": "me"}),
        _ => serde_json::json!({}),
    }
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

async fn j(router: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = router.oneshot(authed(uri, "GET", None)).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn post(router: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = router
        .oneshot(authed(uri, "POST", Some("{}")))
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn mr_open_job_swaps_workspace() {
    // The env token belongs to GITLAB_HOST only (never to whatever host an
    // MR link names).
    for v in ["GL_HOST", "GITLAB_URI", "GITLAB_ACCESS_TOKEN"] {
        std::env::remove_var(v);
    }
    std::env::set_var("GITLAB_HOST", "git.example.com");
    std::env::set_var("GITLAB_TOKEN", "glpat-test");
    // Origin with main + feature exposed as merge-requests/9/head.
    let og = tempfile::tempdir().unwrap();
    git(og.path(), &["init", "-b", "main", "."]);
    cfg(og.path());
    std::fs::write(og.path().join("base.txt"), "base\n").unwrap();
    git(og.path(), &["add", "."]);
    git(og.path(), &["commit", "-m", "base"]);
    let base = git_out(og.path(), &["rev-parse", "HEAD"]);
    git(og.path(), &["checkout", "-qb", "feature"]);
    std::fs::write(og.path().join("feat.txt"), "feat\n").unwrap();
    git(og.path(), &["add", "."]);
    git(og.path(), &["commit", "-m", "feat"]);
    let head = git_out(og.path(), &["rev-parse", "HEAD"]);
    git(
        og.path(),
        &["update-ref", "refs/merge-requests/9/head", &head],
    );
    git(og.path(), &["checkout", "-q", "main"]);

    // Nested matching origin for path A (subgroup included).
    let nest_base = tempfile::tempdir().unwrap();
    let nested = nest_base.path().join("git.example.com/group/sub/proj");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    assert!(std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(og.path())
        .arg(&nested)
        .status()
        .unwrap()
        .success());
    cfg(&nested);
    git(
        &nested,
        &["update-ref", "refs/merge-requests/9/head", &head],
    );
    let work = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(&nested)
        .arg(work.path())
        .status()
        .unwrap()
        .success());

    // The MR's source project: a fork, the push target.
    let fork_base = tempfile::tempdir().unwrap();
    let fork = fork_base.path().join("fork.git");
    assert!(std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg("--bare")
        .arg(og.path())
        .arg(&fork)
        .status()
        .unwrap()
        .success());

    let log: Log = Default::default();
    let mock = mock_gitlab(&base, &head, &fork.to_string_lossy(), log.clone()).await;
    std::env::set_var("FERRO_FORGE_API_BASE", &mock);

    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        work.path().to_path_buf(),
        dirs.clone(),
        ferro_server::Host::Cli,
        "test".into(),
    );
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        false,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let app = server::build_router(st, guard, last, true, None);

    let res = app
        .clone()
        .oneshot(authed(
            "/api/v1/workspace/open",
            "POST",
            Some(r#"{"prUrl":"https://git.example.com/group/sub/proj/-/merge_requests/9"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let job_id = v["job"]["id"].as_str().unwrap().to_string();

    let mut result = serde_json::Value::Null;
    for _ in 0..100 {
        let (s, j) = j(app.clone(), &format!("/api/v1/jobs/{job_id}")).await;
        assert_eq!(s, StatusCode::OK);
        let state = j["state"].as_str().unwrap_or("");
        if state == "done" {
            result = j["result"].clone();
            break;
        }
        assert_ne!(state, "failed", "{j}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert_eq!(result["provider"], "gitlab");
    assert_eq!(result["title"], "GL feat");
    assert_eq!(result["headSha"], head);
    assert_eq!(result["mergeBaseSha"], base);
    // CI from the head pipeline; size from /diffs + /commits.
    assert_eq!(result["checks"]["state"], "success");
    assert_eq!(
        result["stats"],
        serde_json::json!({"files": 1, "additions": 1, "deletions": 0, "commits": 1})
    );
    assert_eq!(result["auth"]["source"], "env");

    let (s, m) = j(app.clone(), "/api/v1/meta").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m["mode"], "pr");
    assert_eq!(m["workspace"]["name"], "group/sub/proj#9");
    assert!(m["features"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pr.gitlab")));

    let (s, p) = j(app.clone(), "/api/v1/pr").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(p["pr"]["headRef"], "feat");
    let (s, f) = j(app.clone(), "/api/v1/file?path=feat.txt").await;
    assert_eq!(s, StatusCode::OK, "{f}");

    // A refresh (like every poll) rereads the metadata; the push must
    // still land in the fork, not the target project.
    let (s, v) = post(app.clone(), "/api/v1/pr/refresh").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let r = ferro_forge::parse_mr_url("https://git.example.com/group/sub/proj/-/merge_requests/9")
        .unwrap();
    let wt = ferro_forge::checkout::worktree_dir(&dirs.state_dir, &r);
    std::fs::write(wt.join("feat.txt"), "feat\nmore\n").unwrap();
    git(&wt, &["add", "."]);
    git(
        &wt,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "more",
        ],
    );
    let (s, v) = post(app.clone(), "/api/v1/git/push").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        git_out(&fork, &["rev-parse", "refs/heads/feat"]),
        git_out(&wt, &["rev-parse", "HEAD"])
    );

    // A submit that fails at publish (mock: 500) leaves nothing staged on
    // GitLab — the reply draft and the inline note are deleted — and keeps
    // both drafts local, so a retry posts each exactly once.
    for body in [
        r#"{"path":"feat.txt","line":1,"side":"RIGHT","body":"inline"}"#,
        r#"{"path":"feat.txt","line":1,"side":"RIGHT","body":"reply","threadId":"d1"}"#,
    ] {
        let res = app
            .clone()
            .oneshot(authed("/api/v1/review/drafts", "POST", Some(body)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
    let (s, v) = post(app.clone(), "/api/v1/review/submit").await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    let log = log.lock().unwrap().clone();
    let staged: Vec<_> = log
        .iter()
        .filter(|l| *l == &format!("POST {MR}/draft_notes"))
        .collect();
    assert_eq!(staged.len(), 2, "{log:?}");
    for id in [100, 101] {
        assert!(
            log.contains(&format!("DELETE {MR}/draft_notes/{id}")),
            "{id}: {log:?}"
        );
    }
    let (s, d) = j(app.clone(), "/api/v1/review/drafts").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(d["drafts"].as_array().unwrap().len(), 2, "{d}");

    for v in ["FERRO_FORGE_API_BASE", "GITLAB_HOST", "GITLAB_TOKEN"] {
        std::env::remove_var(v);
    }
}
