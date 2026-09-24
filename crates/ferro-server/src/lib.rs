//! Ferro HTTP boundary (BACKEND.md § 4.1).

pub mod bus;
pub mod error;
pub mod hl;
pub mod jobs;
pub mod lines;
pub mod state;
pub mod v1;

pub use error::{ApiError, ErrorCode};
pub use state::{AppState, DesktopHost, Host, Limits, Mode, Workspace};
