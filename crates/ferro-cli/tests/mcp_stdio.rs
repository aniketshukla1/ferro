//! `ferro mcp` stdio transport: stdout must be MCP JSON lines only.

use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn ferro_mcp_stdio_protocol_only_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("x.txt"), "hello\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ferro");
    let mut child = Command::new(bin)
        .arg("mcp")
        .arg("--path")
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"mcp-test","version":"1"}}}"#;
    stdin
        .write_all(format!("{init}\n").as_bytes())
        .await
        .unwrap();
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n")
        .await
        .unwrap();
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"ferro_read\",\"arguments\":{\"path\":\"x.txt\"}}}\n",
        )
        .await
        .unwrap();
    stdin.flush().await.unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut out_lines = Vec::new();
    for _ in 0..3 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), lines.next_line())
            .await
            .expect("timeout")
            .unwrap()
            .unwrap();
        assert!(
            line.starts_with('{'),
            "stdout must be JSON only, got: {line}"
        );
        serde_json::from_str::<serde_json::Value>(&line).expect("valid json line");
        out_lines.push(line);
    }
    let list: serde_json::Value = serde_json::from_str(&out_lines[1]).unwrap();
    assert!(list["result"]["tools"].as_array().unwrap().len() >= 13);
    let call: serde_json::Value = serde_json::from_str(&out_lines[2]).unwrap();
    assert_eq!(call["result"]["isError"], false);
    assert!(call["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("hello"));

    let _ = child.kill().await;
    let _ = child.wait().await;
}
