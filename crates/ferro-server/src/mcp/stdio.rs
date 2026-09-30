//! MCP over newline-delimited JSON on stdin/stdout (no logs on stdout).

use std::sync::Arc;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::state::AppState;

use super::protocol;

/// Same cap as `POST /mcp`. A longer line is rejected and discarded so the
/// next frame stays aligned; the bytes are never echoed.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Read one JSON-RPC message per line; write one response line per request id.
pub async fn serve_stdio(s: Arc<AppState>, version: String) -> std::io::Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    loop {
        match read_frame(&mut lines, MAX_FRAME_BYTES).await? {
            None => break,
            Some(Err(())) => {
                write_line(
                    &mut stdout,
                    &serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32700, "message": "frame over 1 MiB" }
                    }),
                )
                .await?;
            }
            Some(Ok(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let msg: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": { "code": -32700, "message": format!("parse error: {e}") }
                        });
                        write_line(&mut stdout, &err).await?;
                        continue;
                    }
                };
                if let Some(resp) = protocol::handle_message(&s, msg, &version).await {
                    write_line(&mut stdout, &resp).await?;
                }
            }
        }
    }
    Ok(())
}

async fn write_line<W: AsyncWrite + Unpin>(
    stdout: &mut W,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let out = match serde_json::to_string(value) {
        Ok(s) => format!("{s}\n"),
        Err(_) => concat!(
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"response encode failed"}}"#,
            "\n"
        )
        .to_string(),
    };
    stdout.write_all(out.as_bytes()).await?;
    stdout.flush().await
}

/// One line without the trailing newline, or `Err(())` when it exceeds `max`
/// (the remainder of that line is discarded). `Ok(None)` is EOF.
pub(crate) async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<Option<Result<String, ()>>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut overflow = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buf.is_empty() && !overflow {
                return Ok(None);
            }
            break;
        }
        if let Some(i) = available.iter().position(|b| *b == b'\n') {
            if !overflow {
                let take = &available[..i];
                if buf.len().saturating_add(take.len()) > max {
                    overflow = true;
                } else {
                    buf.extend_from_slice(take);
                }
            }
            reader.consume(i + 1);
            break;
        }
        if !overflow {
            if buf.len().saturating_add(available.len()) > max {
                overflow = true;
                buf.clear();
            } else {
                buf.extend_from_slice(available);
            }
        }
        let n = available.len();
        reader.consume(n);
    }
    if overflow {
        return Ok(Some(Err(())));
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Some(Ok(String::from_utf8_lossy(&buf).into_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn frame_over_limit_is_rejected_and_stream_stays_aligned() {
        let raw = b"abcdefghij\n{\"a\":1}\n";
        let mut reader = BufReader::new(&raw[..]);
        let first = read_frame(&mut reader, 8).await.unwrap();
        assert!(matches!(first, Some(Err(()))));
        let second = read_frame(&mut reader, 8).await.unwrap().unwrap().unwrap();
        assert_eq!(second, "{\"a\":1}");
        assert!(read_frame(&mut reader, 8).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn short_frame_round_trips() {
        let raw = b"{\"a\":1}\r\n";
        let mut reader = BufReader::new(&raw[..]);
        let line = read_frame(&mut reader, 1024)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(line, "{\"a\":1}");
        assert!(read_frame(&mut reader, 1024).await.unwrap().is_none());
    }
}
