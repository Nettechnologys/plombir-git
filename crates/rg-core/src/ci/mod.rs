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
    /// Branch the event targets, for the workflow formats that filter on it.
    ///
    /// A `branches:` filter under `on: pull_request` applies to the PR's **base**
    /// branch, not to `ref_name` (which is the head). Nothing carried that
    /// branch down here, so the matcher fell back to the repository's default
    /// branch — correct only for PRs that happen to target it, and silently
    /// wrong for a PR into `develop`. `None` keeps the old fallback and is the
    /// right answer for events that have no target branch (a push, a manual run).
    pub base_branch: Option<&'a str>,
    pub triggered_by: Option<i64>,
    pub docker_enabled: bool,
    pub external_runners: bool,
    /// Whether jobs without an `image:` may run as a shell directly on the host.
    /// Defaults to `false` (secure): untrusted CI config must not execute on the
    /// server. When false, imageless jobs are refused unless dispatched to a
    /// Docker container or an external runner.
    pub allow_host_runner: bool,
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
    /// See [`TriggerPipelineParams::allow_host_runner`].
    pub allow_host_runner: bool,
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

    /// Whether a workflow at `commit_sha` is actually triggered by `event`.
    ///
    /// The gate for events the *native* `.forgekeep-ci.yml` format has no notion
    /// of. [`has_ci_config`](Self::has_ci_config) answers the weaker question
    /// "is there a pipeline definition here at all", which for `pull_request`
    /// is a false yes on every repository driving CI from a native config: it
    /// describes one push pipeline and would be run a second time, identically,
    /// on every PR open and every PR sync.
    ///
    /// `base_branch` is the PR's target branch (see
    /// [`TriggerPipelineParams::base_branch`]); `None` for events without one.
    fn has_workflow_for_event(
        &self,
        repo_path: &Path,
        commit_sha: &str,
        event: &str,
        ref_name: &str,
        base_branch: Option<&str>,
    ) -> bool;

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

/// Operator-facing wording for "this commit carries no pipeline definition".
///
/// It lives next to [`has_ci_config`] so the message cannot drift from what the
/// gate actually accepts: the manual-trigger endpoint used to answer
/// `no .forgekeep-ci.yml found`, while `.gitea/workflows/` has been recognised
/// for just as long — so a repository using Gitea Actions was told its perfectly
/// valid config did not exist.
pub const NO_CI_CONFIG_MESSAGE: &str =
    "no CI config found at this commit — expected `.forgekeep-ci.yml` or `.gitea/workflows/*.yml`";

/// Check if a repo has any CI config at the given commit.
///
/// M-14: Moved from `rg-ci` to `rg-core` so it can be used without
/// depending on `rg-ci`.
///
/// **Fail-open on purpose:** a repository that cannot even be opened answers
/// `false`, i.e. "no CI here", so a broken repository never blocks a push. What
/// it must not do is stay *silent* about it — see [`has_ci_config_checked`] for
/// why, and for the variant that reports instead of swallowing.
pub fn has_ci_config(repo_path: &Path, commit_sha: &str) -> bool {
    match has_ci_config_checked(repo_path, commit_sha) {
        Ok(present) => present,
        Err(error) => {
            // `{:#}` keeps the whole anyhow chain: the context names the path,
            // the cause carries the actual reason gix refused to open it.
            tracing::warn!(
                repo = %repo_path.display(),
                "CI gate: cannot open repository, treating the commit as having no CI config: {:#}",
                error
            );
            false
        }
    }
}

/// [`has_ci_config`] without the fail-open: `Err` means the repository itself
/// could not be opened, so the answer is "unknown", not "no CI config".
///
/// The gate runs ahead of `trigger_pipeline` in all four trigger paths (the
/// post-push hook, the manual trigger, the merge queue and review-suggestions),
/// and every one of them simply returns when it answers `false`. With the open
/// error dropped on the floor, a repository whose `objects/` became unreadable,
/// whose `HEAD` got corrupted, or which was removed from under the server
/// accepted pushes while quietly creating no pipeline and logging nothing at
/// all — "this repo has no CI config" and "the server cannot read this repo"
/// were indistinguishable. `trigger_pipeline` itself would have said
/// `failed to open repository: {path}`, but the gate never let it run.
pub fn has_ci_config_checked(repo_path: &Path, commit_sha: &str) -> Result<bool> {
    use anyhow::Context;

    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {}", repo_path.display()))?;

    // An unborn HEAD is the one ordinary negative commit case: a freshly
    // initialized repository has no tree in which a CI config could exist.
    // Every other failure below is storage or object corruption and must remain
    // observable to the fail-open wrapper.
    if commit_sha == "HEAD" && repo.head()?.is_unborn() {
        return Ok(false);
    }

    let commit = repo
        .rev_parse_single(commit_sha)
        .with_context(|| format!("failed to resolve CI commit {commit_sha}"))?
        .object()
        .with_context(|| format!("failed to read CI commit {commit_sha}"))?
        .peel_to_tree()
        .with_context(|| format!("failed to read CI tree at commit {commit_sha}"))?;

    for path in [".gitea/workflows", ".forgekeep-ci.yml"] {
        if commit
            .lookup_entry_by_path(path)
            .with_context(|| format!("failed to look up {path} at commit {commit_sha}"))?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{has_ci_config, has_ci_config_checked, NO_CI_CONFIG_MESSAGE};

    /// The bug: an unopenable repository was indistinguishable from one that
    /// simply has no pipeline definition, and nothing was logged either way.
    /// The checked variant is what makes the difference observable — and
    /// testable without capturing a tracing subscriber.
    #[test]
    fn a_repository_that_cannot_be_opened_is_an_error_not_a_plain_no() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nothing-here.git");

        let error = has_ci_config_checked(&missing, "deadbeef")
            .expect_err("a path that is not a repository must not answer a confident `false`");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(missing.to_str().unwrap()),
            "error must name the repository path: {rendered}"
        );
        assert!(
            rendered.contains("failed to open repository"),
            "error must say the open failed: {rendered}"
        );
    }

    /// The gate itself stays fail-open: a broken repository must never block a
    /// push, it must only stop being silent about it.
    #[test]
    fn the_gate_stays_fail_open_on_an_unopenable_repository() {
        let dir = tempfile::tempdir().unwrap();

        assert!(!has_ci_config(
            &dir.path().join("nothing-here.git"),
            "deadbeef"
        ));
    }

    /// A real repository with no CI config is the ordinary negative answer, and
    /// it must stay distinct from the error above.
    #[test]
    fn a_repository_without_ci_config_answers_a_clean_no() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("empty.git");
        gix::init_bare(&repo_path).expect("a bare repo must initialise");

        assert!(
            !has_ci_config_checked(&repo_path, "HEAD").expect("an openable repo must not error"),
            "a repository with no CI config must answer `false`, not error"
        );
        assert!(!has_ci_config(&repo_path, "HEAD"));
    }

    /// The card's deploy-shaped scenario: the reference remains readable but
    /// its commit object disappears from the store. The gate must preserve the
    /// error for the fail-open wrapper instead of returning a confident no.
    #[test]
    fn a_missing_commit_object_does_not_pass_for_a_missing_ci_config() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("work");
        let git = rg_git::cli_gateway::GitCommandGateway::new().expect("git must be installed");
        git.run_or_bail(&["init", "-q", repo_path.to_str().unwrap()], None)
            .unwrap();
        std::fs::write(repo_path.join(".forgekeep-ci.yml"), "jobs: {}\n").unwrap();
        for args in [
            vec!["config", "user.email", "ci@example.com"],
            vec!["config", "user.name", "CI"],
            vec!["add", "."],
            vec!["-c", "commit.gpgsign=false", "commit", "-qm", "ci"],
        ] {
            git.run_or_bail(&args, Some(&repo_path)).unwrap();
        }
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(&repo_path))
            .unwrap()
            .stdout_str()
            .trim()
            .to_string();

        // Sanity: with the objects readable the gate finds the config.
        assert!(
            has_ci_config(&repo_path, &sha),
            "the fixture itself must have a discoverable CI config"
        );

        let object_path = repo_path
            .join(".git")
            .join("objects")
            .join(&sha[..2])
            .join(&sha[2..]);
        assert!(
            object_path.exists(),
            "fixture must keep HEAD as a loose object"
        );
        std::fs::remove_file(&object_path).unwrap();

        // Fail-open is preserved (a push is never blocked)...
        let gate = has_ci_config(&repo_path, &sha);
        // ...but the reason is now retrievable rather than dropped on the floor.
        let checked = has_ci_config_checked(&repo_path, &sha)
            .expect_err("a missing commit object must not become no CI config");

        assert!(!gate, "the gate must stay fail-open, not block the push");
        let rendered = format!("{checked:#}");
        assert!(
            rendered.contains("failed to resolve CI commit"),
            "error must preserve the failed object-store lookup: {rendered}"
        );
    }

    /// The manual trigger used to name only `.forgekeep-ci.yml`, so a repository
    /// driving CI from `.gitea/workflows/` was told its config did not exist.
    #[test]
    fn the_operator_message_names_every_accepted_location() {
        assert!(NO_CI_CONFIG_MESSAGE.contains(".forgekeep-ci.yml"));
        assert!(NO_CI_CONFIG_MESSAGE.contains(".gitea/workflows"));
    }
}
