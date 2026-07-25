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
