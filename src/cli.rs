use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "ferro", version, about = "Ferro — fast local code review")]
pub struct Cli {
    /// Directory or file to inspect. Also accepts file:line, e.g. main.rs:42
    pub path: Option<PathBuf>,

    #[arg(short, long, default_value_t = 7778)]
    pub port: u16,

    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, default_value_t = false)]
    pub no_open: bool,

    #[arg(long, default_value_t = false)]
    pub no_lsp: bool,
}
