//! B7 harness dispatch, snapshot/revert, and /git/hunk (API.md § 10.5).

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use ferro_server::server;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "01234567890123456789012345678901";

fn git(args: &[&str], dir: &Path) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "{args:?}");
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

struct Fixture {
    _repo: tempfile::TempDir,
    _home: tempfile::TempDir,
    _bin: tempfile::TempDir,
    app: axum::Router,
}

fn fixture(script: &str, extra_env: Vec<(String, String)>, timeout: Duration) -> Fixture {
    let repo = tempfile::tempdir().unwrap();
    git(&["init", "-b", "main"], repo.path());
    git(&["config", "user.email", "t@t"], repo.path());
    git(&["config", "user.name", "t"], repo.path());
    git(&["config", "commit.gpgsign", "false"], repo.path());
    git(&["config", "core.autocrlf", "false"], repo.path());
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    write_exec(&bin.path().join("claude"), script);
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        repo.path().to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    {
        let mut host = st.harness_host.lock();
        host.path_env = Some(bin.path().to_string_lossy().into_owned());
        host.home = Some(home.path().to_path_buf());
        host.scan_system_dirs = false;
        host.timeout = timeout;
        host.extra_env = extra_env;
    }
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
    Fixture {
        _repo: repo,
        _home: home,
        _bin: bin,
        app,
    }
}

fn req(method: Method, uri: &str, body: Option<&str>) -> Request<Body> {
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

async fn call(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let res = app.clone().oneshot(req(method, uri, body)).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

async fn wait_job(app: &axum::Router, id: &str) -> serde_json::Value {
    for _ in 0..100 {
        let (status, v) = call(app, Method::GET, &format!("/api/v1/jobs/{id}"), None).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let state = v["state"].as_str().unwrap_or("");
        if state == "done" || state == "failed" || state == "cancelled" {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("job {id} did not finish");
}

fn repo_path(f: &Fixture) -> PathBuf {
    f._repo.path().to_path_buf()
}

#[tokio::test]
async fn meta_lists_harness_and_spec_is_1_1() {
    let f = fixture("#!/bin/sh\nexit 0\n", vec![], Duration::from_secs(5));
    let (status, v) = call(&f.app, Method::GET, "/api/v1/meta", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["specVersion"], "1.11");
    for flag in ["harness", "git.hunk"] {
        assert!(
            v["features"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(flag)),
            "{flag}"
        );
    }
}

#[tokio::test]
async fn opt_in_is_required_and_edit_reports_changed_exactly() {
    let repo_script = "#!/bin/sh\nprintf '%s\\n' agent > a.txt\nprintf '%s\\n' created > created.txt\nrm -f gone.txt\n";
    let f = fixture(repo_script, vec![], Duration::from_secs(5));
    let root = repo_path(&f);
    std::fs::write(root.join("a.txt"), "old\n").unwrap();
    std::fs::write(root.join("keep.txt"), "keep\n").unwrap();
    std::fs::write(root.join("gone.txt"), "gone-bytes\n").unwrap();
    std::fs::write(root.join("dirty.txt"), "base\n").unwrap();
    git(&["add", "."], &root);
    git(&["commit", "-m", "init"], &root);
    std::fs::write(root.join("dirty.txt"), "base\ndirty\n").unwrap();
    let old_a = std::fs::read(root.join("a.txt")).unwrap();
    let old_gone = std::fs::read(root.join("gone.txt")).unwrap();
    let dirty = std::fs::read(root.join("dirty.txt")).unwrap();

    let (status, v) = call(&f.app, Method::GET, "/api/v1/harness", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(v["selected"].is_null());
    assert_eq!(v["pinned"], false);
    let claude = v["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["id"] == "claude")
        .unwrap();
    assert_eq!(claude["installed"], true);
    assert_eq!(claude["models"].as_array().unwrap().len(), 0);

    let (status, denied) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"a.txt","startLine":1,"endLine":1,"instruction":"edit"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{denied}");

    let (status, v) = call(
        &f.app,
        Method::PUT,
        "/api/v1/harness",
        Some(r#"{"id":"claude"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["selected"], "claude");
    assert_eq!(v["pinned"], true);

    let (status, v) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"a.txt","startLine":1,"endLine":1,"instruction":"edit a"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let id = v["job"]["id"].as_str().unwrap().to_string();
    let job = wait_job(&f.app, &id).await;
    assert_eq!(job["state"], "done", "{job}");
    let changed: Vec<&str> = job["result"]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(changed, ["a.txt", "created.txt", "gone.txt"]);
    assert!(!job["result"]["base"].as_str().unwrap().is_empty());

    let (status, rev) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/revert",
        Some(&format!(r#"{{"jobId":"{id}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rev}");
    let reverted: Vec<&str> = rev["reverted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(reverted, ["a.txt", "created.txt", "gone.txt"]);
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), old_a);
    assert_eq!(std::fs::read(root.join("gone.txt")).unwrap(), old_gone);
    assert!(!root.join("created.txt").exists());
    assert_eq!(std::fs::read(root.join("dirty.txt")).unwrap(), dirty);
    assert_eq!(std::fs::read(root.join("keep.txt")).unwrap(), b"keep\n");
}

#[tokio::test]
async fn overlapping_range_is_409() {
    let hold = tempfile::NamedTempFile::new().unwrap();
    let hold_path = hold.path().to_path_buf();
    // The file existing means "release". Start without it.
    drop(hold);
    let script = "#!/bin/sh\ni=0\nwhile [ ! -f \"$HARNESS_HOLD\" ] && [ \"$i\" -lt 200 ]; do\n  sleep 0.05\n  i=$((i+1))\ndone\n";
    let f = fixture(
        script,
        vec![("HARNESS_HOLD".into(), hold_path.display().to_string())],
        Duration::from_secs(30),
    );
    std::fs::write(repo_path(&f).join("a.txt"), "a\n").unwrap();
    std::fs::write(repo_path(&f).join("b.txt"), "b\n").unwrap();
    git(&["add", "."], &repo_path(&f));
    git(&["commit", "-m", "init"], &repo_path(&f));
    let (status, _) = call(
        &f.app,
        Method::PUT,
        "/api/v1/harness",
        Some(r#"{"id":"claude"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, first) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"a.txt","startLine":1,"endLine":10,"instruction":"hold"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, overlap) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"a.txt","startLine":10,"endLine":12,"instruction":"overlap"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{overlap}");
    assert_eq!(overlap["error"]["code"], "conflict");
    let (status, other) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"b.txt","startLine":1,"endLine":4,"instruction":"other file"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{other}");
    let (status, adjacent) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"a.txt","startLine":11,"endLine":20,"instruction":"adjacent"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{adjacent}");

    std::fs::write(&hold_path, "go\n").unwrap();
    for id in [
        first["job"]["id"].as_str().unwrap(),
        other["job"]["id"].as_str().unwrap(),
        adjacent["job"]["id"].as_str().unwrap(),
    ] {
        let job = wait_job(&f.app, id).await;
        assert_eq!(job["state"], "done", "{job}");
    }
}

#[tokio::test]
async fn long_output_is_a_truncated_tail() {
    let mut script = String::from("#!/bin/sh\nprintf '%s' 'HEAD");
    script.push_str(&"A".repeat(40_000));
    script.push_str("TAIL'\nprintf '%s' 'HEAD");
    script.push_str(&"B".repeat(40_000));
    script.push_str("TAIL' >&2\n");
    let f = fixture(&script, vec![], Duration::from_secs(5));
    std::fs::write(repo_path(&f).join("a.txt"), "a\n").unwrap();
    git(&["add", "."], &repo_path(&f));
    git(&["commit", "-m", "init"], &repo_path(&f));
    let (status, _) = call(
        &f.app,
        Method::PUT,
        "/api/v1/harness",
        Some(r#"{"id":"claude"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"a.txt","startLine":1,"endLine":1,"instruction":"noise"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let job = wait_job(&f.app, v["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "{job}");
    let result = &job["result"];
    assert_eq!(result["stdoutTruncated"], true);
    assert_eq!(result["stderrTruncated"], true);
    let stdout = result["stdoutTail"].as_str().unwrap();
    let stderr = result["stderrTail"].as_str().unwrap();
    assert!(stdout.ends_with("TAIL") && !stdout.contains("HEAD"));
    assert!(stderr.ends_with("TAIL") && !stderr.contains("HEAD"));
    assert_eq!(stdout.len(), 32 * 1024);
}

#[tokio::test]
async fn timeout_is_reported_on_the_job() {
    let f = fixture("#!/bin/sh\nsleep 30\n", vec![], Duration::from_millis(200));
    std::fs::write(repo_path(&f).join("a.txt"), "a\n").unwrap();
    git(&["add", "."], &repo_path(&f));
    git(&["commit", "-m", "init"], &repo_path(&f));
    let (status, _) = call(
        &f.app,
        Method::PUT,
        "/api/v1/harness",
        Some(r#"{"id":"claude"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"a.txt","startLine":1,"endLine":1,"instruction":"sleep"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let job = wait_job(&f.app, v["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert_eq!(job["error"]["code"], "timeout");
    assert!(job["error"]["message"]
        .as_str()
        .unwrap()
        .contains("timed out"));
    assert_eq!(job["result"]["timedOut"], true);
    assert_eq!(job["result"]["exitCode"], 124);
}

#[tokio::test]
async fn git_hunk_reverts_one_hunk() {
    let f = fixture("#!/bin/sh\nexit 0\n", vec![], Duration::from_secs(5));
    let root = repo_path(&f);
    let mut original = String::new();
    for i in 1..=30 {
        original.push_str(&format!("line{i}\n"));
    }
    std::fs::write(root.join("a.txt"), &original).unwrap();
    git(&["add", "a.txt"], &root);
    git(&["commit", "-m", "lines"], &root);
    let mut edited = original.clone();
    edited = edited.replacen("line2\n", "LINE2\n", 1);
    edited = edited.replacen("line20\n", "LINE20\n", 1);
    std::fs::write(root.join("a.txt"), &edited).unwrap();

    let (status, diff) = call(
        &f.app,
        Method::GET,
        "/api/v1/git/diff?path=a.txt&base=HEAD&target=worktree&hl=0",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{diff}");
    let hunks = diff["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 2, "{diff}");
    let index_before = std::fs::read(root.join(".git/index")).unwrap();
    let hunk_id = hunks
        .iter()
        .find(|h| {
            h["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["text"] == "LINE2")
        })
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let (status, st) = call(
        &f.app,
        Method::POST,
        "/api/v1/git/hunk",
        Some(&format!(
            r#"{{"path":"a.txt","hunkId":"{hunk_id}","base":"HEAD","target":"worktree","action":"revert"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{st}");
    assert!(st.get("branch").is_some(), "{st}");
    let after = std::fs::read(root.join("a.txt")).unwrap();
    let mut expect = original;
    expect = expect.replacen("line20\n", "LINE20\n", 1);
    assert_eq!(after, expect.into_bytes());
    assert_eq!(
        std::fs::read(root.join(".git/index")).unwrap(),
        index_before
    );

    let (status, missing) = call(
        &f.app,
        Method::POST,
        "/api/v1/git/hunk",
        Some(r#"{"path":"a.txt","hunkId":"missing","base":"HEAD","target":"worktree","action":"revert"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
}

#[tokio::test]
async fn harness_stdout_redacts_secrets() {
    let script = "#!/bin/sh\nprintf '%s\\n' 'AKIAIOSFODNN7EXAMPLE'\nprintf '%s\\n' 'api_key: \"abcdefghijklmnop\"' >&2\nexit 0\n";
    let f = fixture(script, vec![], Duration::from_secs(5));
    let root = repo_path(&f);
    std::fs::write(root.join("a.txt"), "old\n").unwrap();
    git(&["add", "a.txt"], &root);
    git(&["commit", "-m", "init"], &root);
    let (status, v) = call(
        &f.app,
        Method::PUT,
        "/api/v1/harness",
        Some(r#"{"id":"claude"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/edit",
        Some(r#"{"path":"a.txt","startLine":1,"endLine":1,"instruction":"edit"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let id = v["job"]["id"].as_str().unwrap();
    let job = wait_job(&f.app, id).await;
    assert_eq!(job["state"], "done", "{job}");
    let stdout = job["result"]["stdoutTail"].as_str().unwrap();
    let stderr = job["result"]["stderrTail"].as_str().unwrap();
    assert!(!stdout.contains("AKIAIOSFODNN7EXAMPLE"), "{stdout}");
    assert!(stdout.contains("[REDACTED]"));
    assert!(!stderr.contains("abcdefghijklmnop"), "{stderr}");
    assert!(stderr.contains("[REDACTED]"));
}

// ---------- agent threads (API.md § 10.7) ----------

/// Fake harness: the prompt is the last argument; it records it and edits notes.txt.
const RECORDING_HARNESS: &str = "#!/bin/sh\nfor last; do :; done\nprintf '%s' \"$last\" > \"$PWD/.last_prompt\"\necho \"edited\" >> notes.txt\necho done-output\n";

fn thread_fixture(script: &str) -> Fixture {
    let f = fixture(script, vec![], Duration::from_secs(10));
    let root = repo_path(&f);
    std::fs::write(root.join("notes.txt"), "start\n").unwrap();
    std::fs::write(root.join(".gitignore"), ".last_prompt\n").unwrap();
    git(&["add", "."], &root);
    git(&["commit", "-m", "init"], &root);
    f
}

async fn opt_in(f: &Fixture) {
    let (status, v) = call(
        &f.app,
        Method::PUT,
        "/api/v1/harness",
        Some(r#"{"id":"claude"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn thread_turns_run_the_harness_and_replay_earlier_turns() {
    let f = thread_fixture(RECORDING_HARNESS);
    opt_in(&f).await;
    let (status, t) = call(&f.app, Method::POST, "/api/v1/harness/threads", Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{t}");
    let tid = t["id"].as_str().unwrap().to_string();

    // Turn 1, with a review comment as context.
    let body = r#"{"message":"Tidy the notes","context":[{"path":"notes.txt","startLine":1,"endLine":1,"note":"Say what this file is for"}]}"#;
    let (status, v) = call(
        &f.app,
        Method::POST,
        &format!("/api/v1/harness/threads/{tid}/turns"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let job = wait_job(&f.app, v["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "{job}");
    let first_prompt = std::fs::read_to_string(repo_path(&f).join(".last_prompt")).unwrap();
    assert!(
        first_prompt.contains("Tidy the notes")
            && first_prompt.contains("notes.txt line 1: Say what this file is for"),
        "{first_prompt}"
    );

    // The turn was saved with its result; the thread took its title from the first message.
    let (_, t) = call(
        &f.app,
        Method::GET,
        &format!("/api/v1/harness/threads/{tid}"),
        None,
    )
    .await;
    assert_eq!(t["title"], "Tidy the notes");
    assert_eq!(t["turns"][0]["state"], "done", "{t}");
    assert_eq!(
        t["turns"][0]["result"]["changed"],
        serde_json::json!(["notes.txt"])
    );

    // Turn 2 replays turn 1 (message, context, outcome, output) before the new message.
    let (status, v) = call(
        &f.app,
        Method::POST,
        &format!("/api/v1/harness/threads/{tid}/turns"),
        Some(r#"{"message":"Now shorten it"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    wait_job(&f.app, v["job"]["id"].as_str().unwrap()).await;
    let second = std::fs::read_to_string(repo_path(&f).join(".last_prompt")).unwrap();
    for want in [
        "--- Turn 1 ---",
        "User: Tidy the notes",
        "changed: notes.txt",
        "done-output",
        "--- Now ---",
        "Now shorten it",
    ] {
        assert!(second.contains(want), "missing {want:?} in {second}");
    }

    // Listing and revert by turn (the path a restarted server uses).
    let (_, list) = call(&f.app, Method::GET, "/api/v1/harness/threads", None).await;
    assert_eq!(list["threads"][0]["turns"], 2);
    let turn = &t["turns"][0];
    let body = serde_json::json!({ "jobId": turn["jobId"], "threadId": tid, "turnId": turn["id"] })
        .to_string();
    let (status, v) = call(&f.app, Method::POST, "/api/v1/harness/revert", Some(&body)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["reverted"], serde_json::json!(["notes.txt"]));
    assert_eq!(
        std::fs::read_to_string(repo_path(&f).join("notes.txt")).unwrap(),
        "start\n"
    );

    let (status, _) = call(
        &f.app,
        Method::DELETE,
        &format!("/api/v1/harness/threads/{tid}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &f.app,
        Method::GET,
        &format!("/api/v1/harness/threads/{tid}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_running_turn_per_thread_and_context_stays_in_the_repo() {
    let f = thread_fixture("#!/bin/sh\nsleep 1\n");
    opt_in(&f).await;
    let (_, t) = call(
        &f.app,
        Method::POST,
        "/api/v1/harness/threads",
        Some(r#"{"title":"slow"}"#),
    )
    .await;
    let tid = t["id"].as_str().unwrap().to_string();
    let url = format!("/api/v1/harness/threads/{tid}/turns");
    let (status, v) = call(&f.app, Method::POST, &url, Some(r#"{"message":"one"}"#)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, busy) = call(&f.app, Method::POST, &url, Some(r#"{"message":"two"}"#)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{busy}");
    assert_eq!(busy["error"]["detail"]["jobId"], v["job"]["id"]);
    let (status, _) = call(
        &f.app,
        Method::POST,
        &url,
        Some(r#"{"message":"x","context":[{"path":"../../etc/passwd"}]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&f.app, Method::GET, "/api/v1/harness/threads/t_nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    wait_job(&f.app, v["job"]["id"].as_str().unwrap()).await;
}
