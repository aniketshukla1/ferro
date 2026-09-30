//! OpenAI-compatible adapter (B5): text streams live, and a `length` finish
//! is a truncated turn. A local server speaks chunked SSE; no network.

use ferro_agent::provider_v2::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// One-shot chunked SSE server: sends `first`, waits for `release`, then
/// sends `rest` and ends the response.
async fn gated_server(
    first: &'static str,
    rest: &'static str,
    release: tokio::sync::oneshot::Receiver<()>,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (rh, mut wh) = sock.into_split();
        let mut rd = tokio::io::BufReader::new(rh);
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
                len = line[15..].trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len];
        let _ = rd.read_exact(&mut body).await;
        let chunk = |s: &str| format!("{:x}\r\n{s}\r\n", s.len());
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
        wh.write_all(head.as_bytes()).await.unwrap();
        wh.write_all(chunk(first).as_bytes()).await.unwrap();
        wh.flush().await.unwrap();
        let _ = release.await;
        wh.write_all(chunk(rest).as_bytes()).await.unwrap();
        wh.write_all(b"0\r\n\r\n").await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn text_streams_before_the_turn_ends_and_length_is_max_tokens() {
    let (release, gate) = tokio::sync::oneshot::channel();
    let url = gated_server(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n\
         data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n\
         data: [DONE]\n\n",
        gate,
    )
    .await;
    let client = CompatV2::new(ProviderKind::Compat, url, "k".into(), "m".into());
    let messages = vec![Msg {
        role: MsgRole::User,
        blocks: vec![MsgBlock::Text("q".into())],
        cache: false,
    }];
    let req = ChatReq {
        system: "s",
        messages: &messages,
        tools: &[],
        max_tokens: 100,
        effort: None,
        stop: tokio_util::sync::CancellationToken::new(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let turn = client.stream_turn(req, tx);
    tokio::pin!(turn);
    // The server holds the rest of the answer until the first words reach us.
    let first = tokio::select! {
        e = rx.recv() => e,
        _ = &mut turn => panic!("the turn ended while the server was still holding it"),
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
            panic!("no text before the turn ended: the adapter buffers the stream")
        }
    };
    assert!(
        matches!(first, Some(LlmEvent::Text(ref t)) if t == "Hel"),
        "{first:?}"
    );
    release.send(()).unwrap();
    let out = turn.await.unwrap();
    assert_eq!(out.text, "Hello");
    assert_eq!(out.stop, StopReason::MaxTokens);
}
