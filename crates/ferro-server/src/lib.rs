//! Ferro HTTP boundary (BACKEND.md § 4.1).

pub mod agent_ctx;
pub mod audit;
pub mod bus;
pub mod contract;
pub mod error;
pub mod guard;
pub mod hl;
pub mod jobs;
pub mod legacy;
pub mod lines;
pub mod lsp;
pub mod mcp;
pub mod read_only;
pub mod server;
pub mod state;
pub mod tls;
pub mod update_check;
pub mod v1;
pub mod watch;

pub use error::{ApiError, ErrorCode};
pub use state::{AppState, DesktopHost, Host, Limits, Mode, Workspace};
