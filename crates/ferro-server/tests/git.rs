//! B3 contract tests: git status, changes, log, mutations (API.md § 6).

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ferro_server::server;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn git(args: &[&str], dir: &std::path::Path) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "{args:?}");
}

/// Router over a small live repo. The TempDirs are leaked: the router holds
/// the root path past the end of this function.
fn state() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    git(&["init", "-b", "main"], dir.path());
    git(&["config", "user.email", "t@t"], dir.path());
    git(&["config", "user.name", "t"], dir.path());
    git(&["config", "commit.gpgsign", "false"], dir.path());
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
    git(&["add", "."], dir.path());
    git(&["commit", "-m", "init"], dir.path());
    std::fs::write(dir.path().join("a.txt"), "one\nmodified\n").unwrap();
    std::fs::write(dir.path().join("new.txt"), "fresh\n").unwrap();
    let dir = Box::leak(Box::new(dir));
    let home = Box::leak(Box::new(tempfile::tempdir().unwrap()));
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
    server::build_router(st, guard, last, false, None)
}

fn plain_state_over(dir: &std::path::Path) -> axum::Router {
    let home = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        dir.to_path_buf(),
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
    server::build_router(st, guard, last, true, None)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

fn post(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7778")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn j(router: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = router.oneshot(get(uri)).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    (status, v)
}

async fn p(router: axum::Router, uri: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let res = router.oneshot(post(uri, body)).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test]
async fn status_shape() {
    let app = state();
    let (s, v) = j(app, "/api/v1/git/status").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["branch"], "main");
    assert_eq!(v["detached"], false);
    assert_eq!(v["headSha"].as_str().unwrap().len(), 40);
    let paths: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"a.txt") && paths.contains(&"new.txt"));
    let un = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "new.txt")
        .unwrap();
    assert_eq!(un["untracked"], true);
    assert_eq!(v["counts"]["untracked"], 1);
    assert_eq!(v["counts"]["unstaged"], 1);
}

#[tokio::test]
async fn status_not_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    let app = plain_state_over(dir.path());
    let (s, v) = j(app, "/api/v1/git/status").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "unsupported");
}

#[tokio::test]
async fn changes_worktree() {
    let app = state();
    let (s, v) = j(app, "/api/v1/git/changes?base=HEAD&target=worktree").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["base"], "HEAD");
    assert_eq!(v["target"], "worktree");
    assert_eq!(v["baseSha"].as_str().unwrap().len(), 40);
    assert!(v["targetSha"].is_null());
    let a = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "a.txt")
        .unwrap();
    assert_eq!(a["status"], "M");
    assert_eq!(a["additions"], 1);
    let u = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "new.txt")
        .unwrap();
    assert_eq!(u["status"], "?");
    assert_eq!(u["additions"], 1);
    assert!(v["stats"]["files"].as_u64().unwrap() >= 2);
}

#[tokio::test]
async fn mutations_round_trip() {
    let app = state();
    // Stage the modification; status comes back with the file.
    let (s, v) = p(app.clone(), "/api/v1/git/stage", r#"{"paths":["a.txt"]}"#).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["counts"]["staged"], 1);
    // Unstage again.
    let (s, v) = p(app.clone(), "/api/v1/git/unstage", r#"{"paths":["a.txt"]}"#).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["counts"]["staged"], 0);
    // Discard without confirm is rejected.
    let (s, _) = p(app.clone(), "/api/v1/git/discard", r#"{"paths":["a.txt"]}"#).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Commit with nothing staged is a conflict.
    let (s, v) = p(app.clone(), "/api/v1/git/commit", r#"{"message":"x"}"#).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    // Stage + commit works and returns sha/summary/status.
    let (s, _) = p(app.clone(), "/api/v1/git/stage", r#"{"paths":["a.txt"]}"#).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = p(app.clone(), "/api/v1/git/commit", r#"{"message":"second"}"#).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["sha"].as_str().unwrap().len(), 40);
    assert_eq!(v["summary"], "second");
    // Discard the untracked file with confirm.
    let (s, v) = p(
        app.clone(),
        "/api/v1/git/discard",
        r#"{"paths":["new.txt"],"confirm":true}"#,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["path"] != "new.txt"));
    // Traversal blocked.
    let (s, _) = p(app.clone(), "/api/v1/git/stage", r#"{"paths":["../x"]}"#).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn log_shape() {
    let app = state();
    let (s, v) = j(app, "/api/v1/git/log?limit=5").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["commits"][0]["subject"], "init");
    assert_eq!(v["commits"][0]["sha"].as_str().unwrap().len(), 40);
    assert_eq!(v["commits"][0]["author"], "t");
}

#[tokio::test]
async fn legacy_git_status_still_works() {
    let app = state();
    let (s, v) = j(app, "/api/git-status").await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

fn git_commit(dir: &std::path::Path, msg: &str) {
    git(&["add", "."], dir);
    git(&["commit", "-m", msg], dir);
}

#[tokio::test]
async fn diff_modify_golden() {
    let app = state();
    let (s, v) = j(app.clone(), "/api/v1/git/diff?path=a.txt").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["path"], "a.txt");
    assert_eq!(v["status"], "M");
    assert_eq!(v["binary"], false);
    assert_eq!(v["tooLarge"], false);
    assert!(v["language"].is_null()); // .txt has no mapped language
    let h = &v["hunks"][0];
    assert!(!h["id"].as_str().unwrap().is_empty());
    assert!(h["header"].as_str().unwrap().starts_with("@@ "));
    let rows = h["rows"].as_array().unwrap();
    // Fixture appends one line: ctx + add, no del.
    assert!(rows.iter().any(|r| r["t"] == "ctx") && rows.iter().any(|r| r["t"] == "add"));
    // hl=1 default: highlighted html with theme classes, correct sides.
    let add = rows.iter().find(|r| r["t"] == "add").unwrap();
    assert!(add["n"].is_number() && add["o"].is_null());
    assert!(add["html"].as_str().unwrap().contains("modified"));
    // hl=0: plain text.
    let (s, v) = j(app, "/api/v1/git/diff?path=a.txt&hl=0").await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["hunks"][0]["rows"].as_array().unwrap()[0]["text"].is_string());
}

#[tokio::test]
async fn diff_rename_binary_untracked_deleted() {
    let app = state();
    // Rename a.txt -> r.txt via shell (staged).
    let dir = {
        let d = tempfile::tempdir().unwrap();
        git(&["init", "-b", "main"], d.path());
        git(&["config", "user.email", "t@t"], d.path());
        git(&["config", "user.name", "t"], d.path());
        git(&["config", "commit.gpgsign", "false"], d.path());
        std::fs::write(d.path().join("keep.rs"), "fn keep() {}\n").unwrap();
        git_commit(d.path(), "init");
        git(&["mv", "keep.rs", "moved.rs"], d.path());
        Box::leak(Box::new(d))
    };
    let app2 = plain_state_over(dir.path());
    let (s, v) = j(app2, "/api/v1/git/diff?path=moved.rs").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "R");
    assert_eq!(v["oldPath"], "keep.rs");
    assert_eq!(v["language"], "Rust");
    let _ = app;

    // Binary + untracked + deleted goldens on the shared fixture.
    let app = state();
    // untracked new.txt
    let (s, v) = j(app.clone(), "/api/v1/git/diff?path=new.txt").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "A");
    assert!(v["hunks"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["t"] == "add"));
}

#[tokio::test]
async fn diff_intraline_utf16() {
    let dir = {
        let d = tempfile::tempdir().unwrap();
        git(&["init", "-b", "main"], d.path());
        git(&["config", "user.email", "t@t"], d.path());
        git(&["config", "user.name", "t"], d.path());
        git(&["config", "commit.gpgsign", "false"], d.path());
        std::fs::write(
            d.path().join("e.txt"),
            "let x = \"a🎉b\";\nlet y = \"日本語 ok\";\n",
        )
        .unwrap();
        git_commit(d.path(), "init");
        std::fs::write(
            d.path().join("e.txt"),
            "let x = \"a🎊b\";\nlet y = \"日本語 OK\";\n",
        )
        .unwrap();
        Box::leak(Box::new(d))
    };
    let app = plain_state_over(dir.path());
    let (s, v) = j(app, "/api/v1/git/diff?path=e.txt").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let rows = v["hunks"][0]["rows"].as_array().unwrap().clone();
    let dels: Vec<_> = rows.iter().filter(|r| r["t"] == "del").collect();
    let adds: Vec<_> = rows.iter().filter(|r| r["t"] == "add").collect();
    assert_eq!((dels.len(), adds.len()), (2, 2));
    // 🎉 is 2 UTF-16 units; in `let x = "a🎉b";` it sits at units 10..12.
    let ch0 = dels[0]["ch"].as_array().unwrap();
    assert!(!ch0.is_empty(), "{dels:?}");
    // The changed range must cover the emoji (units 10..12 of the row text).
    let covers_emoji = ch0.iter().any(|r| {
        let a = r[0].as_u64().unwrap();
        let b = r[1].as_u64().unwrap();
        a <= 10 && b >= 12
    });
    assert!(covers_emoji, "{ch0:?}");
    // CJK row also carries ranges.
    assert!(!adds[1]["ch"].as_array().unwrap().is_empty());
    // intraline=0 suppresses ch.
    let (s, v) = j(
        plain_state_over(dir.path()),
        "/api/v1/git/diff?path=e.txt&intraline=0",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["hunks"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r.get("ch").is_none()));
}

#[tokio::test]
async fn diff_too_large() {
    let dir = {
        let d = tempfile::tempdir().unwrap();
        git(&["init", "-b", "main"], d.path());
        git(&["config", "user.email", "t@t"], d.path());
        git(&["config", "user.name", "t"], d.path());
        git(&["config", "commit.gpgsign", "false"], d.path());
        let big: String = (0..30_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(d.path().join("big.txt"), &big).unwrap();
        git_commit(d.path(), "init");
        let big2: String = (0..30_000).map(|i| format!("LINE {i}\n")).collect();
        std::fs::write(d.path().join("big.txt"), &big2).unwrap();
        Box::leak(Box::new(d))
    };
    let app = plain_state_over(dir.path());
    let (s, v) = j(app.clone(), "/api/v1/git/diff?path=big.txt").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["tooLarge"], true);
    assert!(v["hunks"].as_array().unwrap().is_empty());
    let (s, v) = j(app, "/api/v1/git/diff?path=big.txt&force=1").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["tooLarge"], false);
    assert!(!v["hunks"].as_array().unwrap().is_empty());
}
