//! Language-server diagnostics (API.md § 4.9) against a fake server speaking LSP over stdio.

use ferro_server::lsp::LspManager;
use std::time::{Duration, Instant};

/// Answers initialize; on didOpen/didChange publishes one error per line containing "BAD";
/// asks the client for configuration first (the client must answer or servers stall). Like
/// rust-analyzer, the first open lands before its "workspace" loads: it publishes nothing for
/// it, reports itself quiescent, and only reopened or changed files get diagnostics.
const FAKE: &str = r#"#!/usr/bin/env python3
import json, sys
def read():
    n = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line: sys.exit(0)
        line = line.strip()
        if not line: break
        if line.lower().startswith(b"content-length:"): n = int(line.split(b":")[1])
    return json.loads(sys.stdin.buffer.read(n))
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b); sys.stdout.buffer.flush()
loaded = False
while True:
    m = read()
    meth = m.get("method")
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": 900, "method": "workspace/configuration", "params": {"items": [{"section": "rust-analyzer"}]}})
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {}}})
    elif meth in ("textDocument/didOpen", "textDocument/didChange"):
        td = m["params"]["textDocument"]
        if not loaded:
            loaded = True
            send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": td["uri"], "diagnostics": []}})
            send({"jsonrpc": "2.0", "method": "experimental/serverStatus", "params": {"health": "ok", "quiescent": True}})
            continue
        text = m["params"].get("contentChanges", [{}])[0].get("text") if meth.endswith("didChange") else td["text"]
        diags = [{"range": {"start": {"line": i, "character": l.index("BAD")}, "end": {"line": i, "character": l.index("BAD") + 3}},
                  "severity": 1, "message": "found BAD", "source": "fake", "code": "E1"}
                 for i, l in enumerate(text.split("\n")) if "BAD" in l]
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": td["uri"], "diagnostics": diags}})
"#;

fn fake_server_dir() -> tempfile::TempDir {
    let bin = tempfile::tempdir().unwrap();
    let p = bin.path().join("rust-analyzer");
    std::fs::write(&p, FAKE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

fn wait_for(m: &LspManager, want: usize) -> Vec<(String, Vec<ferro_server::lsp::Diagnostic>)> {
    let t0 = Instant::now();
    loop {
        let d = m.diagnostics();
        if d.iter().map(|(_, l)| l.len()).sum::<usize>() == want
            || t0.elapsed() > Duration::from_secs(10)
        {
            return d;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn open_file_gets_diagnostics_and_changes_update_them() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(
        repo.path().join("src/main.rs"),
        "fn main() {\n    let x = BAD;\n}\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("notes.md"), "BAD\n").unwrap();
    let bin = fake_server_dir();
    let m = LspManager::new(
        repo.path().to_path_buf(),
        true,
        ferro_server::bus::Events::new(),
    )
    .with_path_env(bin.path().to_string_lossy().into_owned());

    assert!(!m.open("notes.md"), "no server for Markdown");
    assert!(m.open("src/main.rs"));
    let d = wait_for(&m, 1);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].0, "src/main.rs");
    let e = &d[0].1[0];
    assert_eq!(
        (
            e.line,
            e.col,
            e.severity,
            e.message.as_str(),
            e.code.as_deref()
        ),
        (2, 13, "error", "found BAD", Some("E1"))
    );
    assert!(m
        .servers()
        .iter()
        .any(|s| s.language == "rust" && s.state == "ready"));

    // The file changes on disk: the server gets the new text, the error goes away.
    std::fs::write(repo.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    m.file_changed("src/main.rs");
    assert!(wait_for(&m, 0).is_empty());
}

#[test]
fn disabled_manager_starts_nothing() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("a.rs"), "BAD\n").unwrap();
    let bin = fake_server_dir();
    let m = LspManager::new(
        repo.path().to_path_buf(),
        false,
        ferro_server::bus::Events::new(),
    )
    .with_path_env(bin.path().to_string_lossy().into_owned());
    assert!(!m.open("a.rs"));
    assert!(m.servers().iter().all(|s| s.state != "ready"));
}
