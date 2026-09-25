//! Forge remote access (B4): PR/MR URL parsing, token resolution, and the
//! GitHub REST + GraphQL client with ETag caching and rate-limit handling.
//! Tokens never appear in logs, argv, or API responses: they travel in
//! request headers only, and `Debug` impls redact them.

pub mod checkout;
pub mod error;
pub mod github;
pub mod gitlab;
pub mod parse;
pub mod store;
pub mod token;

pub use checkout::{gc_worktrees, open_pr, CheckoutOpts, OpenedPr, WorktreeEntry};
pub use error::ForgeError;
pub use github::GitHub;
pub use parse::{parse_mr_url, parse_pr_url, ForgeRef, Provider};
pub use token::{resolve_token, TokenSource};
