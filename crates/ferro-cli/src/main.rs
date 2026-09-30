mod cli;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> AnyhowResult {
    // Parse first for output flags, then configure logging.
    // (clap handles --version/-V automatically from the crate version.)
    let mut cli = Cli::parse();
    // Restarted into an installed update: same port and token, so open browsers reconnect.
    if let Some(p) = cli.restart_state.take() {
        if let Some((token, port)) = ferro_server::update_check::take_restart_state(&p) {
            cli.token = Some(token);
            cli.port = port;
            cli.no_open = true;
            // The old process frees the port as it execs (on Windows, as it exits).
            for _ in 0..100 {
                if std::net::TcpListener::bind((cli.host.as_str(), port)).is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    if matches!(cli.command, Some(Commands::Mcp { .. })) {
        return run_mcp(cli).await;
    }
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else if cli.quiet {
        EnvFilter::new("warn")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(!cli.no_color)
        .init();

    if cli.update {
        return run_self_update().await;
    }

    match cli.command {
        Some(Commands::Ask {
            question,
            path,
            model,
            base_url,
            api_key,
            max_steps,
            allow_write,
        }) => {
            ask(
                question,
                path,
                model,
                base_url,
                api_key,
                max_steps,
                allow_write,
            )
            .await
        }
        Some(Commands::Open {
            ref target,
            ref view,
            no_serve,
            print,
        }) => {
            let (target, view) = (target.clone(), view.clone());
            open_cmd(cli, target, view, no_serve, print).await
        }
        Some(Commands::Gc) => gc().await,
        Some(Commands::Mcp { .. }) => unreachable!("handled above"),
        Some(Commands::Ssh { target, no_open }) => ferro_cli::ssh::run::run_ssh(&target, no_open)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() }),
        None => serve(cli, None).await,
    }
}

/// `ferro open`: reuse the server already serving the folder (browser at the file, line and
/// view), else start one like `ferro <target>` would.
async fn open_cmd(
    mut cli: Cli,
    target: Option<String>,
    view: Option<String>,
    no_serve: bool,
    print: bool,
) -> AnyhowResult {
    let target = target.unwrap_or_else(|| ".".into());
    let (root, initial) = launch_target(&target);
    // A diff needs a file; for a folder it means its changes.
    let view = match (view.as_deref(), &initial) {
        (Some("diff"), None) => Some("changes".to_string()),
        _ => view,
    };
    let dirs = ferro_core::dirs::FerroDirs::resolve();
    let key = dirs.workspace_key(&root);
    if let Some(inst) = ferro_cli::instances::find(&dirs.state_dir, &key).await {
        let (path, line) = match &initial {
            Some((p, l)) => (Some(p.as_str()), Some(*l)),
            None => (None, None),
        };
        let url = ferro_cli::instances::open_url(&inst, path, line, view.as_deref());
        if print {
            println!("{url}");
        } else {
            open::that(&url)?;
            println!(
                "ferro: opened in the server already running for {}",
                root.display()
            );
        }
        return Ok(());
    }
    if no_serve {
        eprintln!("ferro: no server is running for {}", root.display());
        std::process::exit(3);
    }
    cli.path = Some(PathBuf::from(target));
    cli.command = None;
    if print {
        cli.no_open = true;
    }
    serve(cli, view).await
}

/// `ferro mcp`: newline-delimited JSON-RPC on stdin/stdout; logs go to stderr only.
async fn run_mcp(cli: Cli) -> AnyhowResult {
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("warn")
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(!cli.no_color)
        .with_writer(std::io::stderr)
        .init();

    let mcp_path = match &cli.command {
        Some(Commands::Mcp { path }) => path.clone(),
        _ => None,
    };
    let root = match mcp_path.or(cli.path.clone()) {
        Some(p) => p.canonicalize().unwrap_or(p),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let dirs = ferro_core::dirs::FerroDirs::resolve();
    let st = ferro_server::server::build_state(
        root,
        dirs,
        ferro_server::Host::Cli,
        env!("CARGO_PKG_VERSION").to_string(),
    );
    {
        let ws = st.ws();
        ws.index.rebuild().await;
        ws.symbols.preload(&ws.index.file_index.load());
    }
    ferro_server::mcp::serve_stdio(st, env!("CARGO_PKG_VERSION").to_string()).await?;
    Ok(())
}

/// Remove worktrees of merged/closed PRs older than 7 days.
async fn gc() -> AnyhowResult {
    let dirs = ferro_core::dirs::FerroDirs::resolve();
    let removed = ferro_forge::checkout::gc_worktrees(&dirs.state_dir, 7, &|r| {
        let (token, _) = ferro_forge::resolve_token(&r.host)
            .map(|(t, s)| (Some(t), Some(s)))
            .unwrap_or((None, None));
        let gh = ferro_forge::GitHub::for_ref(r, token);
        // Sync callback from a multithreaded runtime: isolate the fetch on
        // a throwaway current-thread runtime.
        tokio::task::block_in_place(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
                .and_then(|rt| {
                    rt.block_on(async {
                        match gh.pull(r).await {
                            Ok(m) if m.merged => Some("merged".to_string()),
                            Ok(m) if m.state == "closed" => Some("closed".to_string()),
                            Ok(_) => Some("open".to_string()),
                            Err(_) => None,
                        }
                    })
                })
        })
    });
    if removed.is_empty() {
        println!("ferro gc: nothing to remove");
    } else {
        for d in &removed {
            println!("ferro gc: removed {}", d.display());
        }
    }
    Ok(())
}

async fn serve(cli: Cli, view: Option<String>) -> AnyhowResult {
    if cli.no_auth && cli.host != "127.0.0.1" && cli.host != "localhost" && cli.host != "::1" {
        return Err("--no-auth is only allowed on loopback binds".into());
    }
    // file:line launch: serve the repository root, open the file at the line.
    let mut initial: Option<(String, usize)> = None;
    let root = match cli.path.clone() {
        Some(p) => {
            // PR mode: `ferro https://github.com/owner/repo/pull/N`
            if let Some(raw) = p.to_str() {
                if ferro_forge::parse_pr_url(raw).is_some() {
                    return serve_pr(cli, raw).await;
                }
            }
            let s = p.to_string_lossy().to_string();
            match launch_target(&s) {
                (r, Some((rel, l))) => {
                    initial = Some((rel, l));
                    r
                }
                (r, None) => r,
            }
        }
        None => PathBuf::from(".")
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(".")),
    };

    let dirs = ferro_core::dirs::FerroDirs::resolve();
    let loaded_tls = resolve_tls(&dirs, &cli)?;
    let o = ferro_server::server::ServeOpts {
        host: cli.host.clone(),
        port: cli.port,
        no_git: cli.no_git,
        narrate: !cli.quiet,
        no_open: cli.no_open,
        initial,
        token: cli.token.or_else(|| std::env::var("FERRO_TOKEN").ok()),
        allow_hosts: allow_hosts(cli.allow_host),
        no_auth: cli.no_auth,
        dev_web: cli.dev_web.clone(),
        read_only: cli.read_only,
        tls: loaded_tls.config,
        tls_fingerprint: loaded_tls.fingerprint,
        base_path: cli.base_path.clone(),
        no_lsp: cli.no_lsp,
    };
    let https = o.tls.is_https();
    let state_dir = dirs.state_dir.clone();
    let key = dirs.workspace_key(&root);
    let h = ferro_server::server::serve(
        root.clone(),
        dirs,
        ferro_server::Host::Cli,
        env!("CARGO_PKG_VERSION").to_string(),
        o,
    )
    .await;
    // `ferro open` finds this server by its folder (plain HTTP on this machine only).
    let recorded = !https
        && ferro_cli::instances::record(
            &state_dir,
            &key,
            &ferro_cli::instances::Instance {
                pid: std::process::id(),
                root: root
                    .canonicalize()
                    .unwrap_or(root)
                    .to_string_lossy()
                    .into_owned(),
                base: url_root(&h.url),
                token: h.token.clone(),
            },
        )
        .is_ok();
    if !cli.no_open {
        let url = match &view {
            Some(v) => format!("{}&view={v}", h.url),
            None => h.url.clone(),
        };
        let _ = open::that(&url);
    }
    wait_shutdown(h).await;
    if recorded {
        ferro_cli::instances::forget(&state_dir, &key, std::process::id());
    }
    Ok(())
}

/// Ctrl+C, or a SIGTERM (`kill`, a closing terminal) on Unix: shut down cleanly either way.
async fn wait_shutdown(h: ferro_server::server::ServerHandle) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    let _ = h.shutdown.send(());
}

/// Split `path:line` (line = trailing :digits, file must exist or parent dir must).
fn split_file_line(s: &str) -> Option<(String, usize)> {
    let (head, tail) = s.rsplit_once(':')?;
    let line: usize = tail.parse().ok()?;
    if line == 0 {
        return None;
    }
    let p = std::path::Path::new(head);
    if p.is_file() || p.parent().map(|d| d.is_dir()).unwrap_or(false) {
        Some((head.to_string(), line))
    } else {
        None
    }
}

/// Resolve a CLI target to `(root, initial file:line)`.
/// Files serve from the git toplevel when inside a repo (D22); otherwise
/// from the cwd when inside it, else from the parent directory.
fn launch_target(s: &str) -> (PathBuf, Option<(String, usize)>) {
    let (file_part, line) = match split_file_line(s) {
        Some((f, l)) => (f, Some(l)),
        None => (s.to_string(), None),
    };
    let fp = PathBuf::from(&file_part);
    if fp.is_dir() {
        return (
            fp.canonicalize().unwrap_or_else(|_| PathBuf::from(".")),
            None,
        );
    }
    // Anchor: the file's directory if the file exists, else the cwd.
    let anchor = if fp.is_file() {
        fp.parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(".")
    };
    let toplevel = std::process::Command::new("git")
        .arg("-C")
        .arg(&anchor)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|t| !t.is_empty())
        .map(PathBuf::from);
    if let Some(top) = toplevel {
        let top_canon = top.canonicalize().unwrap_or(top);
        if let Some(rel) = fp.canonicalize().ok().and_then(|abs| {
            abs.strip_prefix(&top_canon)
                .ok()
                .map(|r| r.to_string_lossy().to_string())
        }) {
            if let Some(l) = line {
                return (top_canon, Some((rel, l)));
            }
            // A bare existing file inside a repo still serves the repo root.
            return (top_canon, None);
        }
    }
    if let (Ok(cwd), Ok(abs)) = (std::env::current_dir(), fp.canonicalize()) {
        if let Ok(cwd_canon) = cwd.canonicalize() {
            if let Ok(rel) = abs.strip_prefix(&cwd_canon) {
                return (
                    cwd_canon,
                    line.map(|l| (rel.to_string_lossy().to_string(), l)),
                );
            }
        }
    }
    // Fallback: parent directory, bare filename.
    let dir = fp
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let name = fp
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or(file_part);
    (
        dir.canonicalize().unwrap_or_else(|_| PathBuf::from(".")),
        line.map(|l| (name, l)),
    )
}

/// `ferro <pr-url>`: serve the cwd, then open the PR through the same
/// pr.open job the UI uses (persistent worktrees, no temp clones).
async fn serve_pr(cli: Cli, url: &str) -> AnyhowResult {
    let pr_ref = ferro_forge::parse_pr_url(url)
        .or_else(|| ferro_forge::parse_mr_url(url))
        .ok_or("not a GitHub PR or GitLab MR url")?;
    if !cli.yes {
        // Refuse already-merged PRs unless -y, via the API
        // rather than the gh CLI. Offline or unauthenticated: allow, the
        // server surfaces the real state.
        let (token, _) = match pr_ref.provider {
            ferro_forge::Provider::GitHub => ferro_forge::resolve_token(&pr_ref.host),
            ferro_forge::Provider::GitLab => ferro_forge::resolve_gitlab_token(&pr_ref.host),
        }
        .map(|(t, s)| (Some(t), Some(s)))
        .unwrap_or((None, None));
        if let Some(token) = token {
            let gh = ferro_forge::ForgeClient::for_ref(&pr_ref, Some(token));
            if let Ok(meta) = gh.pull(&pr_ref).await {
                if meta.merged {
                    return Err("PR already merged (use -y to open anyway)".into());
                }
            }
        }
    }
    let root = PathBuf::from(".")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));
    let dirs = ferro_core::dirs::FerroDirs::resolve();
    if cli.no_auth && cli.host != "127.0.0.1" && cli.host != "localhost" && cli.host != "::1" {
        return Err("--no-auth is only allowed on loopback binds".into());
    }
    let loaded_tls = resolve_tls(&dirs, &cli)?;
    let o = ferro_server::server::ServeOpts {
        host: cli.host.clone(),
        port: cli.port,
        no_git: cli.no_git,
        narrate: !cli.quiet,
        no_open: true, // opened below, after the PR job starts
        initial: None,
        token: cli.token.or_else(|| std::env::var("FERRO_TOKEN").ok()),
        allow_hosts: allow_hosts(cli.allow_host),
        no_auth: cli.no_auth,
        dev_web: cli.dev_web.clone(),
        read_only: cli.read_only,
        tls: loaded_tls.config,
        tls_fingerprint: loaded_tls.fingerprint,
        base_path: cli.base_path.clone(),
        no_lsp: cli.no_lsp,
    };
    let h = ferro_server::server::serve(
        root,
        dirs,
        ferro_server::Host::Cli,
        env!("CARGO_PKG_VERSION").to_string(),
        o,
    )
    .await;
    // Drive the same pr.open job the frontend uses, over loopback.
    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("{}/api/v1/workspace/open", url_root(&h.url)))
        .json(&serde_json::json!({ "prUrl": url }));
    if let Some(t) = token_of(&h.url) {
        req = req.bearer_auth(t);
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => eprintln!("ferro: opening PR {url} …"),
        Ok(r) => eprintln!("ferro: pr.open rejected: {}", r.status()),
        Err(e) => eprintln!("ferro: pr.open failed: {e}"),
    }
    if !cli.no_open {
        let _ = open::that(&h.url);
    }
    wait_shutdown(h).await;
    Ok(())
}

/// Base origin of a `http://host:port/?token=…` server URL.
fn url_root(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => base.trim_end_matches('/').to_string(),
        None => url.trim_end_matches('/').to_string(),
    }
}

/// Token embedded in the server URL query string, if any.
fn token_of(url: &str) -> Option<String> {
    url.split_once("token=").and_then(|(_, rest)| {
        rest.split('&')
            .next()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    })
}

async fn ask(
    question: String,
    path: Option<PathBuf>,
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    max_steps: usize,
    allow_write: bool,
) -> AnyhowResult {
    let root = path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));

    let provider = ferro_agent::OpenAiCompat::from_env(api_key, base_url, model).map_err(|e| {
        format!("provider: {e}\n  set GEMINI_API_KEY (or OPENAI_API_KEY / OLLAMA_MODEL)")
    })?;
    eprintln!("ferro ask [{}]", provider.label());

    let index = Arc::new(ferro_core::Index::new(root.clone()));
    index.rebuild().await;

    let mut sandbox = ferro_agent::Sandbox::readonly(root.clone());
    sandbox.allow_write = allow_write;

    let agent = ferro_agent::Agent {
        index,
        sandbox,
        client: Arc::new(provider),
        max_steps,
    };
    let t = agent.run(&question).await;
    let id = ferro_agent::new_id();
    match ferro_agent::log_ask(&root, &id, &question, &t, &[]) {
        Ok(p) => eprintln!("session: {}", p.display()),
        Err(e) => eprintln!("session log skipped: {e}"),
    }
    for (i, step) in t.steps.iter().enumerate() {
        if let Some(thought) = &step.thought {
            println!("— step {}: {thought}", i + 1);
        }
        for (call, result) in &step.calls {
            println!("  $ {} {}", call.name, call.args);
            for line in result.output.lines().take(12) {
                println!("    {line}");
            }
            if result.output.lines().count() > 12 || result.truncated {
                println!("    … (truncated)");
            }
            if !result.ok {
                println!("    ! tool error");
            }
        }
    }
    println!("\n{t}", t = t.final_text);
    Ok(())
}

type AnyhowResult = Result<(), Box<dyn std::error::Error>>;

fn resolve_tls(
    dirs: &ferro_core::dirs::FerroDirs,
    cli: &Cli,
) -> Result<ferro_server::tls::LoadedTls, Box<dyn std::error::Error>> {
    use ferro_server::tls::{self, TlsConfig};
    let self_signed = cli.tls.as_deref() == Some("self-signed");
    if self_signed {
        if cli.tls_cert.is_some() || cli.tls_key.is_some() {
            return Err("--tls self-signed cannot be combined with --tls-cert/--tls-key".into());
        }
        let (loaded, _, _) = tls::self_signed(&dirs.state_dir)?;
        return Ok(loaded);
    }
    if cli.tls.is_some() {
        return Err("--tls must be `self-signed` or omit it and use --tls-cert/--tls-key".into());
    }
    match (&cli.tls_cert, &cli.tls_key) {
        (Some(c), Some(k)) => Ok(tls::load_pem_files(c, k)?),
        (None, None) => Ok(ferro_server::tls::LoadedTls {
            config: TlsConfig::None,
            fingerprint: None,
        }),
        _ => Err("--tls-cert and --tls-key must be given together".into()),
    }
}

async fn run_self_update() -> AnyhowResult {
    use ferro_core::update::{
        apply_release_artifact, artifact_for_target, current_target, parse_manifest,
        require_https_url, MANIFEST_URL_ENV, MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES,
    };
    let url = std::env::var(MANIFEST_URL_ENV).map_err(|_| {
        format!("{MANIFEST_URL_ENV} is not set; the update host is a board decision")
    })?;
    require_https_url(&url).map_err(|e| e.to_string())?;
    let manifest_bytes =
        ferro_server::update_check::fetch_https(&url, MAX_MANIFEST_BYTES, 30).await?;
    let manifest = parse_manifest(&manifest_bytes).map_err(|e| e.to_string())?;
    let target = current_target();
    let art = artifact_for_target(&manifest, &target).map_err(|e| e.to_string())?;
    require_https_url(&art.url).map_err(|e| e.to_string())?;
    eprintln!(
        "ferro --update: downloading {target} archive for {} …",
        manifest.version
    );
    let archive = ferro_server::update_check::fetch_https(&art.url, MAX_ARCHIVE_BYTES, 120).await?;
    let exe = std::env::current_exe()?;
    apply_release_artifact(&exe, &target, &archive, &art.sha256).map_err(|e| e.to_string())?;
    println!("ferro updated to {} ({target})", manifest.version);
    Ok(())
}

/// CLI `--allow-host` (repeatable) merged with `FERRO_ALLOW_HOST` comma list.
fn allow_hosts(cli: Vec<String>) -> Vec<String> {
    let mut out = cli;
    if let Ok(env) = std::env::var("FERRO_ALLOW_HOST") {
        out.extend(
            env.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_file_line() {
        // CWD for bin tests is the package dir.
        assert_eq!(
            split_file_line("src/main.rs:42"),
            Some(("src/main.rs".into(), 42))
        );
        assert_eq!(split_file_line("src/main.rs"), None);
        assert_eq!(split_file_line("nope/nothing.rs:10"), None);
        assert_eq!(split_file_line("README.md:0"), None);
    }

    #[test]
    fn launch_prefers_git_toplevel() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        std::fs::create_dir_all(r.join("sub")).unwrap();
        std::fs::write(r.join("sub/f.rs"), "x\n").unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(r)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        let rel = format!("{}/sub/f.rs:3", r.display());
        let (root, initial) = launch_target(&rel);
        assert_eq!(root, r.canonicalize().unwrap());
        assert_eq!(initial, Some(("sub/f.rs".into(), 3)));
    }
}
