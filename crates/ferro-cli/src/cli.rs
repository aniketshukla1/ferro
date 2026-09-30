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

    /// Refuse every mutation, AI edit, and harness call (403).
    #[arg(long, default_value_t = false)]
    pub read_only: bool,

    /// TLS mode: `self-signed` generates a cert with rcgen and prints its fingerprint.
    #[arg(long, value_name = "MODE")]
    pub tls: Option<String>,

    /// PEM certificate for HTTPS (requires `--tls-key`).
    #[arg(long, value_name = "PATH")]
    pub tls_cert: Option<PathBuf>,

    /// PEM private key for HTTPS (requires `--tls-cert`).
    #[arg(long, value_name = "PATH")]
    pub tls_key: Option<PathBuf>,

    /// Extra allowed Host hostnames (repeatable). Env FERRO_ALLOW_HOST (comma list).
    #[arg(long)]
    pub allow_host: Vec<String>,

    /// Bypass refusal when opening an already-merged PR URL.
    #[arg(short = 'y', long = "yes", default_value_t = false)]
    pub yes: bool,

    /// Serve static files from disk (frontend iteration, no-store).
    #[arg(long)]
    pub dev_web: Option<String>,

    /// Serve under a URL prefix, e.g. `/ferro`, for a reverse proxy that forwards the full path.
    #[arg(long, env = "FERRO_BASE_PATH")]
    pub base_path: Option<String>,

    /// Download and install the latest release (checksum-verified). Requires `FERRO_UPDATE_MANIFEST_URL`.
    #[arg(long, default_value_t = false)]
    pub update: bool,

    /// Internal: set by a restart into an installed update (port and token to keep).
    #[arg(long, hide = true, value_name = "PATH")]
    pub restart_state: Option<PathBuf>,

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
    /// Remove PR worktrees of merged or closed PRs older than 7 days.
    Gc,
    /// MCP server over stdio for coding agents (stdout is protocol-only).
    Mcp {
        /// Workspace root. Defaults to cwd.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Serve a remote directory over SSH (upload cached binary, port-forward, open browser).
    Ssh {
        /// Remote target: `[user@]host[:path]`.
        target: String,
        /// Do not open a local browser (still prints the forwarded URL).
        #[arg(long)]
        no_open: bool,
    },
}
