mod cli;
mod server;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::Cli;

#[tokio::main]
async fn main() -> AnyhowResult {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
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

type AnyhowResult = Result<(), Box<dyn std::error::Error>>;
