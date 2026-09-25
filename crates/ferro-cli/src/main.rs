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
    let cli = Cli::parse();
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
        Some(Commands::Gc) => gc().await,
        None => serve(cli).await,
    }
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

async fn serve(cli: Cli) -> AnyhowResult {
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
    };
    let h = ferro_server::server::serve(
        root,
        dirs,
        ferro_server::Host::Cli,
        env!("CARGO_PKG_VERSION").to_string(),
        o,
    )
    .await;
    if !cli.no_open {
        let _ = open::that(&h.url);
    }
    wait_shutdown(h).await;
    Ok(())
}

async fn wait_shutdown(h: ferro_server::server::ServerHandle) {
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
    // Fallback: parent directory, bare filename (px0 parity).
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
        // Refuse already-merged PRs unless -y (px0 parity), via the API
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
