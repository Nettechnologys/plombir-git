//! CI/CD business logic and utilities.

pub mod log_write_queue;

use anyhow::{Context, Result};
use sea_orm::{DatabaseConnection, TransactionTrait};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

/// Smallest per-job CI timeout a config may declare, in seconds.
pub const JOB_TIMEOUT_MIN_SECS: i64 = 1;

/// Largest per-job CI timeout a config may declare, in seconds (24 hours).
pub const JOB_TIMEOUT_MAX_SECS: i64 = 86_400;

/// Grace window added to a job's execution timeout when minting its
/// `CI_JOB_TOKEN`, so the token outlives the run it was issued for.
const CI_JOB_TOKEN_GRACE_SECS: i64 = 300;

/// Floor for a `CI_JOB_TOKEN` lifetime: a one-second job still has to fetch its
/// sources and report back.
const CI_JOB_TOKEN_MIN_LIFETIME_SECS: i64 = 60;

/// The event name a manual pipeline run carries.
///
/// `pipeline.trigger_type` is not a label for the database — it is the event
/// the workflow matcher is asked about, and the same string reaches the job as
/// `CI_EVENT` / `${{ github.event_name }}`. The Run button used to invent
/// `"manual"`, which no `on:` clause can name, so for a repository whose CI
/// lives in `.gitea/workflows/` it always answered "no workflow is triggered by
/// event manual" — for a completely valid file (card_e87a1b6f9633).
/// `workflow_dispatch` is what that event is called everywhere else.
pub const WORKFLOW_DISPATCH_EVENT: &str = "workflow_dispatch";

/// Every event a pipeline may be created under.
///
/// The vocabulary is `Workflow::matches_event`'s, not this list's — the list
/// exists so that a producer inventing a name of its own is caught by
/// `every_event_a_pipeline_is_created_under_is_one_a_workflow_can_declare` in
/// `rg-ci` rather than by a user wondering why their workflow never ran. Three
/// producers have already made exactly that mistake (`suggestion`, `manual`,
/// `retry`), and each failed the same way: a workflow file that is perfectly
/// valid, and no pipeline.
pub const PIPELINE_EVENTS: [&str; 4] = [
    "push",
    crate::pull_request::ci::PULL_REQUEST_EVENT,
    "merge_group",
    WORKFLOW_DISPATCH_EVENT,
];

/// Variables whose values belong to the pipeline runner, never to a committed
/// job variable or a repository secret.
///
/// Both the embedded runner and the external-runner API inject this vocabulary.
/// Keeping one list prevents a new built-in from being protected on one path
/// while a user-controlled value can still replace it on the other.
pub const BUILTIN_CI_VARIABLES: [&str; 11] = [
    "CI",
    "FORGEKEEP",
    "CI_PIPELINE_ID",
    "CI_COMMIT_SHA",
    "CI_SHA",
    "CI_REF",
    "CI_EVENT",
    "CI_REPOSITORY",
    "CI_REPOSITORY_OWNER",
    "CI_JOB_TOKEN",
    "CI_OIDC_TOKEN_URL",
];

pub fn is_builtin_ci_variable(name: &str) -> bool {
    BUILTIN_CI_VARIABLES.contains(&name)
}

/// The outcome a runner may report for a job it took.
///
/// `pipeline_job.status` is a string column whose domain was expressed nowhere:
/// the writer declared it in a comment (`status: String, // success | failure |
/// error`) and the readers spelled out literal lists. So
/// `POST /runners/{id}/jobs/{job_id}/finish` with `{"status":"succes"}` answered
/// `200 OK`, stored the typo, and every later roll-up read it as "still
/// running" — the stage and the pipeline hung in `running` forever, required
/// checks on the PR never unblocked, and nothing anywhere logged a problem
/// (card_39bf6a755499).
///
/// Deliberately narrower than the column: `skipped` and `canceled` are the
/// server's own words for work it decided not to run, and a runner reporting
/// them would be describing a decision it did not make — the same line
/// `mirror.status` draws between the operator's switch and the sweep's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Success,
    /// Two spellings, both already in the column: the bundled runner reports
    /// `failure` while the embedded one writes `failed`. Accepting only one of
    /// them would reject a runner that copied the other, so both parse — and
    /// each is stored as it arrived, since the readers already treat them alike.
    Failure,
    Failed,
    /// The job could not be executed at all (no image, timeout, runner fault),
    /// as opposed to a job that ran and came back non-zero.
    Error,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Failed => "failed",
            Self::Error => "error",
        }
    }

    /// Parse an outcome reported over the runner API.
    ///
    /// The error is [`crate::error::invalid_request`], the one type
    /// `AppError::from` turns into a `400`, and its message states the rule
    /// without echoing the input back.
    pub fn parse_runner_report(status: &str) -> Result<Self> {
        match status {
            "success" => Ok(Self::Success),
            "failure" => Ok(Self::Failure),
            "failed" => Ok(Self::Failed),
            "error" => Ok(Self::Error),
            _ => Err(crate::error::invalid_request(
                "invalid job status: a runner reports one of success, failure, failed, error \
                 (`skipped` and `canceled` are decided by the server, not reported)",
            )),
        }
    }
}

/// Whether a declared per-job timeout falls inside the accepted range.
///
/// The single authority for that range. It used to be spelled out separately in
/// the validator, in both runner paths and in both token-minting call sites —
/// five copies that disagreed on what an out-of-range value means, which is how
/// the same persisted number could become "one hour" in one runner and "one
/// second" in the other.
pub fn job_timeout_in_range(secs: i64) -> bool {
    (JOB_TIMEOUT_MIN_SECS..=JOB_TIMEOUT_MAX_SECS).contains(&secs)
}

/// Resolve the execution timeout of a single job, in seconds (`0` = unbounded,
/// the documented meaning of `timeouts.job_secs = 0`).
///
/// `declared` is the value persisted on the job row. `validate_execution_semantics`
/// refuses to create a job outside [`job_timeout_in_range`], so the out-of-range
/// arm is unreachable from any accepted config — but it is what a corrupt or
/// hand-edited row hits, and it must not pass silently: swapping a declared
/// timeout for the server default changes how long the job may run, which is
/// exactly the kind of substitution that used to leave no trace anywhere.
pub fn resolve_job_timeout_secs(job_id: i64, declared: Option<i64>, default_secs: u64) -> u64 {
    let fallback = default_secs.min(JOB_TIMEOUT_MAX_SECS as u64);
    match declared {
        None => fallback,
        Some(secs) if job_timeout_in_range(secs) => u64::try_from(secs).unwrap_or(fallback),
        Some(secs) => {
            tracing::warn!(
                job_id,
                declared_timeout_seconds = secs,
                effective_timeout_seconds = fallback,
                "persisted CI job timeout is outside {}..={} seconds; falling back to the server default",
                JOB_TIMEOUT_MIN_SECS,
                JOB_TIMEOUT_MAX_SECS
            );
            fallback
        }
    }
}

/// The deadline handed to an out-of-process runner, which has no notion of an
/// unbounded job: the wire field is always a positive number of seconds.
///
/// `timeouts.job_secs = 0` documents "no timeout" for the embedded runner. The
/// dispatch path used to push that zero through a `clamp(1, …)`, so on such an
/// instance every job sent to an external runner was killed one second in —
/// "unlimited" read as its exact opposite. Unbounded maps to the same 24h
/// ceiling a job may declare for itself.
pub fn dispatched_job_timeout_secs(execution_timeout_secs: u64) -> i64 {
    if execution_timeout_secs == 0 {
        return JOB_TIMEOUT_MAX_SECS;
    }
    i64::try_from(execution_timeout_secs)
        .unwrap_or(JOB_TIMEOUT_MAX_SECS)
        .clamp(JOB_TIMEOUT_MIN_SECS, JOB_TIMEOUT_MAX_SECS)
}

/// Lifetime for the `CI_JOB_TOKEN` handed to a job whose execution timeout is
/// `execution_timeout_secs` (`0` = unbounded).
///
/// An unbounded job used to mint a token good for six minutes, because `0` was
/// pushed through a `clamp(60, …)` that read it as "one minute" rather than as
/// "no limit". The ceiling is the same 24 hours a declared timeout may ask for.
pub fn ci_job_token_ttl_secs(execution_timeout_secs: u64) -> i64 {
    let execution = if execution_timeout_secs == 0 {
        JOB_TIMEOUT_MAX_SECS
    } else {
        i64::try_from(execution_timeout_secs)
            .unwrap_or(JOB_TIMEOUT_MAX_SECS)
            .min(JOB_TIMEOUT_MAX_SECS)
    };
    execution.max(CI_JOB_TOKEN_MIN_LIFETIME_SECS) + CI_JOB_TOKEN_GRACE_SECS
}

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
    /// Where the ref stood before this event, for the `paths:` / `paths-ignore:`
    /// filters — they ask *which files changed*, and that is a diff, not a
    /// property of the new commit alone.
    ///
    /// `None` falls back to the commit's first parent, which is exact for a
    /// merge commit or a single-commit push and an under-approximation for a
    /// fast-forward of several. The push transports know the real answer and
    /// pass it; a producer that has no previous revision (a manual run, a PR
    /// trigger) leaves it `None`. The zero sha — the git protocol's "this ref
    /// did not exist" — is treated the same as `None`.
    pub previous_sha: Option<&'a str>,
    /// Named values supplied by a `workflow_dispatch` caller. Other producers
    /// pass `None`; an explicit empty map is equivalent to omitting `inputs`.
    pub inputs: Option<&'a HashMap<String, String>>,
    pub triggered_by: Option<i64>,
    pub docker_enabled: bool,
    pub external_runners: bool,
    /// Whether jobs without an `image:` may run as a shell directly on the host.
    /// Defaults to `false` (secure): untrusted CI config must not execute on the
    /// server. When false, imageless jobs are refused unless dispatched to a
    /// Docker container or an external runner.
    pub allow_host_runner: bool,
    pub jwt_secret: Option<&'a str>,
    /// Secret the repository's CI secrets are encrypted with.
    ///
    /// Separate from [`jwt_secret`](Self::jwt_secret) since card_d740512de0a8:
    /// they used to be one value, so rotating the token-signing secret silently
    /// made every stored CI secret undecryptable and each job failed on its own
    /// with "failed to decrypt CI secret". `None` = no key, so no repository
    /// secrets are injected into the job environment.
    pub encryption_key: Option<&'a str>,
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
    /// See [`TriggerPipelineParams::encryption_key`].
    pub encryption_key: Option<&'a str>,
    pub external_url: Option<&'a str>,
}

/// The question [`CiTrigger::has_workflow_for_event`] answers.
///
/// A struct rather than five positional arguments because the gate and
/// [`TriggerPipelineParams`] have to agree on every one of them: the gate that
/// takes fewer inputs than the matcher it stands in front of is a gate that
/// answers a different question, which is how `previous_sha` would have gone
/// missing here while the trigger had it.
pub struct WorkflowEventQuery<'a> {
    pub repo_path: &'a Path,
    pub commit_sha: &'a str,
    pub event: &'a str,
    pub ref_name: &'a str,
    /// See [`TriggerPipelineParams::base_branch`].
    pub base_branch: Option<&'a str>,
    /// See [`TriggerPipelineParams::previous_sha`].
    pub previous_sha: Option<&'a str>,
}

/// Identity of an automatic CI run whose repository-owned configuration was
/// rejected before the normal pipeline graph could be published.
pub struct ConfigurationFailureParams<'a> {
    pub db: &'a DatabaseConnection,
    pub repo_id: i64,
    pub commit_sha: &'a str,
    pub ref_name: &'a str,
    pub trigger_type: &'a str,
    pub triggered_by: Option<i64>,
    /// The branch this run's `on:` filters would have been matched against —
    /// the same value the producer hands [`TriggerPipelineParams::base_branch`]
    /// on the path that succeeds. See [`publish_configuration_failure`] for why
    /// a diagnostic row has to carry it.
    pub base_branch: Option<&'a str>,
    /// Where the ref stood before the event that produced this run, the same
    /// value the producer hands [`TriggerPipelineParams::previous_sha`].
    pub previous_sha: Option<&'a str>,
}

/// Publish a terminal pipeline for a repository-owned CI configuration error.
///
/// Automatic push and pull-request producers run after the operation that made
/// the commit or PR visible. They therefore cannot return an `InvalidRequest`
/// to the original caller, but dropping it leaves no run on the pipelines page
/// and makes validly committed work look as if CI simply did not start.
///
/// Only the typed [`crate::error::InvalidRequest`] message is persisted. Bare
/// database, Git, and filesystem failures may contain operator paths or other
/// internal context and return `Ok(None)` so callers keep them in server logs.
/// The synthetic graph is committed atomically and already terminal, so no
/// runner can observe or claim its diagnostic job.
///
/// card_32422b3fdab1: the row records the event context its producer was
/// holding, not just the ref and the commit. `retry` accepts a pipeline
/// whatever its status, and "run the failed one again" is the most likely
/// retry of all — so this row is *more* likely to be replayed than a healthy
/// one, not less. Written without its `base_branch`, the retry of a pull
/// request into a non-default branch is judged against the default branch
/// instead: its `branches:` workflow then selects nothing and the run falls
/// through to `.forgekeep-ci.yml`, so a run that honestly failed on "CI
/// configuration rejected" can come back green on a different graph under the
/// same `201` (card_74d58ec3ac1e, for the read half).
///
/// `concurrency_group` and `dispatch_inputs` stay `NULL` on purpose and are not
/// on [`ConfigurationFailureParams`]: the group is resolved out of the very
/// workflow that was rejected, so no producer here has one, and this row is
/// already terminal and must not join a group it could cancel. No automatic
/// producer of a configuration failure carries dispatch inputs — a manual run
/// answers its caller directly instead of leaving a diagnostic row.
pub async fn publish_configuration_failure(
    params: ConfigurationFailureParams<'_>,
    error: &anyhow::Error,
) -> Result<Option<i64>> {
    let Some(reason) = error
        .downcast_ref::<crate::error::InvalidRequest>()
        .map(|invalid| invalid.message.clone())
    else {
        return Ok(None);
    };

    let ConfigurationFailureParams {
        db,
        repo_id,
        commit_sha,
        ref_name,
        trigger_type,
        triggered_by,
        base_branch,
        previous_sha,
    } = params;
    let now = chrono::Utc::now().naive_utc();
    let log = format!("CI configuration rejected before any job could run.\n\n{reason}\n");
    let tx = db
        .begin()
        .await
        .context("db: begin CI configuration failure transaction")?;
    let pipeline_id = match async {
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline_row(
            &tx,
            rg_db::ops::pipeline_ops::NewPipeline {
                repo_id,
                commit_sha,
                ref_name,
                trigger_type,
                triggered_by,
                concurrency_group: None,
                dispatch_inputs: None,
                base_branch,
                previous_sha,
            },
        )
        .await?;
        let stage =
            rg_db::ops::pipeline_ops::create_stage(&tx, pipeline.id, "configuration", 0).await?;
        let job = rg_db::ops::pipeline_ops::create_job(
            &tx,
            stage.id,
            "CI configuration",
            "",
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .await?;
        rg_db::ops::pipeline_ops::update_job_result(
            &tx,
            job.id,
            "failed",
            Some(1),
            Some(&log),
            Some(now),
            Some(now),
        )
        .await?;
        rg_db::ops::pipeline_ops::update_stage_status(
            &tx,
            stage.id,
            "failed",
            Some(now),
            Some(now),
        )
        .await?;
        rg_db::ops::pipeline_ops::update_pipeline_status(
            &tx,
            pipeline.id,
            "failed",
            Some(now),
            Some(now),
        )
        .await?;
        Ok::<i64, anyhow::Error>(pipeline.id)
    }
    .await
    {
        Ok(pipeline_id) => pipeline_id,
        Err(error) => {
            if let Err(rollback_error) = tx.rollback().await {
                tracing::error!(
                    repo_id,
                    commit_sha,
                    error = %format!("{rollback_error:#}"),
                    "CI configuration failure graph could not be rolled back"
                );
            }
            return Err(error.context("db: publish CI configuration failure graph"));
        }
    };
    tx.commit()
        .await
        .context("db: commit CI configuration failure graph")?;
    Ok(Some(pipeline_id))
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
    /// `previous_sha` is the same value the trigger takes (see
    /// [`TriggerPipelineParams::previous_sha`]) and for the same reason: this
    /// gate has to answer with the *same* filters the trigger will apply, or it
    /// refuses a workflow the trigger would have run — or the other way round.
    fn has_workflow_for_event(&self, query: WorkflowEventQuery<'_>) -> bool;

    /// Fallible form of [`Self::has_workflow_for_event`].
    ///
    /// Test doubles and engines whose event probe cannot fail inherit the bool
    /// contract. The production engine overrides this so an unreadable or
    /// invalid workflow does not collapse into the same `false` as a valid
    /// workflow that simply does not select this event.
    fn has_workflow_for_event_checked(&self, query: WorkflowEventQuery<'_>) -> Result<bool> {
        Ok(self.has_workflow_for_event(query))
    }

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
mod configuration_failure_tests {
    use super::{publish_configuration_failure, ConfigurationFailureParams};
    use crate::test_support::migrated_memory_database;
    use sea_orm::{DatabaseConnection, NotSet, Set};

    async fn repository(db: &DatabaseConnection) -> rg_db::entities::repository::Model {
        let user = rg_db::ops::user_ops::create_user(
            db,
            "config-failure-owner",
            "config-failure@example.invalid",
            "",
            "Config Failure",
        )
        .await
        .expect("create repository owner");
        let now = chrono::Utc::now();
        rg_db::ops::repo_ops::create(
            db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("config-failure".into()),
                description: Set(None),
                is_private: Set(true),
                default_branch: Set("main".into()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .expect("create repository")
    }

    #[tokio::test]
    async fn only_a_typed_configuration_refusal_becomes_a_terminal_visible_graph() {
        let db = migrated_memory_database().await;
        let repo = repository(&db).await;
        let params = || ConfigurationFailureParams {
            db: &db,
            repo_id: repo.id,
            commit_sha: "0123456789012345678901234567890123456789",
            ref_name: "refs/pull/7/head",
            trigger_type: crate::pull_request::ci::PULL_REQUEST_EVENT,
            triggered_by: Some(repo.owner_id),
            base_branch: Some("develop"),
            previous_sha: None,
        };
        let refusal =
            crate::error::invalid_request("unsupported key `types` in .gitea/workflows/pr.yml");

        let pipeline_id = publish_configuration_failure(params(), &refusal)
            .await
            .expect("publish the diagnostic graph")
            .expect("a typed refusal must be published");
        let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
            .await
            .expect("read pipeline")
            .expect("pipeline exists");
        let stages = rg_db::ops::pipeline_ops::list_stages_by_pipeline(&db, pipeline_id)
            .await
            .expect("read stages");
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, stages[0].id)
            .await
            .expect("read jobs");

        assert_eq!(pipeline.status, "failed");
        assert_eq!(
            pipeline.trigger_type,
            crate::pull_request::ci::PULL_REQUEST_EVENT
        );
        assert!(pipeline.started_at.is_some() && pipeline.finished_at.is_some());
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].name, "configuration");
        assert_eq!(stages[0].status, "failed");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "CI configuration");
        assert_eq!(jobs[0].status, "failed");
        assert_eq!(jobs[0].exit_code, Some(1));
        let log = jobs[0].log.as_deref().expect("diagnostic log");
        assert!(log.contains(".gitea/workflows/pr.yml"), "{log}");
        assert!(log.contains("types"), "{log}");

        // card_32422b3fdab1: the diagnostic row is written through the same
        // provenance-carrying constructor as a healthy one, because `retry`
        // takes it like any other row. What the producer did not hand over
        // stays `NULL` — including the concurrency group, which is resolved out
        // of the very workflow that was rejected and which this already
        // terminal row must never join.
        assert_eq!(pipeline.base_branch.as_deref(), Some("develop"));
        assert_eq!(pipeline.previous_sha, None);
        assert_eq!(pipeline.concurrency_group, None);
        assert_eq!(pipeline.dispatch_inputs, None);

        let infrastructure =
            anyhow::anyhow!("db: repository lookup failed at /srv/private/forgekeep.sqlite");
        assert_eq!(
            publish_configuration_failure(params(), &infrastructure)
                .await
                .expect("classification itself must not fail"),
            None,
            "operator-only infrastructure context must never become a repository-visible job log"
        );
        let (_, total) =
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 10)
                .await
                .expect("list repository pipelines");
        assert_eq!(total, 1, "the infrastructure failure created a second row");
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::{
        ci_job_token_ttl_secs, dispatched_job_timeout_secs, resolve_job_timeout_secs,
        JOB_TIMEOUT_MAX_SECS, JOB_TIMEOUT_MIN_SECS,
    };

    #[test]
    fn a_declared_timeout_inside_the_range_is_used_verbatim() {
        for declared in [JOB_TIMEOUT_MIN_SECS, 900, JOB_TIMEOUT_MAX_SECS] {
            assert_eq!(
                resolve_job_timeout_secs(1, Some(declared), 3600),
                declared as u64
            );
        }
    }

    /// No declared timeout is the ordinary "use the instance default" case,
    /// including the documented `0` = unbounded.
    #[test]
    fn an_absent_timeout_takes_the_server_default() {
        assert_eq!(resolve_job_timeout_secs(1, None, 3600), 3600);
        assert_eq!(resolve_job_timeout_secs(1, None, 0), 0);
        assert_eq!(
            resolve_job_timeout_secs(1, None, u64::MAX),
            JOB_TIMEOUT_MAX_SECS as u64
        );
    }

    /// A persisted value the validator would have refused cannot be honoured,
    /// so it falls back — but to one number shared by every consumer, not to
    /// whatever each call site happened to clamp or `unwrap_or` its way to.
    #[test]
    fn an_out_of_range_persisted_timeout_falls_back_to_one_shared_answer() {
        for declared in [-1, 0, JOB_TIMEOUT_MAX_SECS + 1, i64::MIN] {
            assert_eq!(resolve_job_timeout_secs(1, Some(declared), 3600), 3600);
        }
    }

    /// The token has to outlive the run it was issued for. An unbounded job
    /// used to mint a six-minute token, because `0` went through a
    /// `clamp(60, …)` that read it as one minute rather than as "no limit".
    #[test]
    fn the_job_token_outlives_the_run_it_was_issued_for() {
        assert!(ci_job_token_ttl_secs(900) > 900);
        assert!(
            ci_job_token_ttl_secs(1) >= 360,
            "a one-second job still has to fetch sources and report back"
        );
        assert_eq!(
            ci_job_token_ttl_secs(0),
            ci_job_token_ttl_secs(JOB_TIMEOUT_MAX_SECS as u64),
            "unbounded is the ceiling, not the floor"
        );
        assert!(ci_job_token_ttl_secs(u64::MAX) > JOB_TIMEOUT_MAX_SECS);
    }

    /// The regression this seam exists for: `timeouts.job_secs = 0` means "no
    /// timeout", and dispatching it as `1` killed every externally-run job a
    /// second after it started.
    #[test]
    fn an_unbounded_job_is_dispatched_as_the_ceiling_not_as_one_second() {
        assert_eq!(dispatched_job_timeout_secs(0), JOB_TIMEOUT_MAX_SECS);
        assert_eq!(dispatched_job_timeout_secs(900), 900);
        assert_eq!(dispatched_job_timeout_secs(1), JOB_TIMEOUT_MIN_SECS);
        assert_eq!(
            dispatched_job_timeout_secs(u64::MAX),
            JOB_TIMEOUT_MAX_SECS,
            "the wire field never carries a value the agent would refuse"
        );
    }
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
