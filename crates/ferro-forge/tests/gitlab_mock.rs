//! GitLab shapes against a local mock (no network in CI).
//! Metadata mapping, discussions→threads, draft-notes submit, approvals.

use ferro_forge::{ForgeRef, Provider};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

struct Scripted {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Mock {
    addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    async fn start(scripts: Vec<Scripted>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let scripts = Arc::new(Mutex::new(VecDeque::from(scripts)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let s2 = scripts.clone();
        let r2 = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let s3 = s2.clone();
                let r3 = r2.clone();
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
                                Ok(0) => return,
                                Ok(_) => {}
                                Err(_) => return,
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
                        let raw = format!("{}{}", head.join(""), String::from_utf8_lossy(&body));
                        r3.lock().unwrap().push(raw);
                        let script = s3.lock().unwrap().pop_front().unwrap_or(Scripted {
                            status: 500,
                            headers: vec![],
                            body: b"no script".to_vec(),
                        });
                        let mut resp = format!(
                            "HTTP/1.1 {} x\r\nContent-Length: {}\r\nConnection: keep-alive\r\n",
                            script.status,
                            script.body.len()
                        );
                        for (k, v) in &script.headers {
                            resp.push_str(&format!("{k}: {v}\r\n"));
                        }
                        resp.push_str("\r\n");
                        if wh.write_all(resp.as_bytes()).await.is_err() {
                            return;
                        }
                        if wh.write_all(&script.body).await.is_err() {
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
}

fn json(v: serde_json::Value) -> Scripted {
    Scripted {
        status: 200,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: serde_json::to_vec(&v).unwrap(),
    }
}

fn mr_ref() -> ForgeRef {
    ForgeRef {
        provider: Provider::GitLab,
        host: "git.example.com".into(),
        owner: "group/sub".into(),
        repo: "proj".into(),
        number: 9,
    }
}

fn mr_json() -> serde_json::Value {
    serde_json::json!({
        "title": "Add it", "description": "body text", "state": "opened",
        "draft": false, "source_branch": "feat", "target_branch": "main",
        "sha": "h".repeat(40).as_str(),
        "diff_refs": {"base_sha": "b".repeat(40).as_str(), "head_sha": "h".repeat(40).as_str(), "start_sha": "s".repeat(40).as_str()},
        "source_project_id": 2, "target_project_id": 1,
        "author": {"username": "ann", "avatar_url": "https://x/a.png"},
        "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
        "web_url": "https://git.example.com/group/sub/proj/-/merge_requests/9",
        "diverged_commits_count": 4,
        "changes": [
            {"old_path": "a.txt", "new_path": "a.txt", "diff": "@@ -1,2 +1,2 @@\n ctx\n-del\n+add\n"},
            {"old_path": "o.txt", "new_path": "n.txt", "diff": "@@ -1 +1 @@\n-x\n+y\n"},
        ],
    })
}

#[tokio::test]
async fn metadata_and_stats() {
    let m = Mock::start(vec![json(mr_json())]).await;
    let meta = m.client().pull(&mr_ref()).await.unwrap();
    assert_eq!(meta.title, "Add it");
    assert_eq!(meta.state, "open");
    assert!(!meta.merged);
    assert_eq!(meta.base_ref, "main");
    assert_eq!(meta.head_ref, "feat");
    assert!(meta.is_fork);
    assert_eq!(
        (meta.additions, meta.deletions, meta.changed_files),
        (2, 2, 2)
    );
    assert_eq!(meta.commits, 4);
    for raw in m.recorded() {
        let low = raw.to_lowercase();
        assert!(low.contains("private-token: glpat-sekret"), "{raw}");
        let without_headers: String = raw.lines().skip_while(|l| !l.is_empty()).collect();
        assert!(!without_headers.contains("glpat-sekret"), "{raw}");
        assert!(
            raw.contains("/api/v4/projects/group%2Fsub%2Fproj/merge_requests/9 "),
            "{raw}"
        );
    }
}

#[tokio::test]
async fn discussions_split_threads_and_conversation() {
    let positioned = serde_json::json!({
        "id": "d1", "individual_note": false, "resolved": false,
        "notes": [
            {"id": 11, "type": "DiffNote", "body": "look", "author": {"username": "ann"},
             "created_at": "2026-01-01T00:00:00Z", "system": false,
             "position": {"base_sha": "b", "start_sha": "s", "head_sha": "h",
                 "old_path": "a.txt", "new_path": "a.txt", "position_type": "text",
                 "old_line": null, "new_line": 10,
                 "line_range": {"start": {"type": "new", "new_line": 8}, "end": {"type": "new", "new_line": 10}}}},
            {"id": 12, "type": "DiffNote", "body": "ack", "author": {"username": "bob"},
             "created_at": "2026-01-02T00:00:00Z", "system": false,
             "position": {"base_sha": "b", "start_sha": "s", "head_sha": "h",
                 "old_path": "a.txt", "new_path": "a.txt", "position_type": "text",
                 "old_line": null, "new_line": 10, "line_range": null}},
        ],
    });
    let top_level = serde_json::json!({
        "id": "d2", "individual_note": true,
        "notes": [
            {"id": 13, "type": null, "body": "general", "author": {"username": "ann"},
             "created_at": "2026-01-01T00:00:00Z", "system": false},
            {"id": 14, "type": null, "body": "bot noise", "author": {"username": "bot"},
             "created_at": "2026-01-01T00:00:00Z", "system": true},
        ],
    });
    // Short pages stop pagination: one script per call.
    let m = Mock::start(vec![
        json(serde_json::json!([positioned.clone()])),
        json(serde_json::json!([top_level.clone()])),
    ])
    .await;
    let g = m.client();
    let threads = g.threads(&mr_ref()).await.unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].id, "d1");
    assert_eq!(threads[0].path, "a.txt");
    assert_eq!(threads[0].line, Some(10));
    assert_eq!(threads[0].start_line, Some(8));
    assert_eq!(threads[0].side, "RIGHT");
    assert_eq!(threads[0].comments.len(), 2);
    let conv = g.conversation(&mr_ref()).await.unwrap();
    assert_eq!(conv.len(), 1);
    assert_eq!(conv[0].body, "general");
}

#[tokio::test]
async fn submit_drafts_and_bulk_publish() {
    use ferro_forge::github::{ReviewComment, ReviewEvent};
    let m = Mock::start(vec![
        json(mr_json()),                    // diff_refs
        json(serde_json::json!({"id": 1})), // draft note 1
        json(serde_json::json!({"id": 2})), // draft note 2
        json(serde_json::json!({})),        // bulk_publish (reviewer_state)
    ])
    .await;
    let r = m
        .client()
        .submit_review(
            &mr_ref(),
            &"h".repeat(40),
            ReviewEvent::RequestChanges,
            "needs work",
            &[
                ReviewComment {
                    path: "a.txt".into(),
                    body: "one".into(),
                    line: Some(5),
                    side: Some("RIGHT".into()),
                    start_line: None,
                    start_side: None,
                },
                ReviewComment {
                    path: "b.txt".into(),
                    body: "span".into(),
                    line: Some(9),
                    side: Some("LEFT".into()),
                    start_line: Some(7),
                    start_side: Some("LEFT".into()),
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(r.state, "REQUEST_CHANGES");
    let raw = m.recorded().join("\n");
    assert!(raw.contains("draft_notes"), "{raw}");
    assert!(raw.contains("bulk_publish"), "{raw}");
    assert!(raw.contains("requested_changes"), "{raw}");
    // Positions carry the diff shas and the multi-line range.
    assert!(raw.contains("line_range"), "{raw}");
    assert!(raw.contains("needs work"), "{raw}");
}

#[tokio::test]
async fn approve_and_reply() {
    let m = Mock::start(vec![
        json(mr_json()), // diff_refs
        json(serde_json::json!({})), // approve
        json(serde_json::json!({})), // bulk_publish
        json(serde_json::json!({"id": 21, "author": {"username": "me"}, "body": "done", "created_at": "2026-01-03T00:00:00Z"})),
    ])
    .await;
    let g = m.client();
    g.submit_review(
        &mr_ref(),
        &"h".repeat(40),
        ferro_forge::github::ReviewEvent::Approve,
        "",
        &[],
    )
    .await
    .unwrap();
    let c = g.reply(&mr_ref(), "d9", "done").await.unwrap();
    assert_eq!(c.body, "done");
    let raw = m.recorded().join("\n");
    assert!(raw.contains("/approve"), "{raw}");
    assert!(raw.contains("/discussions/d9/notes"), "{raw}");
}
