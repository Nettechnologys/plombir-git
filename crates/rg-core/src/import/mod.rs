//! Data migration import — GitHub / GitLab → IronForge.
//!
//! Supports importing repositories and their metadata (issues, PRs,
//! labels, milestones, releases, wiki) from external platforms.

pub mod github_client;
pub mod gitlab_client;
pub mod service;
