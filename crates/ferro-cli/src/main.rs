mod cli;
mod server;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> AnyhowResult {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
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
    let root = cli
        .path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."));

    let state = Arc::new(ferro_core::Index::new(root.clone()));
    let bg = state.clone();
    tokio::spawn(async move { bg.rebuild().await });

    let addr = format!("{}:{}", cli.host, cli.port);
    tracing::info!("ferro serving {} on http://{}", root.display(), addr);

    if !cli.no_open {
        let url = format!("http://127.0.0.1:{}/", cli.port);
        let _ = open::that(&url);
    }

    server::serve(state, &addr).await;
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
