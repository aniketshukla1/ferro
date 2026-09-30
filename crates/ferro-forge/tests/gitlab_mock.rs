//! GitLab shapes against a local mock (no network in CI): metadata, size
//! stats, pipelines, discussions → threads, draft-notes submit, approvals.
//! Responses follow GitLab v4 as served (e.g. the MR JSON has
//! `changes_count` and `head_pipeline`, never a `changes` array).

use ferro_forge::github::{ReviewComment, ReviewEvent};
use ferro_forge::{ForgeError, ForgeRef, Provider};
use std::sync::{Arc, Mutex};

/// Each request gets the response of the route whose method and path (query
/// ignored) match exactly; anything else is a 404. Order-independent, so
/// tests pin behavior rather than call sequences.
struct Routed {
    addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

type Route = (&'static str, String, u16, serde_json::Value);

impl Routed {
    async fn start(routes: Vec<Route>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let routes = Arc::new(routes);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let r2 = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let (routes, r3) = (routes.clone(), r2.clone());
                tokio::spawn(async move {
                    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
                    let (rh, mut wh) = sock.into_split();
                    let mut rd = tokio::io::BufReader::new(rh);
                    loop {
                        let mut head = Vec::new();
                        let mut len = 0usize;
                        loop {
                            let mut line = String::new();
                            match rd.read_line(&mut line).await {
                                Ok(0) | Err(_) => return,
                                Ok(_) => {}
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
                            head.push(line);
                        }
                        let mut body = vec![0u8; len];
                        if len > 0 && rd.read_exact(&mut body).await.is_err() {
                            return;
                        }
                        let first = head.first().cloned().unwrap_or_default();
                        let mut it = first.split_whitespace();
                        let method = it.next().unwrap_or("").to_string();
                        let target = it.next().unwrap_or("").to_string();
                        let path = target.split('?').next().unwrap_or("").to_string();
                        r3.lock().unwrap().push(format!(
                            "{}\r\n{}",
                            head.join(""),
                            String::from_utf8_lossy(&body)
                        ));
                        let (status, mut bytes) = routes
                            .iter()
                            .find(|(m, p, _, _)| *m == method && *p == path)
                            .map(|(_, _, s, b)| (*s, serde_json::to_vec(b).unwrap()))
                            .unwrap_or((404, b"{\"message\":\"404 Not Found\"}".to_vec()));
                        if status == 204 {
                            bytes.clear();
                        }
                        let resp = format!(
                            "HTTP/1.1 {status} x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                            bytes.len()
                        );
                        if wh.write_all(resp.as_bytes()).await.is_err()
                            || wh.write_all(&bytes).await.is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        Self { addr, requests }
    }

    fn client(&self) -> ferro_forge::gitlab::GitLab {
        ferro_forge::gitlab::GitLab::new(
            format!("http://{}/api/v4", self.addr),
            Some("glpat-sekret".into()),
        )
    }

    fn recorded(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    /// Requests whose request line is `method path` (query ignored).
    fn hits(&self, method: &str, path: &str) -> usize {
        self.recorded()
            .iter()
            .filter(|r| {
                let line = r.lines().next().unwrap_or("");
                let target = line.split_whitespace().nth(1).unwrap_or("");
                line.starts_with(method) && target.split('?').next() == Some(path)
            })
            .count()
    }

    /// Bodies of the requests matching `method path`, as JSON.
    fn bodies(&self, method: &str, path: &str) -> Vec<serde_json::Value> {
        self.recorded()
            .iter()
            .filter(|r| {
                let line = r.lines().next().unwrap_or("");
                let target = line.split_whitespace().nth(1).unwrap_or("");
                line.starts_with(method) && target.split('?').next() == Some(path)
            })
            .map(|r| {
                let body = r.split("\r\n\r\n").nth(1).unwrap_or("");
                serde_json::from_str(body).unwrap_or(serde_json::Value::Null)
            })
            .collect()
    }
}

const MR: &str = "/api/v4/projects/group%2Fsub%2Fproj/merge_requests/9";

fn mr_ref() -> ForgeRef {
    ForgeRef {
        provider: Provider::GitLab,
        host: "git.example.com".into(),
        owner: "group/sub".into(),
        repo: "proj".into(),
        number: 9,
    }
}

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

/// `GET merge_requests/:iid` as GitLab serves it: `changes_count` (a
/// string), no `changes` array, `diverged_commits_count` = commits behind
/// the target (not the MR's commits), `head_pipeline`.
fn real_mr(head: &str, pipeline: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "title": "Add it", "description": "body text", "state": "opened", "draft": false,
        "source_branch": "feat", "target_branch": "main", "sha": head,
        "diff_refs": {"base_sha": sha('b'), "head_sha": head, "start_sha": sha('s')},
        "source_project_id": 2, "target_project_id": 1,
        "author": {"username": "ann", "avatar_url": "https://x/a.png"},
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
        "web_url": "https://git.example.com/group/sub/proj/-/merge_requests/9",
        "changes_count": "2", "diverged_commits_count": 5,
        "head_pipeline": pipeline,
    })
}

fn comment(path: &str, line: u64, side: &str, start_line: Option<u64>) -> ReviewComment {
    ReviewComment {
        path: path.into(),
        body: format!("on {line}"),
        line: Some(line),
        side: Some(side.into()),
        start_line,
        start_side: start_line.map(|_| side.into()),
    }
}

#[tokio::test]
async fn pull_maps_metadata_fork_url_and_stats_with_bearer_auth() {
    let head = sha('h');
    let m = Routed::start(vec![
        ("GET", MR.into(), 200, real_mr(&head, serde_json::Value::Null)),
        (
            "GET",
            "/api/v4/projects/2".into(),
            200,
            serde_json::json!({"http_url_to_repo": "https://git.example.com/fork/proj.git"}),
        ),
        (
            "GET",
            format!("{MR}/diffs"),
            200,
            serde_json::json!([
                {"old_path": "a.txt", "new_path": "a.txt", "diff": "@@ -1,2 +1,2 @@\n ctx\n-del\n+add\n"},
                {"old_path": "o.txt", "new_path": "n.txt", "diff": "@@ -1 +1 @@\n-x\n+++y\n"},
            ]),
        ),
        (
            "GET",
            format!("{MR}/commits"),
            200,
            serde_json::json!([{"id": "c1"}, {"id": "c2"}, {"id": "c3"}]),
        ),
    ])
    .await;
    let g = m.client();
    let meta = g.pull(&mr_ref()).await.unwrap();
    assert_eq!(meta.title, "Add it");
    assert_eq!(meta.state, "open");
    assert!(!meta.merged);
    assert_eq!(
        (meta.base_ref.as_str(), meta.head_ref.as_str()),
        ("main", "feat")
    );
    assert_eq!(meta.head_sha, head);
    // Fork MR: pushes go to the source project, not the target.
    assert!(meta.is_fork);
    assert_eq!(
        meta.head_clone_url.as_deref(),
        Some("https://git.example.com/fork/proj.git")
    );
    // Size from /diffs and /commits (every +/- line counts: GitLab diffs
    // have no file headers).
    assert_eq!(
        (
            meta.changed_files,
            meta.additions,
            meta.deletions,
            meta.commits
        ),
        (2, 2, 2, 3)
    );
    // Polls reuse both (fixed per MR / per head): one GET each.
    let again = g.pull(&mr_ref()).await.unwrap();
    assert_eq!(again.head_clone_url, meta.head_clone_url);
    assert_eq!(m.hits("GET", "/api/v4/projects/2"), 1);
    assert_eq!(m.hits("GET", &format!("{MR}/diffs")), 1);
    // OAuth tokens (a glab web login) only work as Bearer; the token is in
    // that header and nowhere else.
    for raw in m.recorded() {
        let low = raw.to_lowercase();
        assert!(low.contains("authorization: bearer glpat-sekret"), "{raw}");
        assert!(!low.contains("private-token"), "{raw}");
        let first = raw.lines().next().unwrap_or("");
        assert!(!first.contains("glpat-sekret"), "{raw}");
    }
}

#[tokio::test]
async fn pull_survives_missing_stats_endpoints() {
    // No /diffs, /changes or /commits (old or locked-down instance): the
    // metadata still loads, files from `changes_count`.
    let m = Routed::start(vec![(
        "GET",
        MR.into(),
        200,
        real_mr(&sha('h'), serde_json::Value::Null),
    )])
    .await;
    let meta = m.client().pull(&mr_ref()).await.unwrap();
    assert_eq!(meta.changed_files, 2);
    assert_eq!(meta.head_clone_url, None);
}

#[tokio::test]
async fn checks_follow_the_head_pipeline() {
    let head = sha('h');
    for (pipeline, want) in [
        (
            serde_json::json!({"status": "failed", "sha": head, "web_url": "https://ci/p/1"}),
            "failure",
        ),
        (
            serde_json::json!({"status": "running", "sha": head, "web_url": "https://ci/p/2"}),
            "pending",
        ),
        (
            serde_json::json!({"status": "success", "sha": head, "web_url": "https://ci/p/3"}),
            "success",
        ),
        (serde_json::Value::Null, "none"),
    ] {
        let url = pipeline.get("web_url").cloned();
        let m = Routed::start(vec![
            ("GET", MR.into(), 200, real_mr(&head, pipeline)),
            // Approval is not CI: an approved MR with a red pipeline is red.
            (
                "GET",
                format!("{MR}/approvals"),
                200,
                serde_json::json!({"approved": true, "approved_by": [{"user": {"username": "boss"}}]}),
            ),
        ])
        .await;
        let c = m.client().checks(&mr_ref(), &head).await.unwrap();
        assert_eq!(c.state, want);
        assert_eq!(c.url.map(serde_json::Value::from), url);
    }
    // A pipeline of an older head says nothing about this one.
    let m = Routed::start(vec![(
        "GET",
        MR.into(),
        200,
        real_mr(
            &head,
            serde_json::json!({"status": "success", "sha": sha('o'), "web_url": "https://ci/p/4"}),
        ),
    )])
    .await;
    assert_eq!(
        m.client().checks(&mr_ref(), &head).await.unwrap().state,
        "none"
    );
}

fn diff_note(id: u64, head: &str, new_line: u64) -> serde_json::Value {
    serde_json::json!({"id": id, "type": "DiffNote", "body": format!("n{id}"),
        "author": {"username": "ann"}, "created_at": "2026-01-01T00:00:00Z", "system": false,
        "position": {"base_sha": sha('b'), "start_sha": sha('s'), "head_sha": head,
            "old_path": "a.txt", "new_path": "a.txt", "position_type": "text",
            "old_line": null, "new_line": new_line, "line_range": null}})
}

#[tokio::test]
async fn discussions_split_threads_and_conversation() {
    let head = sha('h');
    let mut ranged = diff_note(11, &head, 10);
    ranged["position"]["line_range"] = serde_json::json!({
        "start": {"line_code": "x_8_8", "type": "old", "old_line": 8, "new_line": 8},
        "end": {"line_code": "x_9_10", "type": "new", "old_line": null, "new_line": 10},
    });
    let discussions = serde_json::json!([
        {"id": "d1", "individual_note": false, "resolved": false,
         "notes": [ranged, diff_note(12, &head, 10)]},
        {"id": "d2", "individual_note": true, "notes": [
            {"id": 13, "type": null, "body": "general", "author": {"username": "ann"},
             "created_at": "2026-01-01T00:00:00Z", "system": false},
            {"id": 14, "type": null, "body": "bot noise", "author": {"username": "bot"},
             "created_at": "2026-01-01T00:00:00Z", "system": true},
        ]},
    ]);
    let m = Routed::start(vec![
        (
            "GET",
            MR.into(),
            200,
            real_mr(&head, serde_json::Value::Null),
        ),
        ("GET", format!("{MR}/discussions"), 200, discussions),
    ])
    .await;
    let client = ferro_forge::ForgeClient::GitLab(m.client());
    let (threads, conv) = client.review_state(&mr_ref()).await.unwrap();
    assert_eq!(threads.len(), 1);
    let t = &threads[0];
    assert_eq!((t.id.as_str(), t.path.as_str()), ("d1", "a.txt"));
    // Anchored on the position's own line; the range starts at 8.
    assert_eq!(
        (t.line, t.start_line, t.side.as_str()),
        (Some(10), Some(8), "RIGHT")
    );
    assert_eq!(t.comments.len(), 2);
    assert!(!t.outdated);
    assert_eq!(conv.len(), 1);
    assert_eq!(conv[0].body, "general");
    // Both from a single pass over the discussions.
    assert_eq!(m.hits("GET", &format!("{MR}/discussions")), 1);
}

#[tokio::test]
async fn threads_on_older_versions_are_outdated() {
    let m = Routed::start(vec![
        (
            "GET",
            MR.into(),
            200,
            real_mr(&sha('2'), serde_json::Value::Null),
        ),
        (
            "GET",
            format!("{MR}/discussions"),
            200,
            serde_json::json!([
                {"id": "cur", "individual_note": false, "notes": [diff_note(1, &sha('2'), 3)]},
                {"id": "old", "individual_note": false, "notes": [diff_note(2, &sha('1'), 3)]},
            ]),
        ),
    ])
    .await;
    let threads = m.client().threads(&mr_ref()).await.unwrap();
    let by_id = |id: &str| threads.iter().find(|t| t.id == id).unwrap();
    assert!(!by_id("cur").outdated);
    assert_eq!(by_id("cur").commit_sha.as_deref(), Some(sha('2').as_str()));
    assert!(by_id("old").outdated);
}

/// Newest first: v2 (target moved, rebased) and v1, the checked-out head.
fn versions() -> serde_json::Value {
    serde_json::json!([
        {"id": 2, "head_commit_sha": sha('2'), "base_commit_sha": sha('c'), "start_commit_sha": sha('d')},
        {"id": 1, "head_commit_sha": sha('1'), "base_commit_sha": sha('a'), "start_commit_sha": sha('e')},
    ])
}

/// v1's file diffs: a.txt changes line 4 and keeps 3, 5, 6 as context;
/// b.txt was renamed from old_b.txt.
fn v1_diffs() -> serde_json::Value {
    serde_json::json!({"id": 1, "diffs": [
        {"old_path": "a.txt", "new_path": "a.txt",
         "diff": "@@ -3,4 +3,4 @@\n ctx3\n-old4\n+new4\n ctx5\n ctx6\n"},
        {"old_path": "old_b.txt", "new_path": "b.txt",
         "diff": "@@ -7,3 +7,3 @@\n c7\n-o8\n+n8\n c9\n"},
    ]})
}

#[tokio::test]
async fn submit_drafts_and_bulk_publish() {
    let m = Routed::start(vec![
        (
            "GET",
            MR.into(),
            200,
            real_mr(&sha('2'), serde_json::Value::Null),
        ),
        ("GET", format!("{MR}/versions"), 200, versions()),
        ("GET", format!("{MR}/versions/1"), 200, v1_diffs()),
        (
            "POST",
            format!("{MR}/draft_notes"),
            201,
            serde_json::json!({"id": 101}),
        ),
        (
            "POST",
            format!("{MR}/draft_notes/bulk_publish"),
            204,
            serde_json::json!({}),
        ),
    ])
    .await;
    let r = m
        .client()
        .submit_review(
            &mr_ref(),
            &sha('1'),
            ReviewEvent::RequestChanges,
            "needs work",
            &[
                // Context line: both lines. Added line: new only.
                comment("a.txt", 5, "RIGHT", None),
                comment("a.txt", 4, "RIGHT", None),
                // Multi-line on the renamed file's old side.
                comment("b.txt", 8, "LEFT", Some(7)),
            ],
        )
        .await
        .unwrap();
    assert_eq!(r.state, "REQUEST_CHANGES");
    let drafts = m.bodies("POST", &format!("{MR}/draft_notes"));
    assert_eq!(drafts.len(), 3);
    let pos = |i: usize| drafts[i]["position"].clone();
    // The position is v1's own triple, not v2's base/start with v1's head.
    assert_eq!(pos(0)["base_sha"], sha('a'));
    assert_eq!(pos(0)["start_sha"], sha('e'));
    assert_eq!(pos(0)["head_sha"], sha('1'));
    assert_eq!(pos(0)["old_line"], 5);
    assert_eq!(pos(0)["new_line"], 5);
    assert_eq!(pos(1)["new_line"], 4);
    assert!(pos(1).get("old_line").is_none());
    let b = pos(2);
    assert_eq!(
        (b["old_path"].as_str(), b["new_path"].as_str()),
        (Some("old_b.txt"), Some("b.txt"))
    );
    assert_eq!(b["old_line"], 8);
    assert!(b.get("new_line").is_none());
    let range = &b["line_range"];
    assert_eq!(range["start"]["type"], "old");
    assert_eq!(range["start"]["old_line"], 7);
    assert_eq!(range["start"]["new_line"], 7);
    assert!(range["end"]["line_code"]
        .as_str()
        .unwrap()
        .ends_with("_8_8"));
    let bulk = m.bodies("POST", &format!("{MR}/draft_notes/bulk_publish"));
    assert_eq!(bulk[0]["note"], "needs work");
    assert_eq!(bulk[0]["reviewer_state"], "requested_changes");
}

#[tokio::test]
async fn submit_keeps_approval_idempotent() {
    let m = Routed::start(vec![
        ("GET", MR.into(), 200, real_mr(&sha('2'), serde_json::Value::Null)),
        ("GET", format!("{MR}/versions"), 200, versions()),
        (
            "GET",
            format!("{MR}/approvals"),
            200,
            serde_json::json!({"approved": true, "user_has_approved": true, "user_can_approve": false}),
        ),
        ("POST", format!("{MR}/draft_notes"), 201, serde_json::json!({"id": 101})),
        ("POST", format!("{MR}/draft_notes/bulk_publish"), 204, serde_json::json!({})),
        // Already approved: GitLab would answer 401.
        ("POST", format!("{MR}/approve"), 401, serde_json::json!({"message": "401 Unauthorized"})),
    ])
    .await;
    m.client()
        .submit_review(
            &mr_ref(),
            &sha('1'),
            ReviewEvent::Approve,
            "lgtm",
            &[comment("a.txt", 5, "RIGHT", None)],
        )
        .await
        .unwrap();
    assert_eq!(m.hits("POST", &format!("{MR}/approve")), 0);
    assert_eq!(m.hits("POST", &format!("{MR}/draft_notes/bulk_publish")), 1);
}

#[tokio::test]
async fn refused_approval_fails_before_anything_is_staged() {
    let m = Routed::start(vec![
        ("GET", MR.into(), 200, real_mr(&sha('1'), serde_json::Value::Null)),
        ("GET", format!("{MR}/versions"), 200, versions()),
        (
            "GET",
            format!("{MR}/approvals"),
            200,
            serde_json::json!({"approved": false, "user_has_approved": false, "user_can_approve": false}),
        ),
        ("POST", format!("{MR}/draft_notes"), 201, serde_json::json!({"id": 101})),
        ("POST", format!("{MR}/draft_notes/bulk_publish"), 204, serde_json::json!({})),
        ("POST", format!("{MR}/approve"), 401, serde_json::json!({"message": "401 Unauthorized"})),
    ])
    .await;
    let err = m
        .client()
        .submit_review(
            &mr_ref(),
            &sha('1'),
            ReviewEvent::Approve,
            "",
            &[comment("a.txt", 5, "RIGHT", None)],
        )
        .await
        .unwrap_err();
    // Not a token problem: the token works, GitLab declines the approval.
    assert!(matches!(err, ForgeError::Refused { .. }), "{err:?}");
    assert_eq!(err.detail()["provider"], "gitlab");
    assert_eq!(m.hits("POST", &format!("{MR}/draft_notes")), 0);
}

#[tokio::test]
async fn failed_submit_discards_the_drafts_it_staged() {
    let m = Routed::start(vec![
        (
            "GET",
            MR.into(),
            200,
            real_mr(&sha('1'), serde_json::Value::Null),
        ),
        ("GET", format!("{MR}/versions"), 200, versions()),
        (
            "POST",
            format!("{MR}/draft_notes"),
            201,
            serde_json::json!({"id": 101}),
        ),
        (
            "POST",
            format!("{MR}/draft_notes/bulk_publish"),
            500,
            serde_json::json!({}),
        ),
        (
            "DELETE",
            format!("{MR}/draft_notes/101"),
            204,
            serde_json::json!({}),
        ),
    ])
    .await;
    let res = m
        .client()
        .submit_review(
            &mr_ref(),
            &sha('1'),
            ReviewEvent::Comment,
            "",
            &[comment("a.txt", 5, "RIGHT", None)],
        )
        .await;
    assert!(res.is_err());
    // A retry must not publish a second copy of the staged note.
    assert_eq!(m.hits("DELETE", &format!("{MR}/draft_notes/101")), 1);
}

#[tokio::test]
async fn approve_reply_and_reply_drafts() {
    let m = Routed::start(vec![
        ("GET", MR.into(), 200, real_mr(&sha('1'), serde_json::Value::Null)),
        ("GET", format!("{MR}/versions"), 200, versions()),
        (
            "GET",
            format!("{MR}/approvals"),
            200,
            serde_json::json!({"approved": false, "user_has_approved": false, "user_can_approve": true}),
        ),
        ("POST", format!("{MR}/approve"), 201, serde_json::json!({})),
        ("POST", format!("{MR}/draft_notes"), 201, serde_json::json!({"id": 55})),
        ("POST", format!("{MR}/draft_notes/bulk_publish"), 204, serde_json::json!({})),
        ("DELETE", format!("{MR}/draft_notes/55"), 204, serde_json::json!({})),
        (
            "POST",
            format!("{MR}/discussions/d9/notes"),
            201,
            serde_json::json!({"id": 21, "author": {"username": "me"}, "body": "done", "created_at": "2026-01-03T00:00:00Z"}),
        ),
    ])
    .await;
    let g = m.client();
    g.submit_review(&mr_ref(), &sha('1'), ReviewEvent::Approve, "", &[])
        .await
        .unwrap();
    assert_eq!(m.hits("POST", &format!("{MR}/approve")), 1);
    let c = g.reply(&mr_ref(), "d9", "done").await.unwrap();
    assert_eq!(c.body, "done");
    // Staged reply drafts report their id, so a failed submit can drop them.
    let id = g.reply_draft(&mr_ref(), "d9", "later").await.unwrap();
    assert_eq!(id, 55);
    assert_eq!(
        m.bodies("POST", &format!("{MR}/draft_notes"))[0]["in_reply_to_discussion_id"],
        "d9"
    );
    g.discard_drafts(&mr_ref(), &[id]).await;
    assert_eq!(m.hits("DELETE", &format!("{MR}/draft_notes/55")), 1);
}

#[tokio::test]
async fn merged_review_comments_read_notes_of_merged_mrs() {
    let base = "/api/v4/projects/group%2Fsub%2Fproj".to_string();
    let m = Routed::start(vec![
        ("GET", format!("{base}/merge_requests"), 200, serde_json::json!([
            {"iid": 4, "author": {"username": "ann"}, "web_url": "https://git.example.com/group/sub/proj/-/merge_requests/4"}
        ])),
        ("GET", format!("{base}/merge_requests/4/notes"), 200, serde_json::json!([
            {"id": 71, "system": true, "author": {"username": "ann"}, "body": "changed the description"},
            {"id": 72, "system": false, "author": {"username": "rev", "bot": false}, "body": "Use the logger, not println",
             "position": {"new_path": "src/main.rs"}},
            {"id": 73, "system": false, "author": {"username": "ci-bot"}, "body": "pipeline passed"}
        ])),
    ])
    .await;
    let (prs, notes) = m
        .client()
        .merged_review_comments(&mr_ref(), 30)
        .await
        .unwrap();
    assert_eq!(prs, 1);
    assert_eq!(notes.len(), 2);
    assert_eq!(
        notes[0].url,
        "https://git.example.com/group/sub/proj/-/merge_requests/4#note_72"
    );
    assert_eq!(
        (
            notes[0].pr,
            notes[0].pr_author.as_str(),
            notes[0].path.as_deref()
        ),
        (4, "ann", Some("src/main.rs"))
    );
    assert!(notes[1].bot);
}
