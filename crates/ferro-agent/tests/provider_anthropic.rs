//! Recorded Anthropic SSE tests (B5, Appendix B): text, thinking,
//! split tool deltas, refusal, mid-stream errors, usage, UTF-8 splits.
//! A local mock speaks SSE; no network in CI.

use ferro_agent::provider_v2::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

static API_KEY_ENV: Mutex<()> = Mutex::new(());

struct Mock {
    addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    /// Each script is a full response body; `chunks` splits it into TCP
    /// writes to prove split-point safety.
    async fn start(scripts: Vec<(u16, String, usize)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        #[allow(clippy::type_complexity)]
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
                        let mut first = String::new();
                        if rd.read_line(&mut first).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let mut len = 0usize;
                        let mut raw = first.clone();
                        loop {
                            let mut line = String::new();
                            if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            raw.push_str(&line);
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
                        let mut body = vec![0u8; len];
                        if len > 0 && rd.read_exact(&mut body).await.is_err() {
                            return;
                        }
                        raw.push_str(&String::from_utf8_lossy(&body));
                        r3.lock().unwrap().push(raw);
                        let (status, text, chunks) =
                            s3.lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or((500, "no script".into(), 1));
                        let bytes = text.into_bytes();
                        let head = format!(
                            "HTTP/1.1 {} x\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                            status,
                            bytes.len()
                        );
                        if wh.write_all(head.as_bytes()).await.is_err() {
                            return;
                        }
                        let n = chunks.max(1);
                        let step = bytes.len().div_ceil(n);
                        for piece in bytes.chunks(step) {
                            if wh.write_all(piece).await.is_err() {
                                return;
                            }
                            tokio::task::yield_now().await;
                        }
                    }
                });
            }
        });
        Self { addr, requests }
    }

    fn client(&self) -> Anthropic {
        let _guard = API_KEY_ENV.lock().unwrap();
        let previous = std::env::var_os("ANTHROPIC_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        let client =
            Anthropic::new("claude-opus-5".into(), format!("http://{}", self.addr)).unwrap();
        match previous {
            Some(value) => std::env::set_var("ANTHROPIC_API_KEY", value),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        client
    }

    fn recorded(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn sse(events: &[&str]) -> String {
    events
        .iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect::<Vec<_>>()
        .join("")
}

fn req(model: &str) -> ChatReq<'static> {
    ChatReq {
        system: Box::leak("sys".to_string().into_boxed_str()),
        messages: Box::leak(
            vec![Msg {
                role: MsgRole::User,
                blocks: vec![MsgBlock::Text(model.to_string())],
                cache: false,
            }]
            .into_boxed_slice(),
        ),
        tools: Box::leak([].into()),
        max_tokens: 64000,
        effort: None,
        stop: tokio_util::sync::CancellationToken::new(),
    }
}

async fn run(client: &Anthropic, req: ChatReq<'_>) -> (TurnOutcome, Vec<LlmEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let out = client.stream_turn(req, tx).await.unwrap();
    let mut evs = Vec::new();
    while let Ok(e) = rx.try_recv() {
        evs.push(e);
    }
    (out, evs)
}

#[tokio::test]
async fn text_thinking_usage_and_cache_headers() {
    let body = sse(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":512,"cache_creation_input_tokens":100,"cache_read_input_tokens":400}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hi"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let m = Mock::start(vec![(200, body, 1)]).await;
    let (out, evs) = run(&m.client(), req("q")).await;
    assert_eq!(out.text, "hi");
    assert_eq!(out.thinking.len(), 1);
    assert_eq!(out.thinking[0].signature, "sig");
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(
        (
            out.usage.input,
            out.usage.output,
            out.usage.cache_read,
            out.usage.cache_write
        ),
        (512, 12, 400, 100)
    );
    assert!(evs.iter().any(|e| matches!(e, LlmEvent::Thinking(_))));
    // Prompt caching: system block carries a breakpoint; model/effort set.
    let raw = m.recorded().join("\n");
    assert!(raw.contains("cache_control"), "{raw}");
    assert!(raw.contains("claude-opus-5"), "{raw}");
    assert!(raw.contains("x-api-key"), "{raw}");
}

#[tokio::test]
async fn second_agent_turn_reports_cache_read_tokens() {
    let first = sse(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":600,"cache_creation_input_tokens":550,"cache_read_input_tokens":0}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"first answer"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let second = sse(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":80,"cache_creation_input_tokens":0,"cache_read_input_tokens":550}}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"second answer"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let m = Mock::start(vec![(200, first, 3), (200, second, 3)]).await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    let index = std::sync::Arc::new(ferro_core::Index::with_dirs(
        dir.path().to_path_buf(),
        ferro_core::dirs::FerroDirs::new(
            dir.path().join("home/c"),
            dir.path().join("home/s"),
            dir.path().join("home/h"),
        ),
    ));
    index
        .file_index
        .store(vec!["a.txt".into()], vec![6], vec![0]);

    let client: ferro_agent::ArcV2 = std::sync::Arc::new(m.client());
    let agent = ferro_agent::AgentV2::new(
        client,
        std::sync::Arc::new(ferro_agent::ToolCtx::new(index)),
    );
    let mut conv = ferro_agent::Conversation::new("c_cache");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let stop = tokio_util::sync::CancellationToken::new();

    let first_out = agent
        .run(&mut conv, "first question", None, &tx, &stop)
        .await
        .unwrap();
    assert_eq!(first_out.usage.cache_read, 0);
    let second_out = agent
        .run(&mut conv, "follow up", None, &tx, &stop)
        .await
        .unwrap();
    assert!(
        second_out.usage.cache_read > 0,
        "second turn usage: {:?}",
        second_out.usage
    );

    let requests = m.recorded();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("first question"), "{}", requests[1]);
    assert!(requests[1].contains("follow up"), "{}", requests[1]);
}

#[tokio::test]
async fn split_tool_delta_parses() {
    // Two complete SSE events carry halves of one tool argument. The literal
    // é exercises UTF-8 across the transport fragmentation below.
    let part1 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"caf"}}"#;
    let part2 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"é\"}"}}"#;
    let body = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"read_file"}}"#,
        part1,
        part2,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#,
    ]);
    // One-byte TCP writes fragment SSE framing as well as the UTF-8 bytes.
    let chunks = body.len();
    let m = Mock::start(vec![(200, body, chunks)]).await;
    let (out, _) = run(&m.client(), req("q")).await;
    assert_eq!(out.stop, StopReason::ToolUse);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0].name, "read_file");
    assert!(out.calls[0].input_ok);
    assert_eq!(out.calls[0].input["path"], "café");
}

#[tokio::test]
async fn invalid_tool_json_flagged() {
    let body = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"read_file"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{oops"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#,
    ]);
    let m = Mock::start(vec![(200, body, 1)]).await;
    let (out, _) = run(&m.client(), req("q")).await;
    assert!(!out.calls[0].input_ok);
    assert!(out.calls[0].input_raw.contains("oops"));
}

#[tokio::test]
async fn refusal_runs_no_tools() {
    let body = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"read_file"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal"}},"usage":{"output_tokens":3}}"#,
    ]);
    let m = Mock::start(vec![(200, body, 1)]).await;
    let (out, _) = run(&m.client(), req("q")).await;
    assert_eq!(out.stop, StopReason::Refusal);
    assert!(out.calls.is_empty());
}

#[tokio::test]
async fn overloaded_retries_then_succeeds() {
    let ok = sse(&[
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"back"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
    ]);
    let m = Mock::start(vec![
        (
            200,
            sse(&[r#"{"type":"error","error":{"type":"overloaded_error"}}"#]),
            1,
        ),
        (200, ok, 1),
    ])
    .await;
    let (out, _) = run(&m.client(), req("q")).await;
    assert_eq!(out.text, "back");
    assert_eq!(m.recorded().len(), 2);
}

#[tokio::test]
async fn bad_request_does_not_retry() {
    let m = Mock::start(vec![(400, "nope".into(), 1)]).await;
    let err = m
        .client()
        .stream_turn(req("q"), tokio::sync::mpsc::unbounded_channel().0)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ferro_agent::provider::ProviderError::BadResponse(_)
    ));
    assert_eq!(m.recorded().len(), 1);
}

#[tokio::test]
async fn assistant_blocks_preserve_turn_order() {
    let body = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"checking"}}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"t1","name":"read_file"}}"#,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\"}"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
    ]);
    let m = Mock::start(vec![(200, body, 1)]).await;
    let (out, _) = run(&m.client(), req("q")).await;
    assert_eq!(out.blocks.len(), 3);
    assert!(matches!(out.blocks[0], TurnBlock::Thinking(_)));
    assert!(matches!(out.blocks[1], TurnBlock::Text(_)));
    assert!(matches!(out.blocks[2], TurnBlock::ToolUse(_)));
    assert_eq!(out.calls.len(), 1);
}

#[tokio::test]
async fn max_tokens_runs_no_tools() {
    let body = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"read_file"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":64000}}"#,
    ]);
    let m = Mock::start(vec![(200, body, 1)]).await;
    let (out, _) = run(&m.client(), req("q")).await;
    assert_eq!(out.stop, StopReason::MaxTokens);
    assert!(out.calls.is_empty());
}

#[tokio::test]
async fn a_stream_cut_before_its_stop_is_not_a_turn() {
    // The connection drops mid-answer: no stop_reason ever arrives. That is
    // an interrupted turn (the UI already has the partial), not an answer.
    let cut = sse(&[
        r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The bug is"}}"#,
    ]);
    let m = Mock::start(vec![(200, cut, 1)]).await;
    let res = m
        .client()
        .stream_turn(req("q"), tokio::sync::mpsc::unbounded_channel().0)
        .await;
    assert!(res.is_err(), "{:?}", res.map(|o| o.text));
    // Cut before any output: retried transparently.
    let early = sse(&[r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#]);
    let ok = sse(&[
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"whole"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
    ]);
    let m = Mock::start(vec![(200, early, 1), (200, ok, 1)]).await;
    let (out, _) = run(&m.client(), req("q")).await;
    assert_eq!(out.text, "whole");
    std::env::remove_var("ANTHROPIC_API_KEY");
}

#[tokio::test]
async fn a_mid_output_fallback_drops_the_declined_partial() {
    // The declined model started a tool call; the fallback model answered.
    // Tool calls and thinking before the `fallback` block are not echoed or run.
    let body = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"declined"}}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t0","name":"read_file"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a\"}"}}"#,
        r#"{"type":"content_block_start","index":2,"content_block":{"type":"fallback","model":"claude-opus-4-8"}}"#,
        r#"{"type":"content_block_start","index":3,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":3,"delta":{"type":"text_delta","text":"answer"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
    ]);
    let m = Mock::start(vec![(200, body, 1)]).await;
    let (out, _) = run(&m.client(), req("q")).await;
    assert!(out.calls.is_empty(), "{:?}", out.calls);
    assert!(
        out.blocks
            .iter()
            .all(|b| !matches!(b, TurnBlock::ToolUse(_) | TurnBlock::Thinking(_))),
        "{:?}",
        out.blocks
    );
    assert!(matches!(out.blocks.last(), Some(TurnBlock::Text(t)) if t == "answer"));
    std::env::remove_var("ANTHROPIC_API_KEY");
}

#[tokio::test]
async fn fallbacks_stay_off_custom_endpoints() {
    // A custom ANTHROPIC_BASE_URL is often a gateway (Bedrock, Vertex, a
    // proxy) where the beta `fallbacks` field is a 400.
    let ok = sse(&[
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
    ]);
    let m = Mock::start(vec![(200, ok, 1)]).await;
    run(&m.client(), req("q")).await;
    let raw = m.recorded().join("\n");
    assert!(!raw.contains("\"fallbacks\""), "{raw}");
    assert!(!raw.contains("server-side-fallback"), "{raw}");
    std::env::remove_var("ANTHROPIC_API_KEY");
}

/// Recorded review run (B5 acceptance): the mock speaks a full review turn
/// with three `report_finding` tool calls (one split across SSE events),
/// then a final turn. The agent loop + validation pipeline must surface ≥ 2
/// findings. No network in CI.
#[tokio::test]
async fn recorded_review_run_finds_seeded_bugs() {
    use std::collections::{HashMap, HashSet};

    fn finding_delta(index: u64, id: &str, line: u64, title: &str) -> Vec<String> {
        let arg = format!(
            "{{\"path\":\"a.txt\",\"line\":{line},\"side\":\"RIGHT\",\"severity\":\"high\",\"category\":\"bug\",\"title\":\"{title}\",\"body\":\"detail\",\"confidence\":0.8}}"
        );
        // Split the JSON mid-argument to prove delta accumulation.
        let mid = arg.len() / 2;
        vec![
            format!(
                "{{\"type\":\"content_block_start\",\"index\":{index},\"content_block\":{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"report_finding\"}}}}"
            ),
            format!(
                "{{\"type\":\"content_block_delta\",\"index\":{index},\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{}}}}}",
                serde_json::to_string(&arg[..mid]).unwrap()
            ),
            format!(
                "{{\"type\":\"content_block_delta\",\"index\":{index},\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{}}}}}",
                serde_json::to_string(&arg[mid..]).unwrap()
            ),
        ]
    }

    let mut turn1: Vec<&str> = Vec::new();
    let s0 = finding_delta(0, "r1", 1, "bug one");
    let s1 = finding_delta(1, "r2", 1, "bug two");
    let s2 = finding_delta(2, "r3", 50, "bug three");
    let owned: Vec<String> = s0.into_iter().chain(s1).chain(s2).collect();
    let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    turn1.extend(refs);
    turn1.push(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":40}}"#);
    let turn2 = sse(&[
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"reviewed"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#,
    ]);
    let m = Mock::start(vec![(200, sse(&turn1), 7), (200, turn2, 1)]).await;

    // Workspace with a one-line file under review.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    let index = std::sync::Arc::new(ferro_core::Index::with_dirs(
        dir.path().to_path_buf(),
        ferro_core::dirs::FerroDirs::new(
            dir.path().join("home/c"),
            dir.path().join("home/s"),
            dir.path().join("home/h"),
        ),
    ));
    index
        .file_index
        .store(vec!["a.txt".into()], vec![6], vec![0]);
    let ctx = std::sync::Arc::new(ferro_agent::ToolCtx::new(index));

    let client: ferro_agent::ArcV2 = std::sync::Arc::new(m.client());
    let mut agent = ferro_agent::AgentV2::new(client, ctx);
    agent.max_steps = 4;
    agent.tools_override = Some(ferro_agent::review_tool_schemas());
    let mut conv = ferro_agent::Conversation::new("c_recorded");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let stop = tokio_util::sync::CancellationToken::new();
    let outcome = agent
        .run(&mut conv, "review a.txt", None, &tx, &stop)
        .await
        .unwrap();

    let mut raws = Vec::new();
    for step in &outcome.steps {
        for call in &step.calls {
            if call.name == ferro_agent::REPORT_FINDING_TOOL && call.result.ok {
                raws.push(ferro_agent::parse_report(&call.args).unwrap());
            }
        }
    }
    assert_eq!(raws.len(), 3, "{raws:?}");
    let mut changed = HashMap::new();
    changed.insert("a.txt".into(), (HashSet::from([1]), HashSet::new()));
    let snapped: Vec<_> = raws
        .iter()
        .filter_map(|f| ferro_agent::snap_to_diff(f, &changed))
        .collect();
    let finals = ferro_agent::dedupe(snapped);
    assert!(finals.len() >= 2, "{finals:?}");
    // The second request carried the redacted tool results back to the model.
    let bodies = m.recorded().join("\n");
    assert!(bodies.contains("report_finding"), "{bodies}");
}

#[tokio::test]
async fn captured_review_requests_exclude_env_contents_and_diff_secrets() {
    const ENV_SECRET: &str = "fixture-env-private-payload-12345";
    const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

    let tool_turn = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"env1","name":"read_file"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\".env\"}"}}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"diff1","name":"git_diff"}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"src/main.rs\",\"base\":\"HEAD\",\"target\":\"worktree\"}"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":8}}"#,
    ]);
    let final_turn = sse(&[
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"reviewed"}}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
    ]);
    let m = Mock::start(vec![(200, tool_turn, 5), (200, final_turn, 2)]).await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join(".env"),
        format!("DATABASE_PASSWORD={ENV_SECRET}\n"),
    )
    .unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", ".env", "src/main.rs"]);
    git(&[
        "-c",
        "user.name=Ferro Test",
        "-c",
        "user.email=ferro@example.invalid",
        "commit",
        "-qm",
        "fixture base",
    ]);
    std::fs::write(
        dir.path().join("src/main.rs"),
        format!("fn main() {{\n    let key = \"{AWS_KEY}\";\n}}\n"),
    )
    .unwrap();

    let index = std::sync::Arc::new(ferro_core::Index::with_dirs(
        dir.path().to_path_buf(),
        ferro_core::dirs::FerroDirs::new(
            dir.path().join("home/c"),
            dir.path().join("home/s"),
            dir.path().join("home/h"),
        ),
    ));
    index.file_index.store(
        vec![".env".into(), "src/main.rs".into()],
        vec![ENV_SECRET.len() as u64, 50],
        vec![0, 0],
    );
    let client: ferro_agent::ArcV2 = std::sync::Arc::new(m.client());
    let mut agent = ferro_agent::AgentV2::new(
        client,
        std::sync::Arc::new(ferro_agent::ToolCtx::new(index)),
    );
    agent.max_steps = 3;
    agent.tools_override = Some(ferro_agent::review_tool_schemas());
    let mut conv = ferro_agent::Conversation::new("c_secret_capture");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let stop = tokio_util::sync::CancellationToken::new();
    let outcome = agent
        .run(&mut conv, "review the fixture changes", None, &tx, &stop)
        .await
        .unwrap();
    assert_eq!(outcome.text, "reviewed");

    let requests = m.recorded();
    assert_eq!(requests.len(), 2);
    let bodies = requests.join("\n");
    assert!(bodies.contains("refused: never-send path"), "{bodies}");
    assert!(bodies.contains("[REDACTED]"), "{bodies}");
    assert!(!bodies.contains(ENV_SECRET), "{bodies}");
    assert!(!bodies.contains(AWS_KEY), "{bodies}");
}
