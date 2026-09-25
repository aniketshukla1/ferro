//! Forge remote access (B4): PR/MR URL parsing, token resolution, and the
//! GitHub REST + GraphQL client with ETag caching and rate-limit handling.
//! Tokens never appear in logs, argv, or API responses: they travel in
//! request headers only, and `Debug` impls redact them.

pub mod checkout;
pub mod error;
pub mod github;
pub mod parse;
pub mod token;

pub use checkout::{open_pr, CheckoutOpts, OpenedPr};
pub use error::ForgeError;
pub use github::GitHub;
pub use parse::{parse_pr_url, ForgeRef};
pub use token::{resolve_token, TokenSource};
