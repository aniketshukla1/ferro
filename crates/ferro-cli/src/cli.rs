use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "ferro", version, about = "Ferro — fast local code review")]
pub struct Cli {
    /// Directory, file, file:line, or GitHub PR URL to inspect.
    pub path: Option<PathBuf>,

    #[arg(short, long, default_value_t = 7778)]
    pub port: u16,

    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, default_value_t = false)]
    pub no_open: bool,

    #[arg(long, default_value_t = false)]
    pub no_lsp: bool,

    /// Disable git awareness (status badges, diffs).
    #[arg(long, default_value_t = false)]
    pub no_git: bool,

    /// Suppress startup narration.
    #[arg(long, default_value_t = false)]
    pub quiet: bool,

    /// Verbose logging (request timing, agent steps).
    #[arg(long, default_value_t = false)]
    pub verbose: bool,

    /// Disable ANSI colors in terminal output.
    #[arg(long, default_value_t = false)]
    pub no_color: bool,

    /// Auth token (else random per launch). Env FERRO_TOKEN.
    #[arg(long)]
    pub token: Option<String>,

    /// Disable API auth. Loopback binds only; prints a warning.
    #[arg(long, default_value_t = false)]
    pub no_auth: bool,

    /// Extra allowed Host hostnames (repeatable). Env FERRO_ALLOW_HOST (comma list).
    #[arg(long)]
    pub allow_host: Vec<String>,

    /// Bypass refusal when opening an already-merged PR URL.
    #[arg(short = 'y', long = "yes", default_value_t = false)]
    pub yes: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Ask the built-in agent about the workspace (uses read-only tools by default).
    Ask {
        /// The question.
        question: String,
        /// Workspace root. Defaults to cwd.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Model override (else GEMINI_MODEL / FERRO_MODEL / default per provider).
        #[arg(long)]
        model: Option<String>,
        /// Base URL override (OpenAI-compatible endpoint).
        #[arg(long)]
        base_url: Option<String>,
        /// Raw API key override (prefer GEMINI_API_KEY env).
        #[arg(long)]
        api_key: Option<String>,
        /// Max agent steps.
        #[arg(long, default_value_t = 8)]
        max_steps: usize,
        /// Allow write tools (apply_patch lands in P2-3; currently a no-op gate).
        #[arg(long, default_value_t = false)]
        allow_write: bool,
    },
}
