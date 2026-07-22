//! CI/CD business logic and utilities.

pub mod log_write_queue;

use anyhow::Result;
use sea_orm::DatabaseConnection;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

/// Parameters for triggering a CI pipeline.
///
/// M-14: Moved from `rg-ci` to `rg-core` so that `rg-http` can depend
/// only on `rg-core` for CI types, removing the direct `rg-http → rg-ci`
/// dependency.
pub struct TriggerPipelineParams<'a> {
    pub db: &'a DatabaseConnection,
    pub repo_path: &'a Path,
    pub repo_id: i64,
    pub commit_sha: &'a str,
    pub ref_name: &'a str,
    pub trigger_type: &'a str,
    pub triggered_by: Option<i64>,
    pub docker_enabled: bool,
    pub external_runners: bool,
    pub jwt_secret: Option<&'a str>,
    pub external_url: Option<&'a str>,
}

/// Parameters for resuming an existing pipeline after a manual gate.
pub struct ResumePipelineParams<'a> {
    pub db: &'a DatabaseConnection,
    pub repo_path: &'a Path,
    pub repo_id: i64,
    pub pipeline_id: i64,
    pub docker_enabled: bool,
    pub external_runners: bool,
    pub jwt_secret: Option<&'a str>,
    pub external_url: Option<&'a str>,
}

/// Trait for CI pipeline triggering, implemented by `rg-ci`.
///
/// M-14: This trait decouples `rg-http` from `rg-ci`. The HTTP layer
/// calls through this trait instead of directly importing `rg-ci`.
pub trait CiTrigger: Send + Sync {
    /// Check if a repo has CI config at the given commit.
    fn has_ci_config(&self, repo_path: &Path, commit_sha: &str) -> bool;

    /// Trigger a CI pipeline. Returns the pipeline ID.
    fn trigger_pipeline<'a>(
        &'a self,
        params: TriggerPipelineParams<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<i64>> + Send + 'a>>;

    /// Resume an existing pipeline whose manual job has been released.
    fn resume_pipeline<'a>(
        &'a self,
        params: ResumePipelineParams<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

/// Check if a repo has any CI config at the given commit.
///
/// M-14: Moved from `rg-ci` to `rg-core` so it can be used without
/// depending on `rg-ci`.
pub fn has_ci_config(repo_path: &Path, commit_sha: &str) -> bool {
    let repo = match gix::open(repo_path) {
        Ok(r) => r,
        Err(_) => return false,
    };

    // Check Gitea Actions format
    let tree_revspec = format!("{}:.gitea/workflows", commit_sha);
    if repo.rev_parse_single(tree_revspec.as_str()).is_ok() {
        return true;
    }

    // Check native format
    let revspec = format!("{}:.ironforge-ci.yml", commit_sha);
    repo.rev_parse_single(revspec.as_str()).is_ok()
}
