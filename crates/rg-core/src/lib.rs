//! ForgeKeep core business logic.
//!
//! Handles users, repositories, authentication, access control,
//! issues, pull requests, wiki, LFS, webhooks, code reviews,
//! branch protection, collaborators, organizations, and notifications.
//!
//! ## Module Groups (for future crate splits)
//!
//! - **Identity**: auth, user, org
//! - **Collaboration**: repo, issue, pull_request, wiki, review, collaborator,
//!   label, board, time_tracking, branch_protection, webhook, notification
//! - **Delivery & CI**: ci, release, package_registry, mirror, import
//! - **Infrastructure**: search, lfs, email, audit, platform

// ── Identity & Auth ─────────────────────────────────
pub mod attachment;
pub mod attestation;
pub mod auth;
pub mod org;
pub mod user;

// ── Collaboration ───────────────────────────────────
pub mod board;
pub mod branch_protection;
pub mod collaborator;
pub mod issue;
pub mod issue_template;
pub mod label;
pub mod notification;
pub mod pull_request;
pub mod repo;
pub mod review;
pub mod time_tracking;
pub mod webhook;
pub mod wiki;

// ── Delivery & CI ───────────────────────────────────
pub mod ci;
pub mod import;
pub mod mirror;
pub mod package_registry;
pub mod release;

// ── Infrastructure ──────────────────────────────────
pub mod audit;
pub mod blob_storage;
pub mod email;
pub mod env_compat; // IRONFORGE_* → FORGEKEEP_* env var fallback (fork rebrand)
pub mod lfs;
pub mod net; // SSRF-hardened outbound HTTP for user-supplied URLs
pub mod platform;
pub mod search; // Cross-platform abstractions
pub mod task_tracker; // Drain-aware tracker for detached fire-and-forget delivery tasks

pub mod error; // Domain error types (CoreError)
pub mod metrics_hook; // Observer hooks so the HTTP layer can meter core-crate events

use anyhow::Result;

/// Check if a username is valid (alphanumeric + hyphen + underscore, max 39).
///
/// Every failure here is the caller's, so each one carries
/// [`error::InvalidRequest`]: a handler that funnels the result through
/// `AppError::from` answers `400` with the rule that was broken instead of
/// blaming itself with a `500`.
pub fn validate_username(username: &str) -> Result<()> {
    if username.is_empty() {
        return Err(error::invalid_request("username cannot be empty"));
    }
    if username.len() > 39 {
        return Err(error::invalid_request(
            "username too long (max 39 characters)",
        ));
    }
    for c in username.chars() {
        if !c.is_alphanumeric() && c != '-' && c != '_' {
            return Err(error::invalid_request(format!(
                "username contains invalid character: {c}"
            )));
        }
    }
    Ok(())
}

/// Check if a repository name is valid.
///
/// Typed like [`validate_username`] — the request is what is wrong, and no
/// retry of it can succeed.
pub fn validate_repo_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(error::invalid_request("repository name cannot be empty"));
    }
    if name.len() > 100 {
        return Err(error::invalid_request(
            "repository name too long (max 100 characters)",
        ));
    }
    for c in name.chars() {
        if !c.is_alphanumeric() && c != '-' && c != '_' && c != '.' {
            return Err(error::invalid_request(format!(
                "repository name contains invalid character: {c}"
            )));
        }
    }
    Ok(())
}
