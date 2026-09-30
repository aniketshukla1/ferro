//! Language-server diagnostics for the Problems pane. API.md § 4.9.
//!
//! One server per language, found on PATH (never installed), started when a file of that
//! language is opened. ferro only asks for diagnostics: files are opened with their text, the
//! server pushes `textDocument/publishDiagnostics`, and the latest set per file is kept here.
//! Servers run with project code execution off where they support it (rust-analyzer: no build
//! scripts, proc macros or `cargo check`), and not at all in PR checkouts unless enabled.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde::Serialize;

use crate::bus::{Events, ServerEvent};

/// Files larger than this are not sent to a server.
const MAX_OPEN_BYTES: u64 = 2 * 1024 * 1024;

struct Spec {
    language: &'static str,
    /// LSP languageId per extension.
    exts: &'static [(&'static str, &'static str)],
    /// Candidate commands, first found on PATH wins.
    commands: &'static [&'static [&'static str]],
}

const SPECS: &[Spec] = &[
    Spec {
        language: "rust",
        exts: &[("rs", "rust")],
        commands: &[&["rust-analyzer"]],
    },
    Spec {
        language: "go",
        exts: &[("go", "go")],
        commands: &[&["gopls"]],
    },
    Spec {
        language: "typescript",
        exts: &[
            ("ts", "typescript"),
            ("tsx", "typescriptreact"),
            ("mts", "typescript"),
            ("cts", "typescript"),
            ("js", "javascript"),
            ("jsx", "javascriptreact"),
            ("mjs", "javascript"),
            ("cjs", "javascript"),
        ],
        commands: &[&["typescript-language-server", "--stdio"]],
    },
    Spec {
        language: "python",
        exts: &[("py", "python"), ("pyi", "python")],
        commands: &[
            &["pyright-langserver", "--stdio"],
            &["basedpyright-langserver", "--stdio"],
            &["pylsp"],
        ],
    },
    Spec {
        language: "c",
        exts: &[
            ("c", "c"),
            ("h", "c"),
            ("cc", "cpp"),
            ("cpp", "cpp"),
            ("cxx", "cpp"),
            ("hpp", "cpp"),
            ("hh", "cpp"),
            ("hxx", "cpp"),
        ],
        commands: &[&["clangd", "--background-index=false"]],
    },
];

fn spec_for(path: &str) -> Option<(&'static Spec, &'static str)> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    SPECS.iter().find_map(|s| {
        s.exts
            .iter()
            .find(|(e, _)| *e == ext)
            .map(|(_, id)| (s, *id))
    })
}

/// First candidate whose program is on `path_env`.
fn find_command(spec: &Spec, path_env: &str) -> Option<Vec<String>> {
    for cand in spec.commands {
        for dir in std::env::split_paths(path_env) {
            let full = dir.join(cand[0]);
            if full.is_file() {
                let mut argv = vec![full.to_string_lossy().into_owned()];
                argv.extend(cand[1..].iter().map(|s| s.to_string()));
                return Some(argv);
            }
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub line: u32,
    pub col: u32,
    #[serde(rename = "endLine")]
    pub end_line: u32,
    #[serde(rename = "endCol")]
    pub end_col: u32,
    /// error | warning | info | hint
    pub severity: &'static str,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub language: &'static str,
    pub command: Option<String>,
    /// missing | starting | ready | failed
    pub state: String,
}

struct Server {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, mpsc::Sender<serde_json::Value>>>,
    opened: Mutex<HashMap<String, i64>>, // rel path -> version
    state: RwLock<String>,
    /// Open files were re-sent after the server first reported itself quiescent.
    resynced: AtomicBool,
}

impl Server {
    fn send(&self, msg: &serde_json::Value) -> std::io::Result<()> {
        let body = msg.to_string();
        let mut w = self.stdin.lock();
        write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        w.flush()
    }
    fn notify(&self, method: &str, params: serde_json::Value) -> std::io::Result<()> {
        self.send(&serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }
    fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().insert(id, tx);
        self.send(
            &serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )
        .ok()?;
        let out = rx.recv_timeout(timeout).ok();
        self.pending.lock().remove(&id);
        out
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.lock().kill();
    }
}

pub struct LspManager {
    root: PathBuf,
    enabled: bool,
    path_env: String,
    bus: Events,
    servers: Mutex<HashMap<&'static str, Arc<Server>>>,
    failed: Mutex<HashSet<&'static str>>,
    diags: Arc<RwLock<HashMap<String, Vec<Diagnostic>>>>,
}

impl LspManager {
    pub fn new(root: PathBuf, enabled: bool, bus: Events) -> Self {
        Self {
            root,
            enabled,
            path_env: std::env::var("PATH").unwrap_or_default(),
            bus,
            servers: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashSet::new()),
            diags: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Tests point discovery at a fake server.
    pub fn with_path_env(mut self, path_env: String) -> Self {
        self.path_env = path_env;
        self
    }

    pub fn servers(&self) -> Vec<ServerInfo> {
        let running = self.servers.lock();
        let failed = self.failed.lock();
        SPECS
            .iter()
            .map(|s| {
                let command = find_command(s, &self.path_env).map(|a| a.join(" "));
                let state = if let Some(srv) = running.get(s.language) {
                    srv.state.read().clone()
                } else if failed.contains(s.language) {
                    "failed".into()
                } else if command.is_some() {
                    "idle".into()
                } else {
                    "missing".into()
                };
                ServerInfo {
                    language: s.language,
                    command,
                    state,
                }
            })
            .collect()
    }

    pub fn diagnostics(&self) -> Vec<(String, Vec<Diagnostic>)> {
        let mut out: Vec<_> = self
            .diags
            .read()
            .iter()
            .filter(|(_, d)| !d.is_empty())
            .map(|(p, d)| (p.clone(), d.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Open `rel` in its language's server (starting it on first use). Blocking: call off the
    /// async runtime. Returns false when no server handles the file.
    pub fn open(&self, rel: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let Some((spec, language_id)) = spec_for(rel) else {
            return false;
        };
        let Some(srv) = self.server(spec) else {
            return false;
        };
        let full = self.root.join(rel);
        if std::fs::metadata(&full)
            .map(|m| m.len() > MAX_OPEN_BYTES)
            .unwrap_or(true)
        {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(&full) else {
            return false;
        };
        let uri = file_uri(&full);
        let mut opened = srv.opened.lock();
        match opened.get_mut(rel) {
            Some(version) => {
                *version += 1;
                let _ = srv.notify(
                    "textDocument/didChange",
                    serde_json::json!({
                        "textDocument": { "uri": uri, "version": *version },
                        "contentChanges": [{ "text": text }],
                    }),
                );
            }
            None => {
                opened.insert(rel.to_string(), 1);
                let _ = srv.notify("textDocument/didOpen", serde_json::json!({
                    "textDocument": { "uri": uri, "languageId": language_id, "version": 1, "text": text },
                }));
            }
        }
        true
    }

    /// A file changed on disk: resend it when a server has it open; forget it when deleted.
    pub fn file_changed(&self, rel: &str) {
        let Some((spec, _)) = spec_for(rel) else {
            return;
        };
        let srv = self.servers.lock().get(spec.language).cloned();
        let Some(srv) = srv else { return };
        if !srv.opened.lock().contains_key(rel) {
            return;
        }
        if self.root.join(rel).is_file() {
            self.open(rel);
        } else {
            srv.opened.lock().remove(rel);
            let _ = srv.notify(
                "textDocument/didClose",
                serde_json::json!({ "textDocument": { "uri": file_uri(&self.root.join(rel)) } }),
            );
            if self.diags.write().remove(rel).is_some() {
                self.bus.publish(ServerEvent::Diagnostics {
                    path: rel.to_string(),
                    count: 0,
                });
            }
        }
    }

    fn server(&self, spec: &'static Spec) -> Option<Arc<Server>> {
        if let Some(s) = self.servers.lock().get(spec.language) {
            return Some(s.clone());
        }
        if self.failed.lock().contains(spec.language) {
            return None;
        }
        let argv = find_command(spec, &self.path_env)?;
        match self.start(spec, &argv) {
            Some(s) => {
                self.servers.lock().insert(spec.language, s.clone());
                Some(s)
            }
            None => {
                tracing::warn!(
                    "{} language server did not start: {}",
                    spec.language,
                    argv.join(" ")
                );
                self.failed.lock().insert(spec.language);
                None
            }
        }
    }

    fn start(&self, spec: &'static Spec, argv: &[String]) -> Option<Arc<Server>> {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let srv = Arc::new(Server {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            next_id: AtomicI64::new(1),
            pending: Mutex::new(HashMap::new()),
            opened: Mutex::new(HashMap::new()),
            state: RwLock::new("starting".into()),
            resynced: AtomicBool::new(false),
        });
        let reader = Arc::downgrade(&srv);
        let (root, diags, bus, language) = (
            self.root.clone(),
            self.diags.clone(),
            self.bus.clone(),
            spec.language,
        );
        std::thread::Builder::new()
            .name(format!("ferro-lsp-{language}"))
            .spawn(move || read_loop(stdout, reader, &root, &diags, &bus))
            .ok()?;
        let root_uri = file_uri(&self.root);
        let init = srv.request(
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": self.root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default() }],
                "capabilities": {
                    "textDocument": { "publishDiagnostics": { "relatedInformation": false }, "synchronization": { "didSave": false } },
                    "workspace": { "configuration": true, "workspaceFolders": true },
                    // rust-analyzer: report when the workspace has loaded (see `reopen_all`).
                    "experimental": { "serverStatusNotification": true },
                },
                "initializationOptions": server_settings(language),
            }),
            Duration::from_secs(30),
        )?;
        if init.get("error").is_some() {
            return None;
        }
        srv.notify("initialized", serde_json::json!({})).ok()?;
        *srv.state.write() = "ready".into();
        Some(srv)
    }
}

/// Settings that keep a server from running project code (answered to `workspace/configuration`
/// too). rust-analyzer: no build scripts, proc macros or `cargo check`.
fn server_settings(language: &str) -> serde_json::Value {
    match language {
        "rust" => serde_json::json!({
            "checkOnSave": false,
            "cargo": { "buildScripts": { "enable": false } },
            "procMacro": { "enable": false },
        }),
        _ => serde_json::json!({}),
    }
}

fn file_uri(p: &Path) -> String {
    let mut out = String::from("file://");
    for b in p.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn uri_to_rel(root: &Path, uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(raw.len());
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            bytes.push(b[i]);
            i += 1;
        }
    }
    let path = PathBuf::from(String::from_utf8(bytes).ok()?);
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let path = path.canonicalize().unwrap_or(path);
    path.strip_prefix(&root)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

fn read_loop(
    stdout: std::process::ChildStdout,
    srv: std::sync::Weak<Server>,
    root: &Path,
    diags: &RwLock<HashMap<String, Vec<Diagnostic>>>,
    bus: &Events,
) {
    let mut r = BufReader::new(stdout);
    loop {
        let mut len = 0usize;
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some(v) = l.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        if len == 0 || len > 64 * 1024 * 1024 {
            return;
        }
        let mut body = vec![0u8; len];
        if r.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(msg) = serde_json::from_slice::<serde_json::Value>(&body) else {
            continue;
        };
        let Some(srv) = srv.upgrade() else { return };
        let method = msg.get("method").and_then(|m| m.as_str());
        match (msg.get("id"), method) {
            // Response to one of our requests.
            (Some(id), None) => {
                if let Some(tx) = id.as_i64().and_then(|id| srv.pending.lock().remove(&id)) {
                    let _ = tx.send(
                        msg.get("result")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({ "error": msg.get("error") })),
                    );
                }
            }
            // A request from the server: answer so it never waits on us.
            (Some(id), Some(m)) => {
                let result = if m == "workspace/configuration" {
                    let items = msg["params"]["items"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    serde_json::Value::Array(
                        items
                            .iter()
                            .map(|it| match it["section"].as_str() {
                                Some(sec) if sec.starts_with("rust-analyzer") => {
                                    server_settings("rust")
                                }
                                _ => serde_json::Value::Null,
                            })
                            .collect(),
                    )
                } else {
                    serde_json::Value::Null
                };
                let _ =
                    srv.send(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            }
            // rust-analyzer answers files opened before its workspace loaded with empty
            // diagnostics and keeps them until the text changes: reopen them once it is ready.
            (None, Some("experimental/serverStatus")) => {
                if msg["params"]["quiescent"].as_bool() == Some(true)
                    && !srv.resynced.swap(true, Ordering::Relaxed)
                {
                    let root = root.to_path_buf();
                    let _ = std::thread::Builder::new()
                        .name("ferro-lsp-reopen".into())
                        .spawn(move || reopen_all(&srv, &root));
                }
            }
            (None, Some("textDocument/publishDiagnostics")) => {
                let p = &msg["params"];
                let Some(rel) = p["uri"].as_str().and_then(|u| uri_to_rel(root, u)) else {
                    continue;
                };
                let list: Vec<Diagnostic> = p["diagnostics"]
                    .as_array()
                    .map(|a| a.iter().filter_map(parse_diag).collect())
                    .unwrap_or_default();
                let count = list.len();
                let prev = diags
                    .write()
                    .insert(rel.clone(), list)
                    .map(|d| d.len())
                    .unwrap_or(0);
                if count > 0 || prev > 0 {
                    bus.publish(ServerEvent::Diagnostics { path: rel, count });
                }
            }
            _ => {}
        }
    }
}

/// Close and reopen every open file with its current text (a new version).
fn reopen_all(srv: &Server, root: &Path) {
    let mut opened = srv.opened.lock();
    for (rel, version) in opened.iter_mut() {
        let full = root.join(rel);
        let (Ok(text), Some((_, language_id))) = (std::fs::read_to_string(&full), spec_for(rel))
        else {
            continue;
        };
        let uri = file_uri(&full);
        *version += 1;
        let _ = srv.notify(
            "textDocument/didClose",
            serde_json::json!({ "textDocument": { "uri": uri } }),
        );
        let _ = srv.notify("textDocument/didOpen", serde_json::json!({
            "textDocument": { "uri": uri, "languageId": language_id, "version": *version, "text": text },
        }));
    }
}

fn parse_diag(d: &serde_json::Value) -> Option<Diagnostic> {
    let r = &d["range"];
    Some(Diagnostic {
        line: r["start"]["line"].as_u64()? as u32 + 1,
        col: r["start"]["character"].as_u64()? as u32 + 1,
        end_line: r["end"]["line"].as_u64().unwrap_or(0) as u32 + 1,
        end_col: r["end"]["character"].as_u64().unwrap_or(0) as u32 + 1,
        severity: match d["severity"].as_u64() {
            Some(1) => "error",
            Some(2) => "warning",
            Some(3) => "info",
            _ => "hint",
        },
        message: d["message"].as_str()?.chars().take(2_000).collect(),
        source: d["source"].as_str().map(str::to_string),
        code: match &d["code"] {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_with_spaces_and_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("src dir").join("mañana.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();
        let uri = file_uri(&file);
        assert!(uri.contains("src%20dir"), "{uri}");
        assert_eq!(
            uri_to_rel(&root, &uri).as_deref(),
            Some("src dir/mañana.rs")
        );
        assert_eq!(
            uri_to_rel(&root, "file:///etc/passwd"),
            None,
            "outside the root"
        );
    }

    #[test]
    fn languages_by_extension() {
        assert_eq!(spec_for("a/b.rs").map(|s| s.0.language), Some("rust"));
        assert_eq!(spec_for("x.tsx").map(|s| s.1), Some("typescriptreact"));
        assert_eq!(spec_for("README.md").map(|s| s.0.language), None);
    }
}
