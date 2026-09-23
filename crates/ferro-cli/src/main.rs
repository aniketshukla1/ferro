mod cli;
mod server;

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
    // PR mode: `ferro https://github.com/owner/repo/pull/N`
    if let Some(raw) = cli.path.as_ref().and_then(|p| p.to_str()) {
        if let Some(info) = ferro_core::pr::parse_pr_url(raw) {
            return serve_pr(cli, info).await;
        }
    }
    let root = cli
        .path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));

    let state = Arc::new(ferro_core::Index::new(root.clone()));
    let bg = state.clone();
    tokio::spawn(async move { bg.rebuild().await });

    server::serve(
        state,
        &cli.host,
        cli.port,
        cli.no_git,
        !cli.quiet,
        cli.no_open,
    )
    .await;
    Ok(())
}

async fn serve_pr(cli: Cli, info: ferro_core::pr::PrInfo) -> AnyhowResult {
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
    server::serve(
        state,
        &cli.host,
        cli.port,
        cli.no_git,
        !cli.quiet,
        cli.no_open,
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
