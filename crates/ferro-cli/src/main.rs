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
        None => serve(cli).await,
    }
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
                if let Some(info) = ferro_core::pr::parse_pr_url(raw) {
                    return serve_pr(cli, info).await;
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

async fn serve_pr(cli: Cli, info: ferro_core::pr::PrInfo) -> AnyhowResult {
    if !cli.yes {
        // Refuse already-merged PRs unless -y (px0 parity).
        let state_out = std::process::Command::new("gh")
            .args([
                "pr",
                "view",
                &info.number.to_string(),
                "--repo",
                &format!("{}/{}", info.owner, info.repo),
                "--json",
                "state",
                "--jq",
                ".state",
            ])
            .output();
        if let Ok(o) = state_out {
            if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "MERGED" {
                return Err("PR already merged (use -y to open anyway)".into());
            }
        }
    }
    eprintln!(
        "ferro: fetching PR #{} {}/{} …",
        info.number, info.owner, info.repo
    );
    let work = tokio::task::spawn_blocking(move || ferro_core::pr::worktree_for_pr(&info))
        .await
        .map_err(|e| format!("pr fetch task: {e}"))?
        .map_err(|e| format!("pr fetch: {e}"))?;
    eprintln!(
        "ferro: PR #{} head {} base {} ({})",
        work.info.number,
        &work.head_sha[..8.min(work.head_sha.len())],
        work.base_ref,
        &work.base_sha[..8.min(work.base_sha.len())]
    );
    let dirs = ferro_core::dirs::FerroDirs::resolve();
    let state = ferro_server::server::build_state(
        work.dir.clone(),
        dirs,
        ferro_server::Host::Cli,
        env!("CARGO_PKG_VERSION").to_string(),
    );
    state.ws().index.set_pr(ferro_core::pr::PrCtx {
        owner: work.info.owner.clone(),
        repo: work.info.repo.clone(),
        number: work.info.number,
        base_ref: work.base_ref.clone(),
        base_sha: work.base_sha.clone(),
        head_sha: work.head_sha.clone(),
    });
    // Keep the ephemeral worktree alive for the serve lifetime.
    let _keep = work;
    if cli.no_auth && cli.host != "127.0.0.1" && cli.host != "localhost" && cli.host != "::1" {
        return Err("--no-auth is only allowed on loopback binds".into());
    }
    let o = ferro_server::server::ServeOpts {
        host: cli.host.clone(),
        port: cli.port,
        no_git: cli.no_git,
        narrate: !cli.quiet,
        no_open: cli.no_open,
        initial: None,
        token: cli.token.or_else(|| std::env::var("FERRO_TOKEN").ok()),
        allow_hosts: allow_hosts(cli.allow_host),
        no_auth: cli.no_auth,
        dev_web: cli.dev_web.clone(),
    };
    let (listener, bound) = ferro_server::server::bind_walk(&o.host, o.port).await;
    let h = ferro_server::server::serve_with(state, listener, bound, o).await;
    if !cli.no_open {
        let _ = open::that(&h.url);
    }
    wait_shutdown(h).await;
    Ok(())
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
