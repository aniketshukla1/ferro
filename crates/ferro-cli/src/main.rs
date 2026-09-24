mod cli;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use ferro::server;
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
    // PR mode: `ferro https://github.com/owner/repo/pull/N`
    if let Some(raw) = cli.path.as_ref().and_then(|p| p.to_str()) {
        if let Some(info) = ferro_core::pr::parse_pr_url(raw) {
            return serve_pr(cli, info).await;
        }
    }
    // file:line launch: serve the containing dir, open the file at the line.
    let mut initial: Option<(String, usize)> = None;
    let root = match cli.path.clone() {
        Some(p) => {
            let s = p.to_string_lossy().to_string();
            if let Some((f, l)) = split_file_line(&s) {
                let fp = std::path::Path::new(&f);
                let dir = fp
                    .parent()
                    .filter(|d| !d.as_os_str().is_empty())
                    .map(|d| d.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));
                let name = fp
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or(f);
                initial = Some((name, l));
                dir.canonicalize().unwrap_or_else(|_| PathBuf::from("."))
            } else {
                p.canonicalize().unwrap_or_else(|_| PathBuf::from("."))
            }
        }
        None => PathBuf::from(".")
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(".")),
    };

    let state = Arc::new(ferro_core::Index::new(root.clone()));
    let bg = state.clone();
    tokio::spawn(async move { bg.rebuild().await });

    server::serve(
        state,
        server::ServeOpts {
            host: cli.host,
            port: cli.port,
            no_git: cli.no_git,
            narrate: !cli.quiet,
            no_open: cli.no_open,
            initial,
            token: cli.token.or_else(|| std::env::var("FERRO_TOKEN").ok()),
            allow_hosts: allow_hosts(cli.allow_host),
            no_auth: cli.no_auth,
        },
    )
    .await;
    Ok(())
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
    let state = Arc::new(ferro_core::Index::new(work.dir.clone()));
    state.set_pr(ferro_core::pr::PrCtx {
        owner: work.info.owner.clone(),
        repo: work.info.repo.clone(),
        number: work.info.number,
        base_ref: work.base_ref.clone(),
        base_sha: work.base_sha.clone(),
        head_sha: work.head_sha.clone(),
    });
    let bg = state.clone();
    tokio::spawn(async move { bg.rebuild().await });
    // Keep the ephemeral worktree alive for the serve lifetime.
    let _keep = work;
    if cli.no_auth && cli.host != "127.0.0.1" && cli.host != "localhost" && cli.host != "::1" {
        return Err("--no-auth is only allowed on loopback binds".into());
    }
    server::serve(
        state,
        server::ServeOpts {
            host: cli.host,
            port: cli.port,
            no_git: cli.no_git,
            narrate: !cli.quiet,
            no_open: cli.no_open,
            initial: None,
            token: cli.token.or_else(|| std::env::var("FERRO_TOKEN").ok()),
            allow_hosts: allow_hosts(cli.allow_host),
            no_auth: cli.no_auth,
        },
    )
    .await;
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
}
