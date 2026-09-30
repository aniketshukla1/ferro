//! Checks on a change (API.md § 16) against small live repos.

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

/// A committed repo with `files`; the TempDir is leaked (the router keeps the root).
fn repo(files: &[(&str, &str)]) -> &'static std::path::Path {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let d = dir.path();
    git(&["init", "-b", "main"], d);
    git(&["config", "user.email", "t@t"], d);
    git(&["config", "user.name", "t"], d);
    git(&["config", "commit.gpgsign", "false"], d);
    for (p, c) in files {
        let full = d.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, c).unwrap();
    }
    git(&["add", "."], d);
    git(&["commit", "-m", "init"], d);
    d
}

pub fn router(root: &std::path::Path) -> axum::Router {
    let home = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let dirs = ferro_core::dirs::FerroDirs::new(
        home.path().join("c"),
        home.path().join("s"),
        home.path().join("h"),
    );
    let st = server::build_state(
        root.to_path_buf(),
        dirs,
        ferro_server::Host::Cli,
        "test".into(),
    );
    // Synchronous file index so the symbol and reference index has files to read.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let idx = st.ws().index.clone();
                rt.block_on(idx.rebuild());
            })
            .join()
            .unwrap();
    });
    let guard = Arc::new(ferro_server::guard::GuardConfig::new(
        Some(TOKEN.into()),
        7778,
        vec![],
        false,
        false,
        false,
    ));
    let last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    server::build_router(st, guard, last, true, None)
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::HOST, "127.0.0.1:7778")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let s = res.status();
    let b = axum::body::to_bytes(res.into_body(), 8 << 20)
        .await
        .unwrap();
    (
        s,
        serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn radar_flags_a_removed_function_that_callers_still_use() {
    let d = repo(&[
        (
            "src/lib.rs",
            "pub fn parse_config(s: &str) -> u32 { 0 }\n\npub fn render(x: u32) {}\n",
        ),
        (
            "src/main.rs",
            "fn main() {\n    let c = parse_config(\"a\");\n    render(c);\n}\n",
        ),
    ]);
    // Remove parse_config and add a parameter to render; main.rs still calls both.
    std::fs::write(d.join("src/lib.rs"), "pub fn render(x: u32, y: u32) {}\n").unwrap();
    let app = router(d);
    let (s, v) = get_json(app, "/api/v1/checks/breaking?base=HEAD&target=worktree").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let changes = v["changes"].as_array().unwrap();
    let gone = changes
        .iter()
        .find(|c| c["name"] == "parse_config")
        .unwrap();
    assert_eq!(gone["change"], "removed");
    assert_eq!(gone["severity"], "high");
    assert_eq!(gone["refs"]["outsideFile"], 1);
    assert_eq!(gone["refs"]["sample"][0]["path"], "src/main.rs");
    assert_eq!(gone["refs"]["sample"][0]["line"], 2);
    let sig = changes.iter().find(|c| c["name"] == "render").unwrap();
    assert_eq!(sig["change"], "signature");
    assert_eq!(sig["severity"], "medium");
    assert!(sig["newSignature"].as_str().unwrap().contains("y: u32"));
    assert_eq!(changes[0]["name"], "parse_config", "highest severity first");
}

async fn send(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "127.0.0.1:7778")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let s = res.status();
    let b = axum::body::to_bytes(res.into_body(), 8 << 20)
        .await
        .unwrap();
    (
        s,
        serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn affected_tests_plan_run_and_parse_failures() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("npm not installed; skipping");
        return;
    }
    // A package whose test script reports one failure the way jest does.
    let script = "console.log('  ✕ adds numbers'); console.log('    at Object.<anonymous> (src/add.js:3:5)'); console.log('Tests:       1 failed, 2 passed, 3 total'); process.exit(1)";
    let d = repo(&[
        (
            "package.json",
            &format!("{{\"name\":\"x\",\"scripts\":{{\"test\":\"node -e \\\"{script}\\\"\"}}}}"),
        ),
        ("src/add.js", "module.exports = (a, b) => a + b;\n"),
    ]);
    std::fs::write(d.join("src/add.js"), "module.exports = (a, b) => a - b;\n").unwrap();
    let app = router(d);
    let (s, plan) = get_json(app.clone(), "/api/v1/checks/tests/plan").await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["allowed"], true);
    assert_eq!(plan["steps"][0]["argv"], serde_json::json!(["npm", "test"]));
    let (s, v) = send(app.clone(), "POST", "/api/v1/checks/tests/run", "{}").await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["job"]["id"].as_str().unwrap().to_string();
    let t0 = std::time::Instant::now();
    let job = loop {
        let (_, j) = get_json(app.clone(), &format!("/api/v1/jobs/{id}")).await;
        if j["state"] != "running" || t0.elapsed() > std::time::Duration::from_secs(60) {
            break j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(job["state"], "done", "{job}");
    let r = &job["result"];
    assert_eq!(r["ok"], false);
    assert_eq!(
        (r["passed"].as_u64(), r["failed"].as_u64()),
        (Some(2), Some(1)),
        "{r}"
    );
    let f = &r["steps"][0]["failures"][0];
    assert_eq!(f["name"], "adds numbers");
    assert_eq!(f["path"], "src/add.js");
    assert_eq!(f["line"], 3);
}

#[tokio::test]
async fn tests_refused_when_the_setting_is_off() {
    let d = repo(&[
        ("package.json", r#"{"scripts":{"test":"node -e 1"}}"#),
        ("a.js", "1\n"),
    ]);
    std::fs::write(d.join("a.js"), "2\n").unwrap();
    let app = router(d);
    let (s, _) = send(
        app.clone(),
        "PUT",
        "/api/v1/settings?scope=user",
        r#"{"values":{"checks.runTests":"off"}}"#,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, plan) = get_json(app.clone(), "/api/v1/checks/tests/plan").await;
    assert_eq!(plan["allowed"], false);
    let (s, _) = send(app, "POST", "/api/v1/checks/tests/run", "{}").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn security_scan_flags_added_secrets_and_sinks_only() {
    let d = repo(&[
        (
            "web/app.js",
            "export function show(el, s) {\n  el.textContent = s;\n}\n",
        ),
        ("old.js", "el.innerHTML = legacy;\n"),
    ]);
    std::fs::write(
        d.join("web/app.js"),
        "export function show(el, s) {\n  el.innerHTML = s;\n}\nconst token = \"ghp_123456789012345678901234567890123456\";\n",
    )
    .unwrap();
    let app = router(d);
    let (s, v) = get_json(app.clone(), "/api/v1/checks/security").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let f = v["findings"].as_array().unwrap();
    assert_eq!(f.len(), 2, "{v}");
    assert_eq!(f[0]["rule"], "secret.github-token", "critical first");
    assert_eq!(f[0]["line"], 4);
    assert!(!f[0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("12345678901234567890"));
    assert_eq!(f[1]["rule"], "js.html-sink");
    assert_eq!(
        (f[1]["path"].as_str(), f[1]["line"].as_u64()),
        (Some("web/app.js"), Some(2))
    );
    // The pre-existing innerHTML in old.js is not part of the change.
    assert!(f.iter().all(|x| x["path"] != "old.js"));
    assert!(v["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "semgrep"));

    let (s, j) = send(app.clone(), "POST", "/api/v1/checks/security/deep", "{}").await;
    assert_eq!(s, StatusCode::ACCEPTED, "{j}");
    let id = j["job"]["id"].as_str().unwrap().to_string();
    let t0 = std::time::Instant::now();
    let job = loop {
        let (_, j) = get_json(app.clone(), &format!("/api/v1/jobs/{id}")).await;
        if j["state"] != "running" || t0.elapsed() > std::time::Duration::from_secs(120) {
            break j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(job["state"], "done", "{job}");
    let tools = job["result"]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .any(|t| t["name"] == "dependencies" && t["status"] == "not-needed"),
        "{tools:?}"
    );
}

#[tokio::test]
async fn coverage_of_added_lines_from_an_lcov_report() {
    let d = repo(&[("src/lib.rs", "pub fn a() -> u32 {\n    1\n}\n")]);
    // The change adds a branch; the report (written after the edit) says line 3 ran, 4 did not.
    std::fs::write(
        d.join("src/lib.rs"),
        "pub fn a(x: bool) -> u32 {\n    // pick\n    if x { 1 }\n    else { 2 }\n}\n",
    )
    .unwrap();
    let app = router(d);
    let (_, none) = get_json(app.clone(), "/api/v1/checks/coverage").await;
    assert!(none["report"].is_null());
    assert!(
        none["hints"][0]
            .as_str()
            .is_some_and(|h| h.contains("llvm-cov"))
            || none["hints"].as_array().is_some_and(|h| h.is_empty())
    );
    std::fs::write(
        d.join("lcov.info"),
        "SF:src/lib.rs\nDA:1,2\nDA:3,2\nDA:4,0\nend_of_record\n",
    )
    .unwrap();
    let (s, v) = get_json(app, "/api/v1/checks/coverage").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["report"]["format"], "lcov");
    assert_eq!(
        v["totals"],
        serde_json::json!({ "executable": 3, "covered": 2, "uncovered": 1 })
    );
    assert_eq!(v["percent"], 66.7);
    assert_eq!(v["files"][0]["uncovered"], serde_json::json!([4]));
    assert_eq!(v["stale"], false);
}

#[tokio::test]
async fn team_memory_rules_hide_findings_and_flag_new_ignores() {
    let d = repo(&[
        ("tests/fixture.rs", "// fixture\n"),
        ("src/a.js", "export const a = 1;\n"),
    ]);
    std::fs::write(
        d.join("tests/fixture.rs"),
        "// fixture\nconst K: &str = \"AKIAABCDEFGHIJKLMNOP\";\n",
    )
    .unwrap();
    std::fs::write(
        d.join("src/a.js"),
        "export const a = 1;\nel.innerHTML = a;\n",
    )
    .unwrap();
    let app = router(d);

    // A rule must say what it ignores.
    let (s, _) = send(
        app.clone(),
        "POST",
        "/api/v1/memory/rules",
        r#"{"kind":"ignore","appliesTo":"security","paths":["src/**"]}"#,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Team rule: fake secrets in tests are fine.
    let (s, rule) = send(
        app.clone(),
        "POST",
        "/api/v1/memory/rules",
        r#"{"kind":"ignore","appliesTo":"security","rule":"secret.*","paths":["tests/**"],"reason":"test fixtures","scope":"team"}"#,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rule}");
    let id = rule["id"].as_str().unwrap().to_string();
    let file = std::fs::read_to_string(d.join(".ferro-rules.json")).unwrap();
    assert!(
        file.contains("\"secret.*\"") && file.contains("test fixtures"),
        "{file}"
    );

    let (_, v) = get_json(app.clone(), "/api/v1/checks/security").await;
    let f = v["findings"].as_array().unwrap();
    let secret = f.iter().find(|x| x["rule"] == "secret.aws-key").unwrap();
    assert_eq!(secret["suppressedBy"]["id"], id.as_str());
    assert_eq!(secret["suppressedBy"]["scope"], "team");
    let sink = f.iter().find(|x| x["rule"] == "js.html-sink").unwrap();
    assert!(sink.get("suppressedBy").is_none(), "other findings stay");
    // The rules file itself is new in this change: reviewers are told what it hides.
    let flag = f.iter().find(|x| x["rule"] == "memory.new-ignore").unwrap();
    assert!(
        flag["detail"].as_str().unwrap().contains("secret.*"),
        "{flag}"
    );
    assert!(flag.get("suppressedBy").is_none());

    let (_, m) = get_json(app.clone(), "/api/v1/memory").await;
    assert_eq!(m["team"]["exists"], true);
    let r = m["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id.as_str())
        .unwrap();
    assert_eq!(
        (r["scope"].as_str(), r["hits"].as_u64()),
        (Some("team"), Some(1))
    );

    // Make it personal, then delete it.
    let (s, moved) = send(
        app.clone(),
        "PATCH",
        &format!("/api/v1/memory/rules/{id}"),
        r#"{"scope":"personal"}"#,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{moved}");
    assert!(!std::fs::read_to_string(d.join(".ferro-rules.json"))
        .unwrap()
        .contains(&id));
    let (_, m) = get_json(app.clone(), "/api/v1/memory").await;
    assert_eq!(m["rules"][0]["scope"], "personal");
    let (s, _) = send(
        app.clone(),
        "DELETE",
        &format!("/api/v1/memory/rules/{id}"),
        "",
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Two dismissals of the same finding type suggest an ignore rule; "not now" removes it.
    for p in ["tests/a.rs", "tests/b.rs"] {
        let body = format!(
            r#"{{"action":"dismiss","source":"security","rule":"secret.aws-key","title":"AWS access key","path":"{p}"}}"#
        );
        let (s, _) = send(app.clone(), "POST", "/api/v1/memory/signals", &body).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    let (_, m) = get_json(app.clone(), "/api/v1/memory").await;
    let sug = &m["suggestions"][0];
    assert_eq!(sug["rule"]["rule"], "secret.aws-key");
    assert_eq!(sug["rule"]["paths"], serde_json::json!(["tests/**"]));
    let key = sug["key"].as_str().unwrap().to_string();
    let (s, _) = send(
        app.clone(),
        "POST",
        "/api/v1/memory/suggestions/dismiss",
        &serde_json::json!({ "key": key }).to_string(),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, m) = get_json(app, "/api/v1/memory").await;
    assert_eq!(m["suggestions"].as_array().unwrap().len(), 0);
}
