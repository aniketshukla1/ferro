//! Recorded Anthropic SSE tests (B5, Appendix B): text, thinking,
//! split tool deltas, refusal, mid-stream errors, usage, UTF-8 splits.
//! A local mock speaks SSE; no network in CI.

use ferro_agent::provider_v2::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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
        std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        Anthropic::new("claude-opus-5".into(), format!("http://{}", self.addr)).unwrap()
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
    std::env::remove_var("ANTHROPIC_API_KEY");
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
    std::env::remove_var("ANTHROPIC_API_KEY");
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
    std::env::remove_var("ANTHROPIC_API_KEY");
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
    std::env::remove_var("ANTHROPIC_API_KEY");
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
    std::env::remove_var("ANTHROPIC_API_KEY");
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
    std::env::remove_var("ANTHROPIC_API_KEY");
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
    std::env::remove_var("ANTHROPIC_API_KEY");
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
    std::env::remove_var("ANTHROPIC_API_KEY");
}
