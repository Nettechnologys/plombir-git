//! IronForge core business logic.
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
pub mod lfs;
pub mod platform;
pub mod search; // Cross-platform abstractions

pub mod error; // Domain error types (CoreError)

use anyhow::Result;

/// Check if a username is valid (alphanumeric + hyphen + underscore, max 39).
pub fn validate_username(username: &str) -> Result<()> {
    if username.is_empty() {
        anyhow::bail!("username cannot be empty");
    }
    if username.len() > 39 {
        anyhow::bail!("username too long (max 39 characters)");
    }
    for c in username.chars() {
        if !c.is_alphanumeric() && c != '-' && c != '_' {
            anyhow::bail!("username contains invalid character: {}", c);
        }
    }
    Ok(())
}

/// Check if a repository name is valid.
pub fn validate_repo_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("repository name cannot be empty");
    }
    if name.len() > 100 {
        anyhow::bail!("repository name too long (max 100 characters)");
    }
    for c in name.chars() {
        if !c.is_alphanumeric() && c != '-' && c != '_' && c != '.' {
            anyhow::bail!("repository name contains invalid character: {}", c);
        }
    }
    Ok(())
}
