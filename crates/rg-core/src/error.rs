//! Domain error types for rg-core.
//!
//! Replaces `anyhow::anyhow!()` with typed errors that carry semantic meaning
//! and can be automatically mapped to HTTP status codes via `From` impls.
//!
//! # Usage
//!
//! ```rust,ignore
//! use rg_core::error::CoreError;
//!
//! fn get_repo(name: &str) -> Result<Repo, CoreError> {
//!     if name.is_empty() {
//!         return Err(CoreError::InvalidInput("repo name cannot be empty".into()));
//!     }
//!     // ...
//! }
//! ```

use thiserror::Error;

/// Unified error type for rg-core business logic.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Resource not found (repo, user, issue, PR, etc.)
    #[error("{0}")]
    NotFound(String),

    /// Permission denied — actor lacks required access.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// Resource already exists or state conflict (e.g., PR already merged).
    #[error("conflict: {0}")]
    Conflict(String),

    /// Invalid input / validation failure.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Generic internal error (wraps anyhow for gradual migration).
    #[error(transparent)]
    Internal(anyhow::Error),
}

impl CoreError {
    /// Create a not-found error with a formatted message.
    pub fn not_found(msg: impl Into<String>) -> Self {
        CoreError::NotFound(msg.into())
    }

    /// Create a forbidden error with a formatted message.
    pub fn forbidden(msg: impl Into<String>) -> Self {
        CoreError::Forbidden(msg.into())
    }

    /// Create a conflict error with a formatted message.
    pub fn conflict(msg: impl Into<String>) -> Self {
        CoreError::Conflict(msg.into())
    }

    /// Create an invalid-input error.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        CoreError::InvalidInput(msg.into())
    }
}

// Convenience: convert anyhow::Error directly into CoreError::Internal
impl From<anyhow::Error> for CoreError {
    fn from(e: anyhow::Error) -> Self {
        CoreError::Internal(e)
    }
}

/// The row the caller asked for genuinely is not there.
///
/// Services still return `anyhow::Result`, so "no such pull request" and "the
/// query failed" arrive at the HTTP layer as the same flattened
/// `anyhow::Error`. Handlers papered over that by answering `404` to both —
/// which turns a database outage into "the PR was deleted" for the client's
/// retry logic and puts nothing in the alerts. The distinction has to travel
/// *inside* the error, which is what this type is for; it is the same move as
/// [`crate::package_registry::oci::storage::DigestMismatch`], one level up.
///
/// `resource` is a `&'static str` on purpose: the rendered message is a fixed
/// string with no request data and no `.context("db: …")` chain in it, so the
/// HTTP layer can hand it to the client verbatim without breaching H-05.
///
/// Recognised in `rg-http`'s `From<anyhow::Error> for AppError` via
/// `downcast_ref`, which sees through any `.context(…)` a caller added on the
/// way up. Anything that does *not* carry it stays a 5xx.
#[derive(Debug, Error)]
#[error("{resource} not found")]
pub struct NotFound {
    pub resource: &'static str,
}

impl NotFound {
    pub const fn new(resource: &'static str) -> Self {
        Self { resource }
    }
}

/// Shorthand for the `anyhow` form of [`NotFound`].
pub fn not_found(resource: &'static str) -> anyhow::Error {
    anyhow::Error::new(NotFound::new(resource))
}

/// The request itself is wrong, and no retry of it can succeed.
///
/// The mirror image of [`NotFound`], for the other direction of the same
/// mistake. A service that validates its input and then writes to a blob store
/// fails for two unrelated reasons — "this file type is not allowed" and "the
/// storage root is not writable" — and flattened into an `anyhow::Error` the
/// two look alike. Handlers papered over that by answering `400` to both, which
/// tells the client to fix a request that was never wrong and hides a broken
/// `repo_root` behind the uploader's own file. Only an error carrying this type
/// may become a `400`; anything else is ours, and stays a 5xx the client is
/// allowed to retry.
///
/// Like [`NotFound`], the rendered message reaches the client verbatim, so it
/// must stay a fixed description of the rule that was broken — never a
/// filesystem path, an errno or a `db: …` chain (H-05).
#[derive(Debug, Error)]
#[error("{message}")]
pub struct InvalidRequest {
    pub message: String,
}

impl InvalidRequest {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Shorthand for the `anyhow` form of [`InvalidRequest`].
pub fn invalid_request(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(InvalidRequest::new(message))
}

/// The request is fine and the caller may retry it later, but the resource is
/// in a state that forbids it right now.
///
/// The third member of the [`NotFound`] / [`InvalidRequest`] family, for the
/// cases those two get wrong in the same way: merging a closed pull request,
/// merging a draft, racing another merge, or hitting a merge conflict are all
/// *state* problems, and calling them `400 Bad Request` tells the client to fix
/// a request that was already correct. A `409` says what actually happened —
/// "try again once the state changes" — and keeps the genuine 400s (an
/// unparseable merge strategy) meaningful.
///
/// Like the other two, the rendered message reaches the client verbatim, so it
/// must describe the state and nothing else — no `db: …` chain, no git command
/// line (H-05).
#[derive(Debug, Error)]
#[error("{message}")]
pub struct Conflict {
    pub message: String,
}

impl Conflict {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Shorthand for the `anyhow` form of [`Conflict`].
pub fn conflict(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Conflict::new(message))
}

/// A policy the caller is subject to says no.
///
/// Distinct from [`Conflict`] (the resource's state) and from
/// [`InvalidRequest`] (the request's shape): the request is well-formed and the
/// resource is ready, but a rule — branch protection, a required review, a
/// required status check — refuses it. A handler that blanket-403s every error
/// its policy check returns cannot tell that refusal from the check itself
/// failing, which hides an outage behind "you are not allowed".
///
/// The message names the rule that refused and reaches the client verbatim, so
/// it must stay free of internal detail (H-05).
#[derive(Debug, Error)]
#[error("{message}")]
pub struct Forbidden {
    pub message: String,
}

impl Forbidden {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Shorthand for the `anyhow` form of [`Forbidden`].
pub fn forbidden(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Forbidden::new(message))
}
