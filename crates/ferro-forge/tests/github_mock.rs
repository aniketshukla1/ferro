//! Recorded-shape tests against a local mock server (no network in CI).
//! Covers metadata, ETag/304 reuse, rate limits, thread pagination, submit
//! shape, and the token-hygiene rule (token only in Authorization).

use ferro_forge::{ForgeRef, GitHub};
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
                    // One request per connection is enough for reqwest here,
                    // but keep-alive may pipeline; serve until EOF.
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

    fn client(&self) -> GitHub {
        let base = format!("http://{}/api", self.addr);
        GitHub::new(
            base.clone(),
            format!("{base}/graphql"),
            Some("sekret-token".into()),
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

fn pr_ref() -> ForgeRef {
    ForgeRef {
        provider: ferro_forge::Provider::GitHub,
        host: "github.com".into(),
        owner: "o".into(),
        repo: "r".into(),
        number: 7,
    }
}

#[tokio::test]
async fn metadata_and_permissions() {
    let m = Mock::start(vec![
        json(serde_json::json!({
            "title": "Fix it", "body": "details", "state": "open", "merged": false, "draft": false,
            "base": {"ref": "main", "sha": "b".repeat(40).as_str()},
            "head": {"ref": "feat", "sha": "h".repeat(40).as_str(), "repo": {"clone_url": "https://github.com/o/r.git", "fork": true}},
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
            "additions": 10, "deletions": 2, "changed_files": 1, "commits": 3,
            "html_url": "https://github.com/o/r/pull/7",
            "user": {"login": "ann", "avatar_url": "https://x/y.png"},
        })),
        json(serde_json::json!({"permissions": {"push": true}})),
    ])
    .await;
    let g = m.client();
    let meta = g.pull(&pr_ref()).await.unwrap();
    assert_eq!(meta.title, "Fix it");
    assert!(meta.is_fork);
    assert_eq!(
        meta.head_clone_url.as_deref(),
        Some("https://github.com/o/r.git")
    );
    assert!(g.can_push(&pr_ref()).await.unwrap());
    // Token hygiene: Authorization header only, never in URL or body.
    for raw in m.recorded() {
        let low = raw.to_lowercase();
        assert!(low.contains("authorization: bearer sekret-token"), "{raw}");
        let without_auth: String = raw.lines().skip_while(|l| !l.is_empty()).collect();
        assert!(!without_auth.contains("sekret-token"), "{raw}");
    }
}

#[tokio::test]
async fn etag_reuse_on_304() {
    let body = serde_json::json!({"permissions": {"push": false}});
    let m = Mock::start(vec![
        Scripted {
            status: 200,
            headers: vec![
                ("ETag".into(), "\"abc\"".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            body: serde_json::to_vec(&body).unwrap(),
        },
        Scripted {
            status: 304,
            headers: vec![],
            body: vec![],
        },
    ])
    .await;
    let g = m.client();
    assert!(!g.can_push(&pr_ref()).await.unwrap());
    assert!(!g.can_push(&pr_ref()).await.unwrap());
    let rec = m.recorded();
    assert_eq!(rec.len(), 2);
    assert!(
        rec[1].to_lowercase().contains("if-none-match: \"abc\""),
        "{}",
        rec[1]
    );
}

#[tokio::test]
async fn rate_limit_maps_with_retry_after() {
    let m = Mock::start(vec![Scripted {
        status: 429,
        headers: vec![("Retry-After".into(), "7".into())],
        body: b"{}".to_vec(),
    }])
    .await;
    let err = m.client().pull(&pr_ref()).await.unwrap_err();
    assert!(matches!(
        err,
        ferro_forge::ForgeError::RateLimited {
            retry_after_ms: 7000
        }
    ));
    assert_eq!(err.detail()["retryAfterMs"], 7000);
}

#[tokio::test]
async fn threads_paginate() {
    let page = |next: Option<&str>, ids: &[&str]| {
        let nodes: Vec<_> = ids
            .iter()
            .map(|id| {
                serde_json::json!({"id": id, "isResolved": false, "isOutdated": false, "path": "a.rs",
                    "line": 1, "diffSide": "RIGHT",
                    "comments": {"nodes": [{"databaseId": 11, "author": {"login": "ann"}, "body": "nit", "createdAt": "2026-01-01T00:00:00Z", "url": "https://x"}]}})
            })
            .collect();
        json(
            serde_json::json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
                "nodes": nodes,
                "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
            }}}}}),
        )
    };
    let m = Mock::start(vec![page(Some("c1"), &["t1"]), page(None, &["t2"])]).await;
    let threads = m.client().threads(&pr_ref()).await.unwrap();
    assert_eq!(
        threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        vec!["t1", "t2"]
    );
    assert_eq!(threads[0].comments[0].id, "11");
    assert_eq!(threads[0].side, "RIGHT");
}

#[tokio::test]
async fn submit_pins_commit_and_event() {
    let m = Mock::start(vec![json(
        serde_json::json!({"id": 99, "html_url": "https://x", "state": "COMMENTED"}),
    )])
    .await;
    let g = m.client();
    let r = g
        .submit_review(
            &pr_ref(),
            &"h".repeat(40),
            ferro_forge::github::ReviewEvent::RequestChanges,
            "look",
            &[ferro_forge::github::ReviewComment {
                path: "a.rs".into(),
                body: "fix".into(),
                line: Some(10),
                side: Some("RIGHT".into()),
                start_line: Some(8),
                start_side: Some("RIGHT".into()),
            }],
        )
        .await
        .unwrap();
    assert_eq!((r.id, r.state.as_str()), (99, "COMMENTED"));
    let raw = m.recorded().join("\n");
    assert!(raw.contains(&"h".repeat(40)), "{raw}");
    assert!(raw.contains("REQUEST_CHANGES"), "{raw}");
    assert!(raw.contains("start_line"), "{raw}");
}

#[tokio::test]
async fn checks_rollup() {
    for (runs, statuses, want) in [
        (
            serde_json::json!({"total_count": 1, "check_runs": [{"status": "completed", "conclusion": "failure", "details_url": "https://ci/1"}]}),
            serde_json::json!({}),
            "failure",
        ),
        (
            serde_json::json!({"total_count": 1, "check_runs": [{"status": "in_progress", "conclusion": null}]}),
            serde_json::json!({}),
            "pending",
        ),
        (
            serde_json::json!({"total_count": 1, "check_runs": [{"status": "completed", "conclusion": "success"}]}),
            serde_json::json!({}),
            "success",
        ),
        (
            serde_json::json!({"total_count": 0, "check_runs": []}),
            serde_json::json!({"total_count": 0, "statuses": []}),
            "none",
        ),
    ] {
        let m = Mock::start(vec![json(runs), json(statuses)]).await;
        let c = m.client().checks(&pr_ref(), &"a".repeat(40)).await.unwrap();
        assert_eq!(c.state, want);
    }
}
