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
pub use gitlab::GitLab;
pub use parse::{parse_mr_url, parse_pr_url, parse_remote, ForgeRef, Provider};
pub use store::ReviewStore;
pub use store::{Draft, DraftPatch, DraftSource, Finding, NewDraft, Round, ViewedState};
pub use token::{resolve_gitlab_token, resolve_token, TokenSource};

pub use github::LearnComment;
use github::{
    Checks, ForgeComment, PullMeta, ReviewComment, ReviewEvent, ReviewThread, SubmitResponse,
};

/// Provider-dispatching client with the same shapes either way, so the
/// server endpoints stay provider-agnostic.
pub enum ForgeClient {
    GitHub(GitHub),
    GitLab(GitLab),
}

/// Reply target: GitHub numeric comment id or GitLab discussion id.
pub enum ReplyTarget {
    Comment(u64),
    Discussion(String),
}

impl ForgeClient {
    pub fn for_ref(r: &ForgeRef, token: Option<String>) -> Self {
        match r.provider {
            Provider::GitHub => ForgeClient::GitHub(GitHub::for_ref(r, token)),
            Provider::GitLab => ForgeClient::GitLab(GitLab::for_ref(r, token)),
        }
    }

    pub fn has_token(&self) -> bool {
        match self {
            ForgeClient::GitHub(g) => g.has_token(),
            ForgeClient::GitLab(g) => g.has_token(),
        }
    }

    pub async fn pull(&self, r: &ForgeRef) -> Result<PullMeta, ForgeError> {
        match self {
            ForgeClient::GitHub(g) => g.pull(r).await,
            ForgeClient::GitLab(g) => g.pull(r).await,
        }
    }

    /// Review comments on recently merged pull requests (see `GitHub::merged_review_comments`).
    pub async fn merged_review_comments(
        &self,
        r: &ForgeRef,
        max_prs: usize,
    ) -> Result<(usize, Vec<LearnComment>), ForgeError> {
        match self {
            ForgeClient::GitHub(g) => g.merged_review_comments(r, max_prs).await,
            ForgeClient::GitLab(g) => g.merged_review_comments(r, max_prs).await,
        }
    }

    pub async fn can_push(&self, r: &ForgeRef) -> Result<bool, ForgeError> {
        match self {
            ForgeClient::GitHub(g) => g.can_push(r).await,
            ForgeClient::GitLab(g) => g.can_push(r).await,
        }
    }

    pub async fn checks(&self, r: &ForgeRef, sha: &str) -> Result<Checks, ForgeError> {
        match self {
            ForgeClient::GitHub(g) => g.checks(r, sha).await,
            ForgeClient::GitLab(g) => g.checks(r, sha).await,
        }
    }

    pub async fn threads(&self, r: &ForgeRef) -> Result<Vec<ReviewThread>, ForgeError> {
        match self {
            ForgeClient::GitHub(g) => g.threads(r).await,
            ForgeClient::GitLab(g) => g.threads(r).await,
        }
    }

    pub async fn conversation(&self, r: &ForgeRef) -> Result<Vec<ForgeComment>, ForgeError> {
        match self {
            ForgeClient::GitHub(g) => g.conversation(r).await,
            ForgeClient::GitLab(g) => g.conversation(r).await,
        }
    }

    /// Review threads and conversation together (GitLab serves both from
    /// one discussions list, read once).
    pub async fn review_state(
        &self,
        r: &ForgeRef,
    ) -> Result<(Vec<ReviewThread>, Vec<ForgeComment>), ForgeError> {
        match self {
            ForgeClient::GitHub(g) => {
                let (threads, conversation) = tokio::join!(g.threads(r), g.conversation(r));
                Ok((threads?, conversation?))
            }
            ForgeClient::GitLab(g) => g.review_state(r).await,
        }
    }

    pub async fn submit_review(
        &self,
        r: &ForgeRef,
        head_sha: &str,
        event: ReviewEvent,
        body: &str,
        comments: &[ReviewComment],
    ) -> Result<SubmitResponse, ForgeError> {
        match self {
            ForgeClient::GitHub(g) => g.submit_review(r, head_sha, event, body, comments).await,
            ForgeClient::GitLab(g) => g.submit_review(r, head_sha, event, body, comments).await,
        }
    }

    pub async fn reply(
        &self,
        r: &ForgeRef,
        target: ReplyTarget,
        body: &str,
    ) -> Result<ForgeComment, ForgeError> {
        match (self, target) {
            (ForgeClient::GitHub(g), ReplyTarget::Comment(id)) => g.reply(r, id, body).await,
            (ForgeClient::GitLab(g), ReplyTarget::Discussion(id)) => g.reply(r, &id, body).await,
            _ => Err(ForgeError::Schema("reply target mismatch".into())),
        }
    }

    /// GitLab-only: stage a reply draft (published by bulk_publish).
    /// Returns the draft note id for [`Self::discard_drafts`].
    pub async fn reply_draft(
        &self,
        r: &ForgeRef,
        discussion_id: &str,
        body: &str,
    ) -> Result<u64, ForgeError> {
        match self {
            ForgeClient::GitLab(g) => g.reply_draft(r, discussion_id, body).await,
            ForgeClient::GitHub(_) => {
                Err(ForgeError::Schema("reply drafts are GitLab-only".into()))
            }
        }
    }

    /// GitLab-only, best effort: drop staged drafts of a failed submit.
    pub async fn discard_drafts(&self, r: &ForgeRef, ids: &[u64]) {
        if let ForgeClient::GitLab(g) = self {
            g.discard_drafts(r, ids).await;
        }
    }

    pub async fn post_comment(&self, r: &ForgeRef, body: &str) -> Result<ForgeComment, ForgeError> {
        match self {
            ForgeClient::GitHub(g) => g.post_comment(r, body).await,
            ForgeClient::GitLab(g) => g.post_comment(r, body).await,
        }
    }
}
