//! B4 PR open flow: mock GitHub + local git fixture, job → workspace swap.

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

/// Minimal mock GitHub: canned PR repo answers keyed by request path.
async fn mock_github(base: &str, head: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = base.to_string();
    let head = head.to_string();
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            let (base, head) = (base.clone(), head.clone());
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
                    let is_pull = first.contains("/pulls/7")
                        && !first.contains("/reviews")
                        && !first.contains("/comments");
                    let is_repo = first.contains("GET /repos/o/r ");
                    let body = if is_pull {
                        serde_json::json!({
                            "title": "Feat", "body": "hello", "state": "open", "merged": false, "draft": false,
                            "base": {"ref": "main", "sha": base},
                            "head": {"ref": "feature", "sha": head, "repo": {"clone_url": "https://github.com/o/r.git", "fork": false}},
                            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
                            "additions": 1, "deletions": 0, "changed_files": 1, "commits": 1,
                            "html_url": "https://github.com/o/r/pull/7",
                            "user": {"login": "ann"},
                        })
                    } else if is_repo {
                        serde_json::json!({"permissions": {"push": true}})
                    } else if first.contains("check-runs") {
                        serde_json::json!({"total_count": 1, "check_runs": [{"status": "completed", "conclusion": "success", "details_url": "https://ci/1"}]})
                    } else if first.contains("/status") {
                        serde_json::json!({"total_count": 0, "statuses": []})
                    } else {
                        serde_json::json!({})
                    };
                    let bytes = serde_json::to_vec(&body).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 x\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                        bytes.len()
                    );
                    if wh.write_all(resp.as_bytes()).await.is_err() {
                        return;
                    }
                    if wh.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    format!("http://{addr}")
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

#[tokio::test]
async fn pr_open_job_swaps_workspace() {
    std::env::remove_var("GITHUB_TOKEN");
    std::env::remove_var("GH_TOKEN");
    // Origin with main + feature exposed as refs/pull/7/head.
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
    git(og.path(), &["checkout", "-q", "main"]);

    // Nest under a matching path so path A (local remote) fires.
    let nest_base = tempfile::tempdir().unwrap();
    let nested = nest_base.path().join("github.com/o/r");
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
    git(&nested, &["update-ref", "refs/pull/7/head", &head]);
    let work = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(&nested)
        .arg(work.path())
        .status()
        .unwrap()
        .success());

    let mock = mock_github(&base, &head).await;
    std::env::set_var("FERRO_FORGE_API_BASE", &mock);

    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        work.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let app = server::build_router(st, guard, last, true, None);

    // Open through /workspace/open (also covers the prUrl routing).
    let res = app
        .clone()
        .oneshot(authed(
            "/api/v1/workspace/open",
            "POST",
            Some(r#"{"prUrl":"https://github.com/o/r/pull/7"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let job_id = v["job"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["job"]["kind"], "pr.open");

    // Poll the job to completion.
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
    assert_eq!(result["title"], "Feat");
    assert_eq!(result["headSha"], head);
    assert_eq!(result["mergeBaseSha"], base);
    assert!(result["auth"]["hasToken"].is_boolean());

    // Workspace switched; meta reports PR mode.
    let (s, m) = j(app.clone(), "/api/v1/meta").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m["mode"], "pr");
    assert_eq!(m["workspace"]["name"], "o/r#7");
    assert!(m["features"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pr.open")));

    // GET /pr agrees; the worktree serves the feature file.
    let (s, p) = j(app.clone(), "/api/v1/pr").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(p["pr"]["headRef"], "feature");
    let (s, f) = j(app.clone(), "/api/v1/file?path=feat.txt").await;
    assert_eq!(s, StatusCode::OK, "{f}");

    // Refresh round-trips.
    let res = app
        .clone()
        .oneshot(authed("/api/v1/pr/refresh", "POST", Some("")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    std::env::remove_var("FERRO_FORGE_API_BASE");
}

#[tokio::test]
async fn pr_routes_reject_outside_pr_mode() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        dir.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let app = server::build_router(st, guard, last, true, None);
    let (s, v) = j(app.clone(), "/api/v1/pr").await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["pr"].is_null());
    let res = app
        .clone()
        .oneshot(authed("/api/v1/pr/refresh", "POST", Some("")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // Bad URLs rejected without network.
    let res = app
        .oneshot(authed("/api/v1/pr/open", "POST", Some(r#"{"url":"nope"}"#)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
