//! Plombir Git CI/CD Engine.
//!
//! Parses `.plombir-git-ci.yml` or `.gitea/workflows/*.yml` (Gitea Actions format)
//! from the repository and executes pipelines.
//!
//! ## Native format (`.plombir-git-ci.yml`)
//!
//! ```yaml
//! stages:
//!   - build
//!   - test
//!
//! build_app:
//!   stage: build
//!   script:
//!     - cargo build
//! ```
//!
//! ## Gitea Actions format (`.gitea/workflows/*.yml`)
//!
//! ```yaml
//! name: CI
//! on: push
//! jobs:
//!   build:
//!     runs-on: ubuntu-latest
//!     steps:
//!       - uses: actions/checkout@v4
//!       - run: cargo build
//! ```

pub mod actions;
pub mod condition;
pub mod config;
pub mod gitea_actions;
mod matrix;
mod permissions;
pub mod runner;

use anyhow::{Context, Result};
use gix::bstr::ByteSlice;
use sea_orm::TransactionTrait;

use config::CiConfig;
use runner::PipelineRunner;

// M-14: TriggerPipelineParams and has_ci_config are now defined in rg-core.
// Re-export for backward compatibility with any code that still imports from rg_ci.
pub use rg_core::ci::{has_ci_config, ResumePipelineParams, TriggerPipelineParams};

/// The two post-push inputs that have no home in this crate: the WebSocket hub
/// (an `rg-http` type, reachable only through rg-core's [`PushNotifier`] seam)
/// and the SMTP configuration.
///
/// A pipeline finishing here can land a merge commit on a base branch, and that
/// move owes the same automation a push does. Everything reaching storage — the
/// pipeline, the webhooks, the notification rows — this crate could always do
/// on its own; the real-time `push` / `ci_triggered` events and the "pipeline
/// triggered" email it could not, because it built its [`PostPushContext`] out
/// of thin air with both fields `None` (card_85b8d59246b5). So the same merge
/// was loud when it came in over REST and silent when CI made it.
///
/// The process wires this once at startup ([`CiEngine::with_notifications`]) and
/// every trigger path inherits it, rather than each call site remembering to
/// pass a hub it may not have.
///
/// [`PushNotifier`]: rg_core::push_hooks::PushNotifier
/// [`PostPushContext`]: rg_core::push_hooks::PostPushContext
#[derive(Clone, Default)]
pub struct CiNotifications {
    /// Real-time sink. `None` = no WebSocket hub in this process (a CLI run).
    pub notifier: Option<std::sync::Arc<dyn rg_core::push_hooks::PushNotifier>>,
    /// `None` = no outgoing mail configured.
    pub smtp_config: Option<rg_core::email::SmtpConfig>,
}

/// CI engine implementation. Implements `rg_core::ci::CiTrigger` so that
/// `rg-http` can trigger pipelines without a direct dependency on `rg-ci`.
///
/// M-14: This struct decouples the HTTP layer from the CI engine crate.
#[derive(Clone)]
pub struct CiEngine {
    /// The hub and SMTP wiring the post-push hooks this engine spawns need.
    /// See [`CiNotifications`].
    notifications: CiNotifications,
    /// Instance-wide fallback for jobs that do not declare `timeout_seconds`.
    /// This belongs to the engine because it is process configuration, not a
    /// property every trigger producer should have to remember independently.
    job_timeout_secs: u64,
    /// The process-wide graceful-shutdown signal, when the embedder has one.
    ///
    /// Belongs to the engine for the same reason the job timeout does: every
    /// trigger producer spawns an embedded runner, and none of them should have
    /// to remember to hand it the signal. `None` for a test or a one-off
    /// `rg-cli` command. See [`crate::runner::PipelineRunner::set_shutdown`].
    shutdown: Option<tokio::sync::watch::Receiver<bool>>,
    /// Labels the embedded runner answers to (`ci.runner_labels`).
    ///
    /// Here rather than on every producer's params for the reason the job
    /// timeout is: it is process configuration. It decides two answers that
    /// have to agree — whether a hand-written `tags:` is refused when the
    /// pipeline is triggered, and whether the embedded runner will execute a
    /// job that carries labels at all.
    runner_labels: Vec<String>,
    /// The pool the embedded runners it spawns write through
    /// (`rg_db::open_write_pool`, card_a84b25c9efbe). Process configuration
    /// like the rest; `None` writes through the database the trigger hands in.
    write_pool: Option<sea_orm::DatabaseConnection>,
    /// `[limits]` — the budget the caches and artifacts its embedded runners
    /// publish are held to, the same one the runner routes check. Process
    /// configuration like the rest; unset by default.
    storage_limits: rg_core::storage_quota::StorageLimits,
    /// `[ci]` `job_memory` / `job_cpus` / `job_pids` — the ceiling of every job
    /// container its embedded runners start. Process configuration, set by the
    /// operator; no pipeline can reach it.
    job_limits: rg_process::docker_job::JobLimits,
    /// `[ci]` `actions_*` — where a workflow's `uses:` may load an action from.
    /// Process configuration, set by the operator; see [`actions::ActionsPolicy`].
    actions_policy: actions::ActionsPolicy,
}

impl Default for CiEngine {
    fn default() -> Self {
        Self {
            notifications: CiNotifications::default(),
            job_timeout_secs: rg_core::ci::DEFAULT_JOB_TIMEOUT_SECS,
            shutdown: None,
            runner_labels: rg_core::ci::default_runner_labels(),
            write_pool: None,
            storage_limits: rg_core::storage_quota::StorageLimits::default(),
            job_limits: rg_process::docker_job::JobLimits::default(),
            actions_policy: actions::ActionsPolicy::default(),
        }
    }
}

impl CiEngine {
    /// An engine with no real-time or mail wiring — everything that reaches
    /// storage still runs. Right for a process that has no hub (`rg-cli` one-off
    /// commands, tests).
    pub fn new() -> Self {
        Self::default()
    }

    /// An engine that fans its post-push effects out through this process's
    /// notification hub and SMTP configuration.
    pub fn with_notifications(notifications: CiNotifications) -> Self {
        Self {
            notifications,
            ..Self::default()
        }
    }

    /// An engine with process notification wiring and an explicit instance
    /// fallback for jobs that do not declare their own timeout.
    pub fn with_notifications_and_job_timeout(
        notifications: CiNotifications,
        job_timeout_secs: u64,
    ) -> Self {
        Self {
            notifications,
            job_timeout_secs,
            ..Self::default()
        }
    }

    /// Hand this engine the signal the process fans out on `SIGTERM`.
    ///
    /// Every embedded runner it spawns from here on observes it, hands the job
    /// it was holding straight back to `pending`, and unwinds — instead of
    /// being severed mid-build and leaving the row `running` for the
    /// ten-minute stuck-job sweep (card_34368880dc20). The unwind is what
    /// `rg_core::task_tracker::ci_tracker()` is drained for; without a drain
    /// the write would race the runtime teardown it is trying to survive.
    pub fn with_shutdown(mut self, shutdown: tokio::sync::watch::Receiver<bool>) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    /// Declare what the embedded runner this engine spawns actually is.
    ///
    /// `ci.runner_labels` in the instance configuration. Left alone it is
    /// [`rg_core::ci::default_runner_labels`], which claims the `ubuntu-*`
    /// spellings an Actions workflow carries as boilerplate plus the host's own
    /// shape — so an upgrade keeps running the workflows it was running, and a
    /// `runs-on:` naming something else is refused by name instead of being run
    /// here anyway.
    pub fn with_runner_labels(mut self, labels: Vec<String>) -> Self {
        self.runner_labels = labels;
        self
    }

    /// Send the writes of every embedded runner this engine spawns through
    /// `write_pool` — see [`crate::runner::PipelineRunner::set_write_pool`].
    pub fn with_write_pool(mut self, write_pool: sea_orm::DatabaseConnection) -> Self {
        self.write_pool = Some(write_pool);
        self
    }

    /// Hold the caches and artifacts every embedded runner this engine spawns
    /// publishes to `[limits]` — see
    /// [`crate::runner::PipelineRunner::set_storage_limits`].
    pub fn with_storage_limits(mut self, limits: rg_core::storage_quota::StorageLimits) -> Self {
        self.storage_limits = limits;
        self
    }

    /// Start every job container the embedded runners of this engine run under
    /// `limits` — see [`crate::runner::PipelineRunner::set_job_limits`].
    pub fn with_job_limits(mut self, limits: rg_process::docker_job::JobLimits) -> Self {
        self.job_limits = limits;
        self
    }

    /// Load the actions a workflow `uses:` under `policy` — `[ci]`
    /// `actions_sources`, `actions_upstream_url`, `actions_require_full_sha`
    /// and `actions_cache_max_mb`.
    pub fn with_actions_policy(mut self, policy: actions::ActionsPolicy) -> Self {
        self.actions_policy = policy;
        self
    }

    /// The `[ci]` rules for loading actions. Exposed for the same reason
    /// [`Self::job_timeout_secs`] is.
    pub fn actions_policy(&self) -> &actions::ActionsPolicy {
        &self.actions_policy
    }

    /// The instance-wide fallback handed to every embedded runner this engine
    /// creates. Exposed for startup diagnostics and contract tests.
    pub fn job_timeout_secs(&self) -> u64 {
        self.job_timeout_secs
    }

    /// The labels the embedded runner answers to. Exposed for the same reason
    /// [`Self::job_timeout_secs`] is.
    pub fn runner_labels(&self) -> &[String] {
        &self.runner_labels
    }

    /// The `[ci]` job container ceiling every embedded runner starts jobs
    /// under. Exposed for the same reason [`Self::job_timeout_secs`] is.
    pub fn job_limits(&self) -> &rg_process::docker_job::JobLimits {
        &self.job_limits
    }

    /// The `[limits]` every embedded runner is held to. Exposed for the same
    /// reason [`Self::job_timeout_secs`] is.
    pub fn storage_limits(&self) -> rg_core::storage_quota::StorageLimits {
        self.storage_limits
    }
}

async fn run_ci_config_gate<F>(check: F) -> Result<bool>
where
    F: FnOnce() -> Result<bool> + Send + 'static,
{
    tokio::task::spawn_blocking(check)
        .await
        .context("CI config gate blocking task failed")?
}

impl rg_core::ci::CiTrigger for CiEngine {
    fn has_ci_config(&self, repo_path: &std::path::Path, commit_sha: &str) -> bool {
        rg_core::ci::has_ci_config(repo_path, commit_sha)
    }

    fn has_ci_config_checked(&self, repo_path: &std::path::Path, commit_sha: &str) -> Result<bool> {
        rg_core::ci::has_ci_config_checked(repo_path, commit_sha)
    }

    fn has_ci_config_checked_async<'a>(
        &'a self,
        repo_path: &'a std::path::Path,
        commit_sha: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>> {
        let repo_path = repo_path.to_path_buf();
        let commit_sha = commit_sha.to_string();
        Box::pin(run_ci_config_gate(move || {
            rg_core::ci::has_ci_config_checked(&repo_path, &commit_sha)
        }))
    }

    fn has_workflow_for_event(&self, query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        let repo_path = query.repo_path;
        let event = query.event;
        match workflow_matches_event(query) {
            Ok(matched) => matched,
            Err(error) => {
                // Fail-closed, unlike `has_ci_config`: this gate answers "should
                // an event nobody asked for produce a pipeline", and the honest
                // answer to "the workflows are unreadable" is "not this one".
                // `trigger_pipeline` reports the same failure with its full
                // context on the paths that do reach it.
                tracing::warn!(
                    repo = %repo_path.display(),
                    event,
                    "cannot decide whether a workflow is triggered by this event: {:#}",
                    error
                );
                false
            }
        }
    }

    fn has_workflow_for_event_checked(
        &self,
        query: rg_core::ci::WorkflowEventQuery<'_>,
    ) -> Result<bool> {
        workflow_matches_event(query)
    }

    fn has_workflow_for_event_async<'a>(
        &'a self,
        query: rg_core::ci::WorkflowEventQuery<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>> {
        let query = rg_core::ci::OwnedWorkflowEventQuery::new(query);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || workflow_matches_event(query.as_query()))
                .await
                .context("workflow event evaluation did not complete")?
        })
    }

    fn workflow_dispatch_schema(
        &self,
        query: rg_core::ci::WorkflowDispatchSchemaQuery<'_>,
    ) -> Result<rg_core::ci::WorkflowDispatchSchema> {
        workflow_dispatch_schema(query.repo_path, query.commit_sha)
    }

    fn scheduled_workflows(
        &self,
        repo_path: &std::path::Path,
        commit_sha: &str,
    ) -> Result<Vec<rg_core::ci::WorkflowSchedule>> {
        scheduled_workflows(repo_path, commit_sha)
    }

    fn expand_deferred_matrix<'a>(
        &'a self,
        db: &'a sea_orm::DatabaseConnection,
        placeholder: &'a rg_db::entities::pipeline_job::Model,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<rg_db::entities::pipeline::Model>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(expand_deferred_matrix(db, placeholder))
    }

    fn decide_deferred_condition<'a>(
        &'a self,
        db: &'a sea_orm::DatabaseConnection,
        job: &'a rg_db::entities::pipeline_job::Model,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<rg_db::entities::pipeline::Model>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(decide_deferred_condition(db, job))
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
        Box::pin(trigger_pipeline_with_engine(params, self))
    }

    fn resume_pipeline<'a>(
        &'a self,
        params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(resume_pipeline_with_engine(params, self))
    }
}

/// The trait's `has_ci_config_checked` carries a fail-open default — `Ok` of
/// the bool — so that the dozens of test doubles keep compiling. That default
/// is exactly the wrong answer for the shipped engine: inheriting it would put
/// the merge queue back where it started, asking a fallible question and
/// getting a confident "no CI config" out of a repository nobody could open,
/// with the call site's `?` never firing (card_64dd8532326d). This holds the
/// override in place.
#[cfg(test)]
mod engine_ci_config_gate_tests {
    use super::CiEngine;
    use rg_core::ci::CiTrigger;

    #[tokio::test(flavor = "current_thread")]
    async fn a_delayed_config_read_leaves_the_async_worker_free() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            release_tx.send(()).expect("release CI config read");
        });
        let read = tokio::spawn(super::run_ci_config_gate(move || {
            entered_tx.send(()).expect("signal CI config read");
            release_rx.recv().expect("wait for delayed CI config read");
            Ok(true)
        }));
        entered_rx.await.expect("blocking CI config read starts");
        tokio::time::timeout(std::time::Duration::from_millis(500), async {
            tokio::spawn(async {}).await.expect("cheap task joins");
        })
        .await
        .expect("delayed CI config read held the only async worker");
        assert!(read.await.unwrap().unwrap());
        release.join().expect("release thread joins");
    }

    #[tokio::test]
    async fn async_config_gate_keeps_the_checked_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("not-a-repository.git");
        let error = CiEngine::new()
            .has_ci_config_checked_async(&missing, "deadbeef")
            .await
            .expect_err("unreadable Git remains unknown, not no config");
        assert!(format!("{error:#}").contains("failed to open repository"));
    }

    #[test]
    fn the_engine_reports_an_unopenable_repository_instead_of_inheriting_the_fail_open_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("not-a-repository.git");

        let error = CiEngine::new()
            .has_ci_config_checked(&missing, "deadbeef")
            .expect_err("the engine must not answer `Ok(false)` for a repository it cannot open");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("failed to open repository"),
            "the engine must forward the open failure verbatim: {rendered}"
        );

        // The bool form keeps its fail-open contract; the two must differ, or
        // the checked one buys the queue nothing.
        assert!(
            !CiEngine::new().has_ci_config(&missing, "deadbeef"),
            "the bool gate stays fail-open so a broken repository never blocks a push"
        );
    }

    /// A workflow set at the byte ceiling: sixteen files of large `env:` maps,
    /// none of which `push` triggers, so matching a push parses every one.
    fn heavy_workflows() -> Vec<(String, Vec<u8>)> {
        (0..16)
            .map(|file| {
                let mut yaml = String::from(
                    "on: pull_request\njobs:\n  build:\n    runs-on: ubuntu-latest\n    env:\n",
                );
                for key in 0..24_000 {
                    yaml.push_str(&format!("      KEY_{file}_{key}: value-{key}\n"));
                }
                yaml.push_str("    steps:\n      - run: echo ok\n");
                (
                    format!(".gitea/workflows/w{file:02}.yml"),
                    yaml.into_bytes(),
                )
            })
            .collect()
    }

    /// card_57ccfa9fdde3: deciding whether an event triggers a workflow reads
    /// and parses every workflow at the commit — work the pushed content sizes.
    /// On a one-worker runtime a cheap task keeps getting scheduled while the
    /// evaluation runs: it is on a blocking thread, not on the worker. The
    /// evaluation itself has to outlast the bound several times over, or the
    /// fixture proves nothing.
    #[test]
    fn workflow_evaluation_leaves_the_async_worker_free() {
        const BOUND: std::time::Duration = std::time::Duration::from_millis(500);
        let files = heavy_workflows();
        let borrowed: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_slice()))
            .collect();
        let (repo, sha) = super::matrix_tests::commit_repo(&borrowed);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async {
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            // The clock starts before the evaluation does, and the ticker is
            // already asleep in its first tick when the evaluation begins: a
            // ticker that first ran after a blocking evaluation would start its
            // clock late and never see the stall it is here to measure.
            let mut last = std::time::Instant::now();
            let ticker = tokio::spawn({
                let stop = stop.clone();
                async move {
                    let mut widest = std::time::Duration::ZERO;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        widest = widest.max(last.elapsed());
                        last = std::time::Instant::now();
                    }
                    widest
                }
            });

            tokio::task::yield_now().await;
            let started = std::time::Instant::now();
            let matched = CiEngine::new()
                .has_workflow_for_event_async(rg_core::ci::WorkflowEventQuery {
                    repo_path: repo.path(),
                    commit_sha: &sha,
                    event: "push",
                    ref_name: "refs/heads/main",
                    base_branch: None,
                    previous_sha: None,
                })
                .await
                .expect("the heavy workflow set is valid");
            let took = started.elapsed();
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let widest = ticker.await.unwrap();

            assert!(!matched, "no workflow here is triggered by a push");
            assert!(
                took > BOUND * 3,
                "the fixture is too light to prove anything: evaluation took {took:?}"
            );
            assert!(
                widest < BOUND,
                "a cheap task waited {widest:?} while a {took:?} evaluation held the worker"
            );
        });
    }

    /// The trigger's half: the config it builds the pipeline from is read on a
    /// blocking thread too. Pinned in the source because a pipeline cannot be
    /// triggered without a database this module's tests do not carry.
    #[test]
    fn the_trigger_reads_its_config_inside_a_blocking_boundary() {
        let source = include_str!("lib.rs");
        let function = "trigger_pipeline_with_barrier_and_engine";
        let boundary = super::matrix_tests::rust_source::production_function_call_sites(
            source,
            function,
            &["spawn_blocking"],
        );
        let reads = super::matrix_tests::rust_source::production_function_call_sites(
            source,
            function,
            &["read_ci_config_with_inputs"],
        );
        assert_eq!(boundary.len(), 1, "{boundary:?}");
        assert_eq!(reads.len(), 1, "{reads:?}");
        assert!(
            super::matrix_tests::rust_source::call_site_contains(source, boundary[0], reads[0]),
            "the trigger reads its CI config outside the blocking boundary"
        );
    }
}

/// Resume an already-created pipeline. External runners only need the job to
/// be moved back to `pending`; an internal runner is recreated from persisted
/// pipeline state and skips terminal jobs.
async fn resume_pipeline_with_engine(
    params: ResumePipelineParams<'_>,
    engine: &CiEngine,
) -> Result<()> {
    if params.external_runners {
        tracing::info!(
            pipeline_id = params.pipeline_id,
            "manual pipeline released for external runners"
        );
        return Ok(());
    }
    spawn_internal_runner(
        params.db,
        params.repo_root,
        params.repo_path,
        params.repo_id,
        params.pipeline_id,
        params.docker_enabled,
        params.allow_host_runner,
        params.jwt_secret,
        params.encryption_key,
        params.external_url,
        engine,
    );
    Ok(())
}

/// The group this trigger serializes on, refusing one that is not fully
/// resolved.
///
/// One check above both dialects rather than one per parser. A group that still
/// carries a `${{ … }}` this engine cannot expand is a **literal**, and a
/// literal group is shared by every ref of the repository — under
/// `cancel_in_progress` that is a standing order to cancel whatever else is
/// running. A group reading a name this engine has no value for —
/// `${{ github.actor }}` — lands here, and it has to land loudly: silently
/// degrading it to "no concurrency" is how two deploys end up running at once,
/// which is the very thing the block was written to prevent.
fn resolved_concurrency_group(
    concurrency: Option<&config::ConcurrencyConfig>,
    ref_name: &str,
) -> Result<Option<String>> {
    let Some(concurrency) = concurrency else {
        return Ok(None);
    };
    let group = rg_db::ops::pipeline_ops::resolve_concurrency_group(&concurrency.group, ref_name);
    if group.contains("${{") {
        return Err(rg_core::error::invalid_request(format!(
            "concurrency.group resolved to '{group}', which still contains an expression this \
             engine cannot evaluate. A group may use ${{{{ github.ref }}}}, \
             ${{{{ github.ref_name }}}}, ${{{{ github.workflow }}}}, ${{{{ github.sha }}}}, \
             ${{{{ github.event_name }}}}, ${{{{ github.repository }}}}, \
             ${{{{ github.repository_owner }}}}, ${{{{ github.head_ref }}}}, \
             ${{{{ github.base_ref }}}}, ${{{{ inputs.NAME }}}}, \
             ${{{{ github.event.inputs.NAME }}}}, ${{{{ vars.NAME }}}} — and ${{{{ ref }}}} / \
             ${{{{ branch }}}} in .plombir-git-ci.yml. Left unresolved it is one literal group shared by every ref of \
             this repository."
        )));
    }
    // Checked once resolved: `${{ github.ref }}` grows with the branch name.
    check_stored_name("concurrency.group", &group)?;
    Ok(Some(group))
}

/// Trigger a CI pipeline for a push event.
///
/// This function:
/// 1. Reads `.plombir-git-ci.yml` from the repo at the given commit
/// 2. Parses the CI configuration
/// 3. Checks concurrency control (if configured)
/// 4. Creates pipeline/stage/job records in the DB
/// 5. Spawns the pipeline runner in a background task, injecting CI_JOB_TOKEN
#[cfg(test)]
async fn trigger_pipeline(
    params: TriggerPipelineParams<'_>,
    notifications: &CiNotifications,
) -> Result<i64> {
    trigger_pipeline_with_barrier(params, notifications, None).await
}

async fn trigger_pipeline_with_engine(
    params: TriggerPipelineParams<'_>,
    engine: &CiEngine,
) -> Result<i64> {
    trigger_pipeline_with_barrier_and_engine(params, engine, None).await
}

/// Internal entrypoint with a rendezvous immediately before a grouped trigger
/// tries to acquire its database lock. Production passes `None`; concurrency
/// tests use the barrier to put two real `trigger_pipeline` executions at the
/// old empty-group race window without sleeps or scheduler luck.
#[cfg(test)]
async fn trigger_pipeline_with_barrier(
    params: TriggerPipelineParams<'_>,
    notifications: &CiNotifications,
    before_concurrency_lock: Option<&tokio::sync::Barrier>,
) -> Result<i64> {
    let engine = CiEngine::with_notifications(notifications.clone());
    trigger_pipeline_with_barrier_and_engine(params, &engine, before_concurrency_lock).await
}

/// The repository of this instance `owner/repo` names, for
/// [`actions::RepositoryActionLoader`]: where it lives on disk and whether it
/// is private. A soft-deleted repository is not there.
async fn instance_action_repository(
    db: &sea_orm::DatabaseConnection,
    repo_root: &std::path::Path,
    owner: &str,
    repo: &str,
) -> Result<Option<actions::InstanceRepository>> {
    let Some((repository, namespace)) =
        rg_core::repo::service::find_repo_and_namespace_by_owner_name(db, owner, repo).await?
    else {
        return Ok(None);
    };
    if repository.deleted_at.is_some() {
        return Ok(None);
    }
    Ok(Some(actions::InstanceRepository {
        path: repo_root.join(format!("{namespace}/{}.git", repository.name)),
        is_private: repository.is_private,
        id: repository.id,
    }))
}

async fn trigger_pipeline_with_barrier_and_engine(
    params: TriggerPipelineParams<'_>,
    engine: &CiEngine,
    before_concurrency_lock: Option<&tokio::sync::Barrier>,
) -> Result<i64> {
    let TriggerPipelineParams {
        db,
        repo_root,
        repo_path,
        repo_id,
        commit_sha,
        ref_name,
        trigger_type,
        base_branch,
        head_branch,
        previous_sha,
        inputs,
        schedule,
        triggered_by,
        docker_enabled,
        external_runners,
        allow_host_runner,
        jwt_secret,
        encryption_key,
        external_url,
    } = params;

    // 1. Read CI config from repo
    //
    // The identity is read first because the config conversion needs it, but its
    // failure is *held*: a broken workflow file has to arrive as a 400 even when
    // the pool is dead, and every case in
    // `a_broken_ci_config_is_the_clients_mistake_not_a_server_failure` reaches
    // this function with a connection that has no tables at all. So the file
    // gets its verdict first, and an unresolvable identity is re-raised below —
    // still before anything is written, so no pipeline is ever built against the
    // empty placeholder.
    let identity = rg_core::repo::service::repository_identity(db, repo_id).await;
    // Held the same way, for the same reason: the file is judged first. A
    // workflow that reads `vars.*` is judged against none until then.
    let variables = rg_db::ops::ci_variable_ops::for_repo(db, repo_id).await;
    let (identity_owner, identity_name) = identity
        .as_ref()
        .map(|(owner, name)| (owner.as_str(), name.as_str()))
        .unwrap_or(("", ""));
    // Reading the workflows means opening the repository, reading every
    // workflow blob (up to `MAX_WORKFLOW_TOTAL_BYTES`), parsing them and diffing
    // the event's trees for `paths:` — work whose size the pushed content picks.
    // It runs on a blocking thread, with copies of everything it reads, so a
    // heavy workflow cannot hold an async worker (card_57ccfa9fdde3).
    let mut config = {
        let repo_path = repo_path.to_path_buf();
        let (owner, name) = (identity_owner.to_string(), identity_name.to_string());
        let (commit_sha, ref_name, event) = (
            commit_sha.to_string(),
            ref_name.to_string(),
            trigger_type.to_string(),
        );
        let base_branch = base_branch.map(str::to_string);
        let head_branch = head_branch.map(str::to_string);
        let previous_sha = previous_sha.map(str::to_string);
        let inputs = inputs.cloned();
        let scheduled = schedule.map(|run| (run.workflow.to_string(), run.cron.to_string()));
        let policy = engine.actions_policy().clone();
        let repo_root = repo_root.to_path_buf();
        let vars = variables.as_ref().cloned().unwrap_or_default();
        let lookup_db = db.clone();
        let lookup_root = repo_root.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            // The workflow is read on a blocking thread; the one database
            // question loading an action asks — which repository of this
            // instance is `owner/repo`, and may it be read — is answered by
            // handing it back to the runtime this thread was spawned from.
            let lookup = move |owner: &str, repo: &str| {
                runtime.block_on(instance_action_repository(
                    &lookup_db,
                    &lookup_root,
                    owner,
                    repo,
                ))
            };
            let loader = actions::RepositoryActionLoader {
                policy: &policy,
                workflow_repo: &repo_path,
                workflow_repo_id: repo_id,
                commit_sha: &commit_sha,
                repo_root: &repo_root,
                lookup: &lookup,
            };
            read_ci_config_with_inputs(
                &repo_path,
                RepositoryName {
                    owner: &owner,
                    name: &name,
                },
                &commit_sha,
                &ref_name,
                WorkflowInvocation {
                    event: &event,
                    base_branch: base_branch.as_deref(),
                    head_branch: head_branch.as_deref(),
                    previous_sha: previous_sha.as_deref(),
                    inputs: inputs.as_ref(),
                    schedule: scheduled
                        .as_ref()
                        .map(|(workflow, cron)| rg_core::ci::ScheduledRun { workflow, cron }),
                    actions: Some(&loader),
                    vars: Some(&vars),
                },
            )
        })
        .await
        .context("reading the CI configuration did not complete")??
    };
    validate_execution_semantics(&config)?;
    validate_runner_routing(&config, external_runners, engine.runner_labels())?;
    select_jobs_for_ref(&mut config, ref_name)?;
    // Held until every verdict about the client's file has been given, and still
    // before the first write.
    let (repo_owner, repo_name) = identity?;
    variables?;

    // 2-5. Concurrency control and graph publication — one transaction.
    //
    // The group is what the config asked to serialize on, so the group is what
    // is looked up. This used to search by `ref_name`, which answered a
    // different question in both directions: a fixed group such as
    // `deploy-production`, shared by `main` and `release/*`, did not serialize
    // across them — the very reason a fixed name is written — while two
    // workflows declaring *different* groups on one branch cancelled each
    // other's pipelines. Only pipelines carrying the same group are this
    // trigger's business; a workflow that declared no `concurrency:` block
    // carries `NULL` and is neither waited for nor cancelled.
    let concurrency_group = resolved_concurrency_group(config.concurrency.as_ref(), ref_name)?;
    // What the caller typed, kept as the caller typed it, so a retry can run
    // this pipeline again with the same values (card_24f475c09a17). Only a
    // manual run has them: every other event reaches this function with
    // `inputs: None`, and an empty map is the documented spelling of "none".
    let dispatch_inputs = persisted_dispatch_inputs(trigger_type, inputs)?;
    let tx = db
        .begin()
        .await
        .context("db: begin pipeline concurrency/publication transaction")?;
    let pipeline_id = match async {
        if let (Some(concurrency), Some(group)) = (config.concurrency.as_ref(), &concurrency_group)
        {
            if let Some(barrier) = before_concurrency_lock {
                barrier.wait().await;
            }
            // An absent pipeline row cannot lock an empty group. The stable
            // `(repo_id, group)` row can: its UPSERT is the first statement in
            // this transaction, so another process targeting the same group
            // cannot perform the active read until this transaction commits.
            // Different groups use different rows on server databases; SQLite
            // retains its normal single-writer behavior without an additional
            // process-global mutex.
            rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(&tx, repo_id, group)
                .await?;
            let active =
                rg_db::ops::pipeline_ops::find_active_pipelines_by_group(&tx, repo_id, group)
                    .await?;

            if !active.is_empty() {
                if concurrency.cancel_in_progress {
                    tracing::info!(
                        concurrency_group = %group,
                        "Cancelling {} in-progress pipeline(s) for concurrency group",
                        active.len()
                    );
                    // Cancellation and replacement share this transaction. A
                    // refused child update rolls the old graph back to active;
                    // a failed replacement cannot commit a canceled predecessor
                    // without the graph that was supposed to take its place.
                    for pipeline in &active {
                        rg_db::ops::pipeline_ops::cancel_pipeline_chain_in_transaction(
                            &tx,
                            pipeline.id,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "ci: cancel in-progress pipeline {} of concurrency group '{}' — \
                                 the replacement pipeline was not started",
                                pipeline.id, group
                            )
                        })?;
                    }
                } else {
                    // A busy group is a *state* the caller can do something
                    // about: wait for the running pipeline, or set
                    // `cancel_in_progress`.
                    return Err(rg_core::error::conflict(format!(
                        "Concurrency group '{}' has {} active pipeline(s). \
                         Set cancel_in_progress: true to auto-cancel, or wait for them to finish.",
                        group,
                        active.len()
                    )));
                }
            }
        }

        // Every job row here is immediately schedulable work. Keeping the
        // entire graph under this transaction makes it visible all at once; for
        // grouped workflows the same commit also publishes the concurrency
        // decision that admitted it.
        PipelineGraph {
            repo_id,
            repository: RepositoryName {
                owner: &repo_owner,
                name: &repo_name,
            },
            commit_sha,
            ref_name,
            trigger_type,
            triggered_by,
            concurrency_group: concurrency_group.as_deref(),
            dispatch_inputs: dispatch_inputs.as_deref(),
            base_branch: persisted_replay_value(base_branch),
            head_branch: persisted_replay_value(head_branch),
            previous_sha: persisted_replay_value(previous_sha),
            schedule,
            config: &config,
        }
        .create(&tx)
        .await
    }
    .await
    {
        Ok(pipeline_id) => pipeline_id,
        Err(error) => {
            // The caller only ever sees the error that triggered the rollback,
            // so a rollback that fails can only be reported here.
            if let Err(rollback_error) = tx.rollback().await {
                tracing::error!(
                    repo_id,
                    commit_sha,
                    error = %format!("{rollback_error:#}"),
                    "pipeline concurrency/publication failed and its transaction could not be rolled back — \
                     the group may contain a partially updated graph"
                );
            }
            return Err(error);
        }
    };
    tx.commit()
        .await
        .context("db: commit pipeline creation transaction")?;

    // The graph is published; work that was terminal on arrival — a matrix leg
    // its `if:` excluded, a stage every job of which was skipped — still needs
    // the verdict no job result will produce for it. One transaction, because
    // this request is the only thing that will ever run this roll-up: a fault
    // between a stage write and the pipeline write used to leave terminal
    // stages under a pipeline still calling itself `pending`, with nothing
    // durable to come back for it (card_fff21eed27fe).
    rg_db::ops::pipeline_ops::settle_initial_graph(db, pipeline_id).await?;

    if rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id)
        .await?
        .is_some_and(|pipeline| pipeline.status == "success")
    {
        evaluate_initial_success(
            db,
            repo_root,
            repo_id,
            commit_sha,
            docker_enabled,
            external_runners,
            allow_host_runner,
            jwt_secret,
            encryption_key,
            external_url,
            engine,
        )
        .await;
        return Ok(pipeline_id);
    }

    // 6. Spawn pipeline runner in background (only if not using external runners)
    if !external_runners {
        spawn_internal_runner(
            db,
            repo_root,
            repo_path,
            repo_id,
            pipeline_id,
            docker_enabled,
            allow_host_runner,
            jwt_secret,
            encryption_key,
            external_url,
            engine,
        );
    } else {
        if let Some(first_stage) =
            rg_db::ops::pipeline_ops::list_stages_by_pipeline(db, pipeline_id)
                .await?
                .into_iter()
                .next()
        {
            // Same reason as the roll-up above: no runner follows this
            // request on the external path, so the stage's gate and the
            // pipeline's have to become visible together or not at all.
            rg_db::ops::pipeline_ops::pause_initial_stage_at_manual(
                db,
                pipeline_id,
                first_stage.id,
            )
            .await?;
        }
        tracing::info!(
            pipeline_id = pipeline_id,
            "Pipeline created with external runner mode — jobs will be picked up by registered runners"
        );
    }

    Ok(pipeline_id)
}

/// The `workflow_dispatch` inputs a pipeline row has to carry, as JSON.
///
/// The manual event is the only one that has any: every other producer passes
/// `inputs: None`, and an explicit empty map is documented as equivalent to
/// omitting them — so both become `None`, which is what the column means.
///
/// The values are stored **unresolved**, exactly as the caller supplied them.
/// A retry rebuilds the same commit, so re-reading the workflow re-applies the
/// same `type:` / `default:` / `required:` declarations and reproduces the
/// original run; persisting the resolved map instead would write defaults into
/// the provenance of a run that never asked for them, and could not tell a
/// caller who chose the default value from one who omitted it.
fn persisted_dispatch_inputs(
    trigger_type: &str,
    inputs: Option<&std::collections::HashMap<String, String>>,
) -> Result<Option<String>> {
    if trigger_type != rg_core::ci::WORKFLOW_DISPATCH_EVENT {
        return Ok(None);
    }
    let Some(inputs) = inputs.filter(|inputs| !inputs.is_empty()) else {
        return Ok(None);
    };
    // A `HashMap<String, String>` cannot fail to serialize, but a silent
    // `unwrap_or(None)` here would be a retry that quietly loses its inputs.
    let stored = serde_json::to_string(inputs).context("serialize workflow_dispatch inputs")?;
    // The HTTP boundary refuses an oversized map before any git work; this is
    // the same check where the row is built, so no producer can hand MySQL a
    // document its `TEXT` column rejects with a 500 (card_2a0f4ba3b59d).
    rg_core::ci::ensure_dispatch_inputs_fit(inputs, stored.len())?;
    Ok(Some(stored))
}

/// One value of a run's replay context as the pipeline row should carry it.
///
/// The column means "the producer handed this over", so a value that says
/// nothing must not be stored as though it did: an empty `base_branch` would
/// read back as a branch named `""` and match no `branches:` filter at all,
/// where `None` correctly falls back to the repository's default branch. The
/// zero sha *is* kept — "this ref did not exist" is a real answer about the
/// push, and the filter treats it as such wherever it is read (card_74d58ec3ac1e).
fn persisted_replay_value(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

/// Apply the native `only:` selector before pipeline publication has any side
/// effect — including acquiring a concurrency lock or cancelling an older run.
///
/// `validate_execution_semantics` deliberately runs first: an invalid job does
/// not become valid merely because this particular ref excludes it. Once the
/// file is known to be sound, however, selecting no work is not a configuration
/// failure and automatic producers must be able to distinguish it from one.
fn select_jobs_for_ref(config: &mut CiConfig, ref_name: &str) -> Result<()> {
    let ref_short = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
    config.jobs.retain(|_, job| {
        job.only.as_ref().is_none_or(|only| {
            only.iter()
                .any(|pattern| pattern == ref_short || pattern == ref_name)
        })
    });
    if config.jobs.is_empty() {
        return Err(anyhow::Error::new(rg_core::ci::NoMatchingCiJobs::new(
            ref_name,
        )));
    }
    Ok(())
}

/// Everything one trigger has to write before its pipeline exists: the pipeline
/// row, its stages, and every job of every stage.
///
/// Kept together so the whole graph can be written through a single
/// transaction — a pipeline is either declared in full or not at all.
struct PipelineGraph<'a> {
    repo_id: i64,
    /// `<owner>/<name>`, for the `github.repository` half of the condition
    /// context. Every job's `if:` is evaluated here, so this is the second place
    /// the identity has to reach — the config-conversion context in
    /// `gitea_actions` is the first, and a value present in one and empty in the
    /// other is how a condition comes to mean different things at two hops of
    /// the same trigger.
    repository: RepositoryName<'a>,
    commit_sha: &'a str,
    ref_name: &'a str,
    trigger_type: &'a str,
    triggered_by: Option<i64>,
    /// The resolved `concurrency.group` this pipeline joins, or `None` when the
    /// config declared no `concurrency:` block. Written onto the pipeline row so
    /// the *next* trigger can find it — the group was previously computed,
    /// logged, and thrown away (card_4c5214698ae9).
    concurrency_group: Option<&'a str>,
    /// The caller's `workflow_dispatch` inputs as JSON, or `None` for every
    /// producer that has none. Written onto the pipeline row so a retry can run
    /// the same build again with the values it was started with — the row used
    /// to record the event but not what the caller typed into it, so retrying a
    /// manual run either substituted the workflow's defaults or was refused for
    /// a required input the caller had already supplied (card_24f475c09a17).
    dispatch_inputs: Option<&'a str>,
    /// The branch this run's `on:` filters were matched against, or `None` for
    /// an event that targets no branch but the ref it carries. Written onto the
    /// row for the same reason as the two above: a retry that cannot read it
    /// back matches against the repository's default branch, which is a
    /// different question for every pull request that does not target it
    /// (card_74d58ec3ac1e).
    base_branch: Option<&'a str>,
    /// The branch a `pull_request` run's pull request comes from, or `None` for
    /// every other event. Written onto the row because the runners read it
    /// there, as `github.head_ref`, and so does a retry.
    head_branch: Option<&'a str>,
    /// Where the ref stood before this event, or `None` for a producer with no
    /// previous revision. Written onto the row so a retry's `paths:` filters see
    /// the diff the original event saw, and not the narrower one against the
    /// commit's first parent (card_74d58ec3ac1e).
    previous_sha: Option<&'a str>,
    /// The cron entry a `schedule` run fired for, written onto the row for the
    /// retry that has to select the same file again, and read by a job
    /// condition as `github.event.schedule`.
    schedule: Option<rg_core::ci::ScheduledRun<'a>>,
    config: &'a CiConfig,
}

impl PipelineGraph<'_> {
    /// Write the graph through `tx` and return the new pipeline's id.
    ///
    /// Nothing written here is schedulable until the caller commits, so the
    /// intermediate states — a pipeline with no stages, a stage missing half its
    /// jobs, a protected job not yet gated behind its environment — are never
    /// observable by a runner.
    async fn create(&self, tx: &sea_orm::DatabaseTransaction) -> Result<i64> {
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline_row(
            tx,
            rg_db::ops::pipeline_ops::NewPipeline {
                repo_id: self.repo_id,
                commit_sha: self.commit_sha,
                ref_name: self.ref_name,
                trigger_type: self.trigger_type,
                triggered_by: self.triggered_by,
                concurrency_group: self.concurrency_group,
                dispatch_inputs: self.dispatch_inputs,
                base_branch: self.base_branch,
                head_branch: self.head_branch,
                previous_sha: self.previous_sha,
                schedule: self.schedule.map(|run| (run.workflow, run.cron)),
            },
        )
        .await?;

        let pipeline_id = pipeline.id;

        // The list the *file* runs, not the list it spells: a job that names no
        // stage belongs to `default`, and nothing here used to create that
        // stage. `validate_execution_semantics` has already refused every job
        // whose named stage this list does not hold, so the map below covers
        // every job that reaches `create_jobs`.
        let stage_names = resolved_stage_order(self.config);
        let mut stage_id_map: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();

        for (order, stage_name) in stage_names.iter().enumerate() {
            let stage =
                rg_db::ops::pipeline_ops::create_stage(tx, pipeline_id, stage_name, order as i32)
                    .await?;
            stage_id_map.insert(stage_name.clone(), stage.id);
        }

        self.create_jobs(tx, &stage_id_map).await?;
        Ok(pipeline_id)
    }

    async fn create_jobs(
        &self,
        tx: &sea_orm::DatabaseTransaction,
        stage_id_map: &std::collections::HashMap<String, i64>,
    ) -> Result<()> {
        let PipelineGraph {
            repo_id,
            repository,
            commit_sha,
            ref_name,
            trigger_type,
            base_branch,
            head_branch,
            schedule,
            config,
            ..
        } = *self;
        for (job_name, job_config) in &config.jobs {
            let stage_name = job_config.stage.as_deref().unwrap_or(DEFAULT_STAGE);
            // A job whose stage has no row is a job this pipeline cannot place,
            // and it used to be dropped here behind a server-side `warn!` the
            // author never sees: the file went green having never run it, and a
            // file that named no stages at all went green having run *nothing*
            // (card_d92cd3260864). The stage set now covers every job the
            // validator admitted, so reaching this arm means the two disagree —
            // and the whole transaction is abandoned rather than a pipeline
            // published without the job.
            let Some(&stage_id) = stage_id_map.get(stage_name) else {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{job_name}' names stage '{stage_name}', which this pipeline has no stage for"
                )));
            };

            // A matrix that reads a needed job's outputs has no variants yet:
            // the job is one placeholder until its stage is reached.
            if let Some(spec) = job_config
                .actions_matrix
                .as_ref()
                .filter(|spec| !spec.needs_outputs().is_empty())
            {
                create_deferred_matrix_job(tx, stage_id, job_name, job_config, spec).await?;
                continue;
            }
            let (head_ref, base_ref) =
                rg_core::ci::pull_request_refs(trigger_type, head_branch, base_branch);
            let rows = JobRowContext {
                repo_id,
                repository,
                commit_sha,
                ref_name,
                trigger_type,
                schedule_cron: schedule.map(|run| run.cron),
                head_ref,
                base_ref,
            };
            create_variant_jobs(
                tx,
                &rows,
                stage_id,
                job_name,
                job_config,
                expand_matrix(job_name, job_config)?,
            )
            .await?;
        }
        Ok(())
    }
}

/// What every job row of one pipeline is written against.
#[derive(Clone, Copy)]
struct JobRowContext<'a> {
    repo_id: i64,
    repository: RepositoryName<'a>,
    commit_sha: &'a str,
    ref_name: &'a str,
    trigger_type: &'a str,
    schedule_cron: Option<&'a str>,
    /// `github.head_ref` / `github.base_ref` ([`rg_core::ci::pull_request_refs`]).
    head_ref: &'a str,
    base_ref: &'a str,
}

/// Write the rows of one job of the graph, one per matrix variant: the job
/// itself, its steps and context, its matrix membership, its environment and
/// the verdict of its `if:`. The pipeline build calls it for every job, and a
/// matrix decided at run time calls it once its placeholder is taken.
async fn create_variant_jobs(
    tx: &sea_orm::DatabaseTransaction,
    rows: &JobRowContext<'_>,
    stage_id: i64,
    job_name: &str,
    job_config: &config::JobConfig,
    variants: Vec<MatrixVariant>,
) -> Result<()> {
    let JobRowContext {
        repo_id,
        repository,
        commit_sha,
        ref_name,
        trigger_type,
        ..
    } = *rows;
    for variant in variants {
        let fields = resolve_action_job_fields(
            job_name,
            job_config,
            &variant,
            ref_name,
            trigger_type,
            commit_sha,
            repository,
        )?;
        // Both executors refuse a value that is not an image reference
        // before it reaches `docker run`, but by then the pipeline is
        // published and the job just fails. Checked here, after a
        // matrix value has been rendered into it, the author gets the
        // rule back when the file is read (card_857c0d4ab7a6).
        if let Some(image) = fields.image.as_deref() {
            rg_process::image_reference::validate_image_reference(image).map_err(|rule| {
                rg_core::error::invalid_request(format!("job '{}': {rule}", variant.name))
            })?;
        }
        // A name a process cannot carry is never delivered, and
        // `PATH=/tmp` would be read as `PATH`: that is the file being
        // wrong, so it is refused here. A *valid* name the runner keeps
        // for itself (`CI`, `LANG`, `HTTP_PROXY`) is not — the runner
        // drops it and says so in the job log (card_a79579f79c29).
        if let Some(name) = variant
            .variables
            .keys()
            .find(|name| !rg_process::job_environment::valid_environment_name(name))
        {
            return Err(rg_core::error::invalid_request(format!(
                "job '{}' sets the variable '{}', which is not a valid environment \
                     variable name (letters, digits and '_', not starting with a digit)",
                variant.name,
                name.escape_debug()
            )));
        }
        let tags_json = fields
            .tags
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize job tags")?;
        let variables_json = if variant.variables.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&variant.variables)?)
        };
        // A path that was a template is this variant's (`out-linux`); both
        // runners check it stays inside the workspace before they touch it.
        let cache_paths_json = job_config
            .cache
            .as_ref()
            .map(|cache| serde_json::to_string(fields.cache_paths.as_ref().unwrap_or(&cache.paths)))
            .transpose()?;
        // Resolved per variant, so a matrix job's artifacts are named
        // after the variant that produced them rather than all landing
        // under one name where each run overwrites the last.
        let artifacts_json = match job_config.artifacts.as_ref() {
            Some(artifacts) => {
                let rendered;
                let artifacts = if fields.artifact_name.is_some() || fields.artifact_paths.is_some()
                {
                    rendered = config::ArtifactsConfig {
                        name: fields
                            .artifact_name
                            .clone()
                            .or_else(|| artifacts.name.clone()),
                        paths: fields
                            .artifact_paths
                            .clone()
                            .unwrap_or_else(|| artifacts.paths.clone()),
                    };
                    &rendered
                } else {
                    artifacts
                };
                let (name, paths) = artifact_spec(&variant.name, artifacts).map_err(|error| {
                    rg_core::error::invalid_request(format!(
                        "job '{}' has an unusable artifacts block: {error}",
                        variant.name
                    ))
                })?;
                Some(serde_json::to_string(&serde_json::json!({
                    "name": name,
                    "paths": paths,
                }))?)
            }
            None => None,
        };
        let job = rg_db::ops::pipeline_ops::create_job(
            tx,
            stage_id,
            &variant.name,
            &job_config.shell_script(),
            fields.image.as_deref(),
            tags_json.as_deref(),
            variables_json.as_deref(),
            job_config.cache.as_ref().map(|cache| cache.key.as_str()),
            cache_paths_json.as_deref(),
            artifacts_json.as_deref(),
            job_config.allow_failure.unwrap_or(DEFAULT_ALLOW_FAILURE),
            job_config.timeout_seconds,
            job_when(job_config),
            job_config.condition.as_deref(),
        )
        .await?;
        if let Some(display_name) = &fields.display_name {
            rg_db::ops::pipeline_ops::set_job_display_name(tx, job.id, display_name).await?;
        }
        // Every variant names its matrix job and carries the job's
        // strategy: `fail-fast` and `max-parallel` are read off the rows.
        if let Some(strategy) = &job_config.matrix_strategy {
            rg_db::ops::pipeline_matrix_ops::set_matrix_membership(
                tx,
                job.id,
                job_name,
                strategy.fail_fast,
                strategy.max_parallel,
            )
            .await?;
        }
        // A job of steps carries them beside its flat `script`, and one
        // `pending` row per step, so the job page lists the steps
        // before a runner takes it. Written in the graph's
        // transaction: a job is never published without them.
        if let Some(context) = &job_config.job_context {
            rg_db::ops::pipeline_step_ops::set_job_context(
                tx,
                job.id,
                &serde_json::to_string(context).context("serialize job context")?,
            )
            .await?;
        }
        if let Some(steps) = &job_config.steps {
            // `- name: Test on ${{ matrix.os }}` is this variant's label.
            let context = ActionTemplateContext {
                variant: &variant,
                ref_name,
                event: trigger_type,
                sha: commit_sha,
                repository,
            };
            let steps = steps
                .iter()
                .cloned()
                .map(|mut step| {
                    step.name = step_label(&step.name, &context);
                    step
                })
                .collect::<Vec<_>>();
            let names = steps
                .iter()
                .map(|step| step.name.clone())
                .collect::<Vec<_>>();
            rg_db::ops::pipeline_step_ops::create_job_steps(
                tx,
                job.id,
                &serde_json::to_string(&steps).context("serialize job steps")?,
                &names,
            )
            .await?;
        }
        // Resolved before the job's own `if:` is read, because a name
        // no environment in this repository carries is the author's
        // mistake whether or not this particular run reaches the job.
        // Resolving it only on the runs that fire would hold the
        // refusal back until the first deploy that actually needed the
        // approval.
        let environment = match fields.environment.as_deref() {
            Some(environment_name) => Some((
                environment_name,
                resolve_job_environment(tx, repo_id, &variant.name, environment_name).await?,
            )),
            None => None,
        };
        let condition_context =
            || job_condition_context(rows, &variant.variables, &variant.coordinates);
        // A condition that reads what a needed job left, or how the needed
        // jobs ended, has no answer yet; any other is answered now, as it
        // always was.
        let deferred_condition = job_config.condition.as_deref().filter(|condition| {
            condition::reads_needs(condition) || !condition::status_functions(condition).is_empty()
        });
        let should_run = match job_config.condition.as_deref() {
            Some(condition) if deferred_condition.is_none() => {
                condition::evaluate_condition(condition, &condition_context())?
            }
            _ => true,
        };
        let needs = job_needs(job_config);
        if should_run && !needs.is_empty() {
            // Held until its stage is reached: then it runs, or is skipped
            // because its condition is false or a job it needs was skipped.
            // Its environment is attached only if it runs, so a job that is
            // skipped never asks anyone for an approval.
            let held = HeldJob {
                condition: deferred_condition.map(str::to_owned),
                needs,
                context: deferred_condition
                    .map(|_| condition_context().into_iter().collect())
                    .unwrap_or_default(),
                environment: environment.as_ref().map(|(name, _)| (*name).to_owned()),
            };
            rg_db::ops::pipeline_condition_ops::set_deferred_condition(
                tx,
                job.id,
                &serde_json::to_string(&held).context("serialize a held job")?,
            )
            .await?;
        } else if !should_run {
            let now = chrono::Utc::now().naive_utc();
            rg_db::ops::pipeline_ops::update_job_result(
                tx,
                job.id,
                "skipped",
                None,
                None,
                None,
                Some(now),
            )
            .await?;
        } else if let Some((environment_name, environment)) = environment {
            rg_db::ops::ci_environment_ops::attach_job(
                tx,
                job.id,
                Some(&environment),
                environment_name,
            )
            .await?;
        }
    }
    Ok(())
}

/// What a job whose matrix reads a needed job's outputs is built from once its
/// stage is reached: the job as the workflow translated it, including the
/// parts a committed file cannot set (and `JobConfig` therefore does not
/// serialize), and the matrix still to resolve.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DeferredMatrixJob {
    config: config::JobConfig,
    action_templates: Option<config::ActionJobTemplates>,
    steps: Option<Vec<rg_process::ci_steps::StepSpec>>,
    job_context: Option<rg_process::ci_steps::JobContext>,
    matrix: matrix::MatrixSpec,
    strategy: Option<matrix::MatrixStrategy>,
}

impl DeferredMatrixJob {
    fn of(job_config: &config::JobConfig, spec: &matrix::MatrixSpec) -> Self {
        Self {
            config: job_config.clone(),
            action_templates: job_config.action_templates.clone(),
            steps: job_config.steps.clone(),
            job_context: job_config.job_context.clone(),
            matrix: spec.clone(),
            strategy: job_config.matrix_strategy,
        }
    }

    fn job_config(self) -> config::JobConfig {
        config::JobConfig {
            action_templates: self.action_templates,
            steps: self.steps,
            job_context: self.job_context,
            actions_matrix: Some(self.matrix),
            matrix_strategy: self.strategy,
            ..self.config
        }
    }
}

/// The one row a job whose matrix reads a needed job's outputs has until its
/// stage is reached. Its name is the job's; it is never handed to a runner,
/// and [`expand_deferred_matrix`] replaces it with the variants.
async fn create_deferred_matrix_job(
    tx: &sea_orm::DatabaseTransaction,
    stage_id: i64,
    job_name: &str,
    job_config: &config::JobConfig,
    spec: &matrix::MatrixSpec,
) -> Result<()> {
    let job = rg_db::ops::pipeline_ops::create_job(
        tx,
        stage_id,
        job_name,
        &job_config.shell_script(),
        None,
        None,
        None,
        None,
        None,
        None,
        job_config.allow_failure.unwrap_or(DEFAULT_ALLOW_FAILURE),
        job_config.timeout_seconds,
        None,
        job_config.condition.as_deref(),
    )
    .await?;
    let stored = serde_json::to_string(&DeferredMatrixJob::of(job_config, spec))
        .context("serialize a deferred matrix job")?;
    rg_db::ops::pipeline_matrix_ops::set_deferred_matrix(tx, job.id, &stored).await?;
    Ok(())
}

/// Replace the placeholder of a matrix that reads a needed job's outputs with
/// the variants those outputs make, now that the needed jobs have finished.
///
/// What the needed job left is the workflow's input like any other: when it
/// does not make a matrix — not JSON, not the right shape, past the 256-variant
/// cap, a variant the job cannot be written for — the placeholder fails with
/// the reason in its log and the pipeline rolls up, rather than the error
/// surfacing as this server's. A placeholder another caller already expanded
/// is left alone. `Some(pipeline)` when the failure finished the pipeline, for
/// the caller's completion hooks.
pub async fn expand_deferred_matrix(
    db: &sea_orm::DatabaseConnection,
    placeholder: &rg_db::entities::pipeline_job::Model,
) -> Result<Option<rg_db::entities::pipeline::Model>> {
    let Some(stored) = placeholder.deferred_matrix.as_deref() else {
        return Ok(None);
    };
    let stored: DeferredMatrixJob = serde_json::from_str(stored)
        .context("a deferred matrix job's stored form is unreadable")?;
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(db, placeholder.stage_id)
        .await?
        .with_context(|| {
            format!(
                "stage {} of a deferred matrix job is gone",
                placeholder.stage_id
            )
        })?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(db, stage.pipeline_id)
        .await?
        .with_context(|| {
            format!(
                "pipeline {} of a deferred matrix job is gone",
                stage.pipeline_id
            )
        })?;
    let (owner, name) = rg_core::repo::service::repository_identity(db, pipeline.repo_id).await?;
    let job_name = placeholder.name.clone();
    let spec = stored.matrix.clone();
    let job_config = stored.job_config();
    let needs = job_config
        .job_context
        .as_ref()
        .map(|context| context.needs.clone())
        .unwrap_or_default();
    let needed = rg_db::ops::pipeline_step_ops::needs_context(db, pipeline.id, &needs).await?;
    // A matrix job whose needed job was skipped is skipped whole: its matrix
    // would be read from outputs nobody wrote.
    if let Some(alias) = skipped_need(&needs, &needed) {
        let log = format!("Skipped: `{alias}`, a job this one needs, was skipped.\n");
        return rg_db::ops::pipeline_condition_ops::settle_held_job(
            db,
            placeholder,
            "skipped",
            &log,
        )
        .await;
    }
    let combinations = spec
        .resolve(|reference| {
            needed
                .get(&reference.job)
                .and_then(|job| job.outputs.get(&reference.output))
                .cloned()
        })
        .and_then(|matrix| matrix::combinations(&matrix));
    let variants = match combinations
        .map_err(|reason| rg_core::error::invalid_request(format!("job '{job_name}' {reason}")))
        .and_then(|combinations| matrix_variants(&job_name, &job_config, Some(combinations)))
    {
        Ok(variants) => variants,
        Err(error) => return fail_deferred_matrix(db, placeholder, error).await,
    };
    let (head_ref, base_ref) = rg_core::ci::pull_request_refs(
        &pipeline.trigger_type,
        pipeline.head_branch.as_deref(),
        pipeline.base_branch.as_deref(),
    );
    let rows = JobRowContext {
        repo_id: pipeline.repo_id,
        repository: RepositoryName {
            owner: &owner,
            name: &name,
        },
        commit_sha: &pipeline.commit_sha,
        ref_name: &pipeline.ref_name,
        trigger_type: &pipeline.trigger_type,
        schedule_cron: pipeline.schedule_cron.as_deref(),
        head_ref,
        base_ref,
    };
    let expanded = rg_db::ops::pipeline_matrix_ops::expand_deferred_job(db, placeholder, |tx| {
        let (rows, job_name, job_config, variants) = (&rows, &job_name, &job_config, &variants);
        async move {
            let written =
                create_variant_jobs(&tx, rows, stage.id, job_name, job_config, variants.clone())
                    .await;
            (tx, written)
        }
    })
    .await;
    match expanded {
        Ok(_) => Ok(None),
        Err(error) => fail_deferred_matrix(db, placeholder, error).await,
    }
}

/// Fail the placeholder when `reason` is the workflow's — an
/// [`rg_core::error::InvalidRequest`] — and hand any other error back: a
/// database that did not answer is no verdict on the matrix.
async fn fail_deferred_matrix(
    db: &sea_orm::DatabaseConnection,
    placeholder: &rg_db::entities::pipeline_job::Model,
    reason: anyhow::Error,
) -> Result<Option<rg_db::entities::pipeline::Model>> {
    if reason
        .downcast_ref::<rg_core::error::InvalidRequest>()
        .is_none()
    {
        return Err(reason);
    }
    let log = format!("The matrix could not be expanded: {reason:#}\n");
    rg_db::ops::pipeline_matrix_ops::fail_deferred_job(db, placeholder, &log).await
}

/// The `when_condition` a job's row is written with: its own `when:`, or —
/// for a job whose `if:` may be true after a job it needs failed —
/// [`rg_db::ops::pipeline_ops::RUNS_AFTER_FAILURE_WHEN`], which keeps it from
/// being skipped with the rest of the stages after a failed one. Only a job
/// that lists `needs:` may name a status function other than `success()`, so
/// only one that is held gets it.
fn job_when(job_config: &config::JobConfig) -> Option<&str> {
    if job_config
        .condition
        .as_deref()
        .is_some_and(condition::runs_after_failure)
    {
        Some(rg_db::ops::pipeline_ops::RUNS_AFTER_FAILURE_WHEN)
    } else {
        job_config.when.as_deref()
    }
}

/// The `needs:` of a job of steps: alias in its expressions → job name in the
/// pipeline. Empty for a native job and for one that needs nothing.
fn job_needs(job_config: &config::JobConfig) -> std::collections::BTreeMap<String, String> {
    job_config
        .job_context
        .as_ref()
        .map(|context| context.needs.clone())
        .unwrap_or_default()
}

/// What decides whether a job that lists `needs:` runs, once they have
/// finished: written on its row with the graph, read by
/// [`decide_deferred_condition`].
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HeldJob {
    /// The job's `if:` when it reads what a needed job left; `None` when it
    /// has none, or one already answered when the graph was written.
    condition: Option<String>,
    needs: std::collections::BTreeMap<String, String>,
    /// What `condition` reads besides `needs.*` — `github.*`, `env.*`,
    /// `matrix.*` — as they were for this job when the graph was written.
    context: std::collections::BTreeMap<String, String>,
    /// The environment the job names, attached only once it may run.
    environment: Option<String>,
}

/// The first job of `needs` that was skipped, by the alias the job reads it
/// under. A job whose needed job was skipped is skipped too, as on GitHub,
/// unless its condition names a status function (`always()`), which decides
/// for itself.
fn skipped_need(
    needs: &std::collections::BTreeMap<String, String>,
    needed: &std::collections::BTreeMap<String, rg_db::ops::pipeline_step_ops::NeededJob>,
) -> Option<String> {
    needs
        .keys()
        .find(|alias| {
            needed
                .get(*alias)
                .is_some_and(|job| job.result == "skipped")
        })
        .cloned()
}

/// Decide a job held until the jobs it needs have finished, now that its stage
/// is reached: skip it when one of them was skipped or its `if:` is false, and
/// otherwise release it to run, attaching its environment.
///
/// A condition with no answer, or an environment that is gone, is the
/// workflow's to fix: the job fails with the reason in its log, rather than the
/// error surfacing as this server's and the job staying held for good. A job
/// another caller already decided is left alone. `Some(pipeline)` when the job's
/// verdict finished the pipeline, for the caller's completion hooks.
pub async fn decide_deferred_condition(
    db: &sea_orm::DatabaseConnection,
    job: &rg_db::entities::pipeline_job::Model,
) -> Result<Option<rg_db::entities::pipeline::Model>> {
    use rg_db::ops::pipeline_condition_ops;
    let Some(stored) = job.deferred_condition.as_deref() else {
        return Ok(None);
    };
    let held: HeldJob =
        serde_json::from_str(stored).context("a held job's stored form is unreadable")?;
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(db, job.stage_id)
        .await?
        .with_context(|| format!("stage {} of a held job is gone", job.stage_id))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(db, stage.pipeline_id)
        .await?
        .with_context(|| format!("pipeline {} of a held job is gone", stage.pipeline_id))?;
    let needed = rg_db::ops::pipeline_step_ops::needs_context(db, pipeline.id, &held.needs).await?;
    // A condition that names a status function decides for itself how the
    // jobs it needs ended — `failure()` runs after one failed, `always()`
    // after one was skipped — as on GitHub. Any other is `success() && …`.
    let status = match held.condition.as_deref() {
        Some(condition) if !condition::status_functions(condition).is_empty() => {
            let ancestors =
                rg_db::ops::pipeline_step_ops::ancestor_results(db, pipeline.id, &held.needs)
                    .await?;
            Some(condition::JobStatus {
                any_failed: ancestors.values().any(|result| result == "failure"),
                all_succeeded: ancestors.values().all(|result| result == "success"),
            })
        }
        _ => None,
    };
    if status.is_none() {
        if let Some(alias) = skipped_need(&held.needs, &needed) {
            let log = format!("Skipped: `{alias}`, a job this one needs, was skipped.\n");
            return pipeline_condition_ops::settle_held_job(db, job, "skipped", &log).await;
        }
    }
    if let Some(condition) = held.condition.as_deref() {
        let mut context = held
            .context
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        for (alias, needed_job) in &needed {
            context.insert(format!("needs.{alias}.result"), needed_job.result.clone());
            for (name, value) in &needed_job.outputs {
                context.insert(format!("needs.{alias}.outputs.{name}"), value.clone());
            }
        }
        let decided = match status {
            Some(status) => condition::evaluate_job_condition(condition, &context, status),
            None => condition::evaluate_condition(condition, &context),
        };
        match decided {
            Ok(true) => {}
            Ok(false) => {
                let log = format!("Skipped: `if: {condition}` is false.\n");
                return pipeline_condition_ops::settle_held_job(db, job, "skipped", &log).await;
            }
            Err(error) => {
                let log = format!("`if: {condition}` could not be evaluated: {error:#}\n");
                return pipeline_condition_ops::settle_held_job(db, job, "failed", &log).await;
            }
        }
    }
    let environment = held.environment.as_deref();
    let released = pipeline_condition_ops::release_deferred_job(db, job, |tx| async move {
        let attached = async {
            if let Some(name) = environment {
                let found = resolve_job_environment(&tx, pipeline.repo_id, &job.name, name).await?;
                rg_db::ops::ci_environment_ops::attach_job(&tx, job.id, Some(&found), name).await?;
            }
            Ok(())
        }
        .await;
        (tx, attached)
    })
    .await;
    match released {
        Ok(_) => Ok(None),
        Err(error)
            if error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some() =>
        {
            let log = format!("The job could not be started: {error:#}\n");
            pipeline_condition_ops::settle_held_job(db, job, "failed", &log).await
        }
        Err(error) => Err(error),
    }
}

/// Resolve the environment a job names, refusing a name this repository has no
/// row for.
///
/// The gate lives on that row: `attach_job` puts the job in
/// `waiting_approval` when the environment is marked protected, and does
/// nothing at all when the lookup came back empty. "Nothing at all" was the
/// wrong answer to a name that resolves to no environment — the job kept the
/// name as plain text, `environment_id` stayed NULL, and it went to a runner
/// like any other. So `environment: producton` did not fail the deploy its
/// author had gated behind a protected `production`; it *released* it, with no
/// signal anywhere: environments are never auto-created, so the typo does not
/// show up in the repository's environment list either (card_adb623830190).
///
/// The name is the author's to fix, so it goes back to them beside the list
/// they can compare it against — the same answer `validate_execution_semantics`
/// gives a job that names an undeclared `stage:`.
async fn resolve_job_environment(
    tx: &sea_orm::DatabaseTransaction,
    repo_id: i64,
    job_name: &str,
    environment_name: &str,
) -> Result<rg_db::entities::ci_environment::Model> {
    if let Some(environment) =
        rg_db::ops::ci_environment_ops::find_by_name(tx, repo_id, environment_name).await?
    {
        return Ok(environment);
    }
    let names = rg_db::ops::ci_environment_ops::list_names(tx, repo_id).await?;
    let declared = if names.is_empty() {
        "this repository has no environments at all".to_string()
    } else {
        format!("the environments it has are {}", names.join(", "))
    };
    Err(rg_core::error::invalid_request(format!(
        "job '{job_name}' names environment '{environment_name}', which this repository has no \
         environment for — {declared}"
    )))
}

#[allow(clippy::too_many_arguments)]
async fn evaluate_initial_success(
    db: &sea_orm::DatabaseConnection,
    repo_root: &std::path::Path,
    repo_id: i64,
    commit_sha: &str,
    docker_enabled: bool,
    external_runners: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    encryption_key: Option<&str>,
    external_url: Option<&str>,
    engine: &CiEngine,
) {
    post_push_context(
        repo_root,
        docker_enabled,
        external_runners,
        allow_host_runner,
        jwt_secret,
        encryption_key,
        external_url,
        engine,
    )
    .evaluate_merges_and_spawn_hooks(db, repo_id, commit_sha, None)
    .await;
}

/// This crate's post-push wiring, for the merges a finished pipeline unblocks.
///
/// A pipeline that goes green is the trigger of the most common auto-merge there
/// is, and the merge commit it puts on the base branch owes the same automation
/// a push does — a pipeline of its own, the `push` webhook, the watch fan-out.
/// Until card_73a1ec5b32f3 both CI-completion paths here ran the merge and threw
/// the ref move away, so that commit was seen by nothing.
///
/// Two of the hooks' inputs have no home in this crate — the WebSocket hub is an
/// `rg-http` type and the SMTP configuration is the process's — so until
/// card_85b8d59246b5 this context was assembled with both of them `None` and the
/// merge CI made was silent where the same merge over REST was loud: no
/// real-time `push` / `ci_triggered` event, no "pipeline triggered" email.
/// They now arrive through the process's [`CiEngine`], wired once at startup.
/// The nested engine (the one that triggers the merge commit's own pipeline,
/// which can cascade into another merge) clones that whole runtime bundle so
/// neither notifications nor the instance job timeout reset one hop down.
#[allow(clippy::too_many_arguments)]
fn post_push_context(
    repo_root: &std::path::Path,
    docker_enabled: bool,
    external_runners: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    encryption_key: Option<&str>,
    external_url: Option<&str>,
    engine: &CiEngine,
) -> rg_core::push_hooks::PostPushContext {
    rg_core::push_hooks::PostPushContext {
        repo_root: repo_root.to_path_buf(),
        docker_enabled,
        external_runners,
        allow_host_runner,
        jwt_secret: jwt_secret.map(str::to_string),
        encryption_key: encryption_key.map(str::to_string),
        smtp_config: engine.notifications.smtp_config.clone(),
        ci_engine: nested_engine(engine),
        external_url: external_url.map(str::to_string),
        notifier: engine.notifications.notifier.clone(),
        delivery_tracker: rg_core::task_tracker::delivery_tracker().clone(),
        write_pool: engine.write_pool.clone(),
        storage_limits: engine.storage_limits,
    }
}

/// The engine the hooks spawned here trigger through.
///
/// The merge commit those hooks produce gets a pipeline of its own, and that
/// pipeline can unblock the next merge — so the engine one hop down must carry
/// the same wiring, or the effects fade out on the second merge instead of at
/// the process boundary.
fn nested_engine(engine: &CiEngine) -> std::sync::Arc<CiEngine> {
    std::sync::Arc::new(engine.clone())
}

#[allow(clippy::too_many_arguments)]
fn spawn_internal_runner(
    db: &sea_orm::DatabaseConnection,
    repo_root: &std::path::Path,
    repo_path: &std::path::Path,
    repo_id: i64,
    pipeline_id: i64,
    docker_enabled: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    encryption_key: Option<&str>,
    external_url: Option<&str>,
    engine: &CiEngine,
) {
    let db_clone = db.clone();
    let engine = engine.clone();
    let repo_root_owned = repo_root.to_path_buf();
    let repo_path_owned = repo_path.to_path_buf();
    let jwt_secret_owned = jwt_secret.map(str::to_string);
    let encryption_key_owned = encryption_key.map(str::to_string);
    let oidc_token_url =
        external_url.map(|url| format!("{}/api/v1/ci/oidc/token", url.trim_end_matches('/')));
    // Through the tracker, not `tokio::spawn`. A bare spawn is owned by nobody:
    // on `SIGTERM` the pipeline was severed wherever it stood, and because the
    // runner also had no signal to observe, the `pipeline_jobs` row it was
    // holding stayed `running` until the stuck-job sweep reclaimed it ten
    // minutes later — on every planned restart, in the default configuration
    // (an instance with no external runner has no other executor). The runner
    // now observes the signal and hands its job back; this tracker is what
    // keeps the runtime alive long enough for that write to land
    // (card_34368880dc20).
    //
    // Claimed before the spawn, so a watchdog tick that lands between the two
    // cannot read this pipeline as unowned and start a second runner beside
    // this one. The lease travels into the task and is released when the task
    // ends — including when it ends by panic, which is exactly the death the
    // mid-life recovery exists to notice (card_111ac7923d7c).
    let lease = rg_core::ci::embedded_runners::claim_embedded_runner(pipeline_id);
    rg_core::task_tracker::ci_tracker().spawn(rg_core::ci::embedded_runners::holding(
        lease,
        async move {
            let runner = build_internal_runner(
                db_clone,
                &repo_root_owned,
                &repo_path_owned,
                repo_id,
                pipeline_id,
                docker_enabled,
                allow_host_runner,
                jwt_secret_owned,
                encryption_key_owned,
                oidc_token_url,
                &engine,
            );
            if let Err(error) = runner.run().await {
                tracing::error!(pipeline_id, error = %format!("{error:#}"), "pipeline runner error");
            }
        },
    ));
}

#[allow(clippy::too_many_arguments)]
fn build_internal_runner(
    db: sea_orm::DatabaseConnection,
    repo_root: &std::path::Path,
    repo_path: &std::path::Path,
    repo_id: i64,
    pipeline_id: i64,
    docker_enabled: bool,
    allow_host_runner: bool,
    jwt_secret: Option<String>,
    encryption_key: Option<String>,
    oidc_token_url: Option<String>,
    engine: &CiEngine,
) -> PipelineRunner {
    let mut runner = if docker_enabled {
        PipelineRunner::new_with_job_timeout(
            db,
            repo_root,
            repo_path,
            pipeline_id,
            engine.job_timeout_secs,
        )
    } else {
        PipelineRunner::new_local_only_with_job_timeout(
            db,
            repo_root,
            repo_path,
            pipeline_id,
            engine.job_timeout_secs,
        )
    };
    runner.set_repo_id(repo_id);
    runner.set_allow_host_runner(allow_host_runner);
    runner.set_runner_labels(engine.runner_labels().to_vec());
    runner.set_notifications(engine.notifications.clone());
    if let Some(secret) = jwt_secret {
        runner.set_jwt_secret(secret);
    }
    if let Some(secret) = encryption_key {
        runner.set_encryption_key(secret);
    }
    if let Some(url) = oidc_token_url {
        runner.set_oidc_token_url(url);
    }
    runner.set_shutdown(engine.shutdown.clone());
    runner.set_storage_limits(engine.storage_limits);
    runner.set_job_limits(engine.job_limits.clone());
    if let Some(write_pool) = &engine.write_pool {
        runner.set_write_pool(write_pool.clone());
    }
    runner
}

/// The stage a job runs in when it names none.
///
/// The name is the one the model's own documentation gives it: "Jobs not listed
/// in `stages` will be placed in a \"default\" stage" ([`config::CiConfig`]).
const DEFAULT_STAGE: &str = "default";

/// Whether a job that says nothing about `allow_failure:` may fail the pipeline.
///
/// The safe reading of silence: a job the author never marked as tolerable is
/// one whose failure fails the run.
const DEFAULT_ALLOW_FAILURE: bool = false;

/// The ordered stage list this config actually runs, `default` included.
///
/// `stages:` is optional, so a `.plombir-git-ci.yml` that declares only jobs is a
/// valid file — and every one of its jobs resolves to [`DEFAULT_STAGE`]. Reading
/// the stage set off `stages:` alone therefore built an empty set, dropped every
/// job for want of a stage, and published a pipeline of zero jobs that the
/// runner walked in no iterations and settled `success`: a green pipeline that
/// ran no command, feeding branch protection (card_d92cd3260864).
///
/// The synthesized stage goes last. A file that lists stages *and* leaves a job
/// without one has not said where that job belongs, and running it after
/// everything it might have depended on is the reading that cannot invent an
/// ordering constraint the author never wrote. A file that spells `default`
/// among its `stages:` keeps the position it chose.
fn resolved_stage_order(config: &CiConfig) -> Vec<String> {
    let mut stages = config.stages.clone().unwrap_or_default();
    let default_is_used = config
        .jobs
        .values()
        .any(|job| job.stage.as_deref().unwrap_or(DEFAULT_STAGE) == DEFAULT_STAGE);
    if default_is_used && !stages.iter().any(|stage| stage == DEFAULT_STAGE) {
        stages.push(DEFAULT_STAGE.to_string());
    }
    stages
}

/// Reject a hand-written `tags:` this instance has no runner to honour.
///
/// `tags:` names the labels a runner must carry. Matching them against a
/// registered runner is `pipeline_ops::find_pending_job_matching_labels`, and
/// the only caller of that is the poll route external runners use. With
/// `ci.external_runners` off there is no poll: `trigger_pipeline` spawns the
/// in-process runner, which is not registered, carries no labels, and does not
/// read the column at all — so `tags: [gpu]` ran on the server, which is the one
/// outcome the declaration exists to prevent (card_97271ae84c0c).
///
/// The refusal is at trigger time and names the key, like the neighbouring
/// `when:` and `timeout_seconds` rules, rather than a silent skip at execution:
/// on an instance with no external runners there is no second runner to fall
/// back to, so "leave it queued" would be a pipeline that hangs for ever with
/// nothing saying why.
///
/// **Only for a hand-written file.** An Actions workflow's `runs-on:` lands in
/// the very same `tags` column ([`GiteaRunsOn::tags`]), but it is a mandatory
/// GitHub field that nearly every workflow fills with boilerplate — refusing
/// the whole trigger over one job's `runs-on:` would take the workflow's other
/// jobs down with it, for a key its author rarely chose. That half is refused
/// per job by the runner that cannot honour it
/// ([`crate::runner::PipelineRunner`]), so the pipeline still exists and the
/// job that asked to be elsewhere fails saying so (card_4f7703a8b575). Both
/// halves route by the one rule, `pipeline_ops::uncovered_job_tags`.
fn validate_runner_routing(
    config: &CiConfig,
    external_runners: bool,
    runner_labels: &[String],
) -> Result<()> {
    if external_runners || config.actions_workflow {
        return Ok(());
    }
    // Sorted, because `jobs` is a `HashMap`: a file with two tagged jobs would
    // otherwise name a different one on each run, and a refusal the author
    // cannot reproduce is one they cannot act on.
    let mut named: Vec<_> = config.jobs.iter().collect();
    named.sort_by_key(|(name, _)| name.as_str());
    for (name, job) in named {
        let Some(tags) = job.tags.as_deref().filter(|tags| !tags.is_empty()) else {
            continue;
        };
        // A label this instance declares it carries is honoured by running the
        // job here — that is what declaring it means. Only the ones it does not
        // carry are the routing instruction it cannot follow.
        let uncovered = rg_db::ops::pipeline_ops::uncovered_job_tags(tags, runner_labels);
        if uncovered.is_empty() {
            continue;
        }
        return Err(rg_core::error::invalid_request(format!(
            "job '{name}' declares tags: [{}], which asks for a runner carrying those labels, but \
             this instance runs CI in-process on a runner labelled [{}] and cannot honour [{}]. \
             Add the label to ci.runner_labels if this server really is that machine, turn on \
             ci.external_runners and register a runner carrying it, or drop tags: from the job",
            tags.join(", "),
            runner_labels.join(", "),
            uncovered.join(", "),
        )));
    }
    Ok(())
}

/// Refuse a name from the workflow file that its column cannot hold.
///
/// Job and stage names and the concurrency group are kept in `VARCHAR(255)`.
/// PostgreSQL and SQLite stored any length, but on MySQL a longer one failed
/// the insert, so the author of a long `concurrency.group` or matrix label got
/// a server failure instead of the key to shorten (card_51b860efc19c). The
/// ceiling is checked on every backend, so a file means the same thing on all
/// three. `what` names the key; the value is quoted only by its start, since
/// it is the part that is too long.
fn check_stored_name(what: &str, name: &str) -> Result<()> {
    let max = rg_core::text_limits::MAX_LINE_CHARS;
    let count = name.chars().count();
    if count <= max {
        return Ok(());
    }
    let start: String = name.chars().take(40).collect();
    Err(rg_core::error::invalid_request(format!(
        "{what} '{start}…' is {count} characters long; it must be at most {max} characters"
    )))
}

/// Reject a CI config whose jobs declare something this engine cannot run.
///
/// Every rejection here is a rule the *user's own file* broke, so each one
/// carries [`rg_core::error::InvalidRequest`]: as a bare `anyhow` they reached
/// `AppError::from` with nothing to classify by and came out a `500` whose body
/// the H-05 sanitizer replaced with "Internal server error" — so someone who
/// typed `when: allways` was told the server had crashed and never learned
/// which job, which field, or which value was wrong. The messages name the job
/// and the rule and nothing else (no path, no errno), which is what lets them
/// reach the client verbatim.
fn validate_execution_semantics(config: &CiConfig) -> Result<()> {
    // A file with no job at all is the other spelling of the empty pipeline:
    // stages get their rows, the runner finds nothing to run in them, and the
    // commit is reported `success` on the strength of no command having failed.
    // The Actions path already refuses to build a pipeline out of no jobs
    // (`try_read_gitea_workflows` falls through to `NoneTriggered`); the native
    // path said nothing.
    if config.jobs.is_empty() {
        return Err(rg_core::error::invalid_request(
            "this CI config declares no jobs; a pipeline that runs nothing cannot report success",
        ));
    }
    // Two stages of one name are one stage: the id map keeps the last row
    // written, so the earlier one can never receive a job and stands in the
    // pipeline permanently empty. The author cannot see which of the two their
    // jobs landed in, so the duplicate is refused instead of resolved.
    let mut seen = std::collections::BTreeSet::new();
    for stage in config.stages.iter().flatten() {
        if !seen.insert(stage.as_str()) {
            return Err(rg_core::error::invalid_request(format!(
                "stages: lists '{stage}' twice; each stage name must appear once"
            )));
        }
    }

    let runnable_stages = resolved_stage_order(config);
    for stage in &runnable_stages {
        check_stored_name("the stage name", stage)?;
    }
    for (name, job) in &config.jobs {
        check_stored_name("the job name", name)?;
        // The engine places a job by its stage name, and a name it cannot place
        // used to cost the job its run — `warn!` server-side, nothing anywhere
        // the author looks. `stage: biuld` is the author's typo to fix, so it
        // is returned to them with the list they can compare it against.
        let stage = job.stage.as_deref().unwrap_or(DEFAULT_STAGE);
        if !runnable_stages.iter().any(|runnable| runnable == stage) {
            let declared = if runnable_stages.is_empty() {
                "this file declares no stages at all".to_string()
            } else {
                format!("the stages it declares are {}", runnable_stages.join(", "))
            };
            return Err(rg_core::error::invalid_request(format!(
                "job '{name}' names stage '{stage}', which this file never lists under stages: — \
                 {declared}"
            )));
        }
        if let Some(when) = job.when.as_deref() {
            // The accepted spelling of the default is the constant the row is
            // actually written with, not a second copy of the same word: a
            // `when:` the author omits and a `when:` they type by hand have to
            // mean the same thing.
            let default_when = rg_db::ops::pipeline_ops::DEFAULT_JOB_WHEN;
            if when != default_when && when != "manual" {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{name}' uses unsupported when: '{when}'; supported values are \
                     '{default_when}' and 'manual'"
                )));
            }
        }
        if let Some(timeout) = job.timeout_seconds {
            // The whole range, not just its upper end and zero: a negative
            // value is as unrunnable as `0`, and letting it through meant the
            // job was created with a timeout no runner could honour — each one
            // silently substituted a different number of its own.
            if !rg_core::ci::job_timeout_in_range(timeout) {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{name}' timeout_seconds must be between {} and {} (got {timeout})",
                    rg_core::ci::JOB_TIMEOUT_MIN_SECS,
                    rg_core::ci::JOB_TIMEOUT_MAX_SECS
                )));
            }
        }
        if let Some(environment) = job.environment.as_deref() {
            if environment.is_empty()
                || environment.len() > 255
                || environment.chars().any(char::is_control)
            {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{name}' has an invalid environment name"
                )));
            }
        }
        if let Some(condition) = job.condition.as_deref() {
            // The parser's complaint ("unsupported condition function 'foo'")
            // is the half that says what to fix, so it is folded into the
            // message rather than left as a `.context(...)` source the
            // client-facing render would drop.
            // A job that needs others is decided once they have finished,
            // and may read what they left (see `hold_job`).
            let validated = if job_needs(job).is_empty() {
                crate::condition::validate_condition(condition)
            } else {
                crate::condition::validate_job_condition(condition)
            };
            if let Err(error) = validated {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{name}' has an unsupported if condition: {error:#}"
                )));
            }
            // A matrix read from a needed job's outputs is expanded only once
            // that job succeeded; after a failure it has no variants to run.
            if condition::runs_after_failure(condition)
                && job
                    .actions_matrix
                    .as_ref()
                    .is_some_and(|spec| !spec.needs_outputs().is_empty())
            {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{name}' has an unsupported if condition: {condition} asks it to run \
                     after a job it needs failed, and its matrix is read from the outputs of a \
                     job it needs, which a failed job does not leave"
                )));
            }
        }
        if let Some(artifacts) = job.artifacts.as_ref() {
            // Refused here rather than at upload time. The artifact is
            // published *after* the script has already run: a rule broken in
            // the file would otherwise be discovered by a green job whose
            // artifact silently never appeared, which is the one failure a
            // pipeline cannot report.
            // A name or a path that is a template is checked once it is
            // rendered for each variant, in `create_variant_jobs`. A path's
            // own text is judged here either way: `/${{ matrix.os }}` is
            // absolute whatever the variant.
            let templates = job.action_templates.as_ref();
            let name_template = templates.and_then(|templates| templates.artifact_name.as_ref());
            let path_templates = templates.and_then(|templates| templates.artifact_paths.as_ref());
            let shaped;
            let artifacts = if name_template.is_some() || path_templates.is_some() {
                shaped = config::ArtifactsConfig {
                    name: match name_template {
                        Some(_) => Some("artifact".to_owned()),
                        None => artifacts.name.clone(),
                    },
                    paths: match path_templates {
                        Some(paths) => paths.iter().map(config::ActionTemplate::shape).collect(),
                        None => artifacts.paths.clone(),
                    },
                };
                &shaped
            } else {
                artifacts
            };
            if let Err(error) = artifact_spec(name, artifacts) {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{name}' has an unusable artifacts block: {error}"
                )));
            }
        }
    }
    Ok(())
}

/// Largest number of paths one artifact may pack, matching the cache's own cap.
const ARTIFACT_PATHS_MAX: usize = 64;

/// Longest artifact name the storage layer keeps intact.
const ARTIFACT_NAME_MAX: usize = 100;

/// The artifact a job publishes, resolved from its declaration and checked
/// against what the two runners and the storage layer can actually honour.
///
/// Returns the name the archive is stored under and the workspace-relative
/// paths packed into it. `name` defaults to the job's own name — an artifact
/// belongs to a job, and a file that names its job is the one an author can
/// find again without opening the pipeline.
pub(crate) fn artifact_spec(
    job_name: &str,
    artifacts: &config::ArtifactsConfig,
) -> std::result::Result<(String, Vec<String>), String> {
    let name = artifacts
        .name
        .clone()
        .unwrap_or_else(|| job_name.to_owned());
    // The store strips every other character out of the name instead of
    // refusing it, so a name written with a space or a slash would be published
    // under a name the author never wrote and cannot search for. Refusing is
    // the only answer that keeps the declaration and the artifact the same
    // thing.
    if name.is_empty() || name.len() > ARTIFACT_NAME_MAX {
        return Err(format!(
            "artifact name must contain 1-{ARTIFACT_NAME_MAX} characters"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        || name.chars().all(|c| c == '.')
    {
        return Err(format!(
            "artifact name '{name}' must use only ASCII letters, digits, '.', '-' and '_'"
        ));
    }
    if artifacts.paths.is_empty() || artifacts.paths.len() > ARTIFACT_PATHS_MAX {
        return Err(format!("artifacts requires 1-{ARTIFACT_PATHS_MAX} paths"));
    }
    for path in &artifacts.paths {
        if path.is_empty() {
            return Err("artifact path must not be empty".to_string());
        }
        let candidate = std::path::Path::new(path);
        if candidate.is_absolute()
            || candidate.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(format!(
                "artifact path must stay within the workspace: {path}"
            ));
        }
    }
    Ok((name, artifacts.paths.clone()))
}

fn job_condition_context(
    rows: &JobRowContext<'_>,
    variables: &std::collections::BTreeMap<String, String>,
    coordinates: &std::collections::BTreeMap<String, String>,
) -> std::collections::HashMap<String, String> {
    let &JobRowContext {
        repository,
        commit_sha: sha,
        ref_name,
        trigger_type: event,
        schedule_cron: event_schedule,
        head_ref,
        base_ref,
        ..
    } = rows;
    let mut context = std::collections::HashMap::from([
        ("github.ref".into(), ref_name.to_string()),
        (
            "github.ref_name".into(),
            condition::ref_name_short(ref_name).to_string(),
        ),
        ("github.event_name".into(), event.to_string()),
        ("github.sha".into(), sha.to_string()),
        (
            "github.repository".into(),
            format!("{}/{}", repository.owner, repository.name),
        ),
        (
            "github.repository_owner".into(),
            repository.owner.to_string(),
        ),
        (
            "github.event.schedule".into(),
            event_schedule.unwrap_or_default().to_string(),
        ),
        ("github.head_ref".into(), head_ref.to_string()),
        ("github.base_ref".into(), base_ref.to_string()),
    ]);
    for (name, value) in variables {
        context.insert(format!("env.{name}"), value.clone());
    }
    for (name, value) in coordinates {
        context.insert(format!("matrix.{name}"), value.clone());
    }
    context
}

#[derive(Debug, Clone)]
struct MatrixVariant {
    name: String,
    variables: std::collections::BTreeMap<String, String>,
    /// `matrix.<key>` → value: what the variant's expressions read. Not
    /// `variables`, which holds a key only when it is a valid variable name —
    /// `matrix.node-version` has no variable of its own.
    coordinates: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ResolvedActionJobFields {
    image: Option<String>,
    tags: Option<Vec<String>>,
    environment: Option<String>,
    /// The rendered `actions/upload-artifact` name, when it was a template.
    artifact_name: Option<String>,
    /// The rendered `actions/upload-artifact` / `actions/cache` paths, when
    /// one of them was a template.
    artifact_paths: Option<Vec<String>>,
    cache_paths: Option<Vec<String>>,
    /// `jobs.<id>.name` as this variant shows it; `None` without one, or when
    /// it rendered empty — the job is then shown by its id, as on GitHub.
    display_name: Option<String>,
}

struct ActionTemplateContext<'value, 'repo> {
    variant: &'value MatrixVariant,
    ref_name: &'value str,
    event: &'value str,
    sha: &'value str,
    repository: RepositoryName<'repo>,
}

fn action_expression_value(
    expression: &config::ActionExpression,
    context: &ActionTemplateContext<'_, '_>,
) -> Option<String> {
    match expression {
        config::ActionExpression::GithubRef => Some(context.ref_name.to_owned()),
        config::ActionExpression::GithubRefName => {
            Some(condition::ref_name_short(context.ref_name).to_owned())
        }
        config::ActionExpression::GithubSha => Some(context.sha.to_owned()),
        config::ActionExpression::GithubEventName => Some(context.event.to_owned()),
        config::ActionExpression::GithubRepository => Some(format!(
            "{}/{}",
            context.repository.owner, context.repository.name
        )),
        config::ActionExpression::GithubRepositoryOwner => {
            Some(context.repository.owner.to_owned())
        }
        config::ActionExpression::Matrix(name) => context.variant.coordinates.get(name).cloned(),
        config::ActionExpression::Input(name) => context
            .variant
            .variables
            .get(&format!(
                "INPUT_{}",
                name.to_ascii_uppercase().replace('-', "_")
            ))
            .cloned(),
        config::ActionExpression::EventInput(name) => context
            .variant
            .variables
            .get(&gitea_actions::event_input_environment_name(name))
            .cloned(),
        config::ActionExpression::Unsupported(_) => None,
    }
}

fn render_action_template(
    job_name: &str,
    field: &str,
    template: &config::ActionTemplate,
    context: &ActionTemplateContext<'_, '_>,
) -> Result<String> {
    template
        .render(|expression| action_expression_value(expression, context))
        .map_err(|expression| {
            rg_core::error::invalid_request(format!(
                "job '{job_name}' cannot resolve {field} expression `{expression}`"
            ))
        })
}

/// A step's `name:` as one variant shows it. A label is never executed, so an
/// expression with no value before a runner starts — `runner.os`,
/// `steps.<id>.outputs.*` — stays as written rather than refusing a workflow
/// over its label.
fn step_label(name: &str, context: &ActionTemplateContext<'_, '_>) -> String {
    match gitea_actions::compile_action_template(name) {
        Some(template) => {
            template.render_or_spell(|expression| action_expression_value(expression, context))
        }
        None => name.to_owned(),
    }
}

fn resolve_action_job_fields(
    job_name: &str,
    config: &config::JobConfig,
    variant: &MatrixVariant,
    ref_name: &str,
    event: &str,
    sha: &str,
    repository: RepositoryName<'_>,
) -> Result<ResolvedActionJobFields> {
    let Some(templates) = &config.action_templates else {
        return Ok(ResolvedActionJobFields {
            image: config.image.clone(),
            tags: config.tags.clone(),
            environment: config.environment.clone(),
            artifact_name: None,
            artifact_paths: None,
            cache_paths: None,
            display_name: None,
        });
    };
    let context = ActionTemplateContext {
        variant,
        ref_name,
        event,
        sha,
        repository,
    };

    let image = templates
        .image
        .as_ref()
        .map(|template| render_action_template(job_name, "container.image", template, &context))
        .transpose()?
        .or_else(|| config.image.clone());
    let tags = templates
        .tags
        .as_ref()
        .map(|templates| {
            templates
                .iter()
                .map(|template| render_action_template(job_name, "runs-on", template, &context))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .or_else(|| config.tags.clone());
    let environment = templates
        .environment
        .as_ref()
        .map(|template| render_action_template(job_name, "environment.name", template, &context))
        .transpose()?
        .or_else(|| config.environment.clone());

    if environment.as_ref().is_some_and(|name| {
        name.is_empty() || name.len() > 255 || name.chars().any(char::is_control)
    }) {
        return Err(rg_core::error::invalid_request(format!(
            "job '{job_name}' has an invalid environment name"
        )));
    }

    let artifact_name = templates
        .artifact_name
        .as_ref()
        .map(|template| render_action_template(job_name, "artifact name", template, &context))
        .transpose()?;
    // A path line that renders empty — an input left blank — is no path, as
    // a blank line of `path:` is not.
    let render_paths = |field: &str, templates: &Option<Vec<config::ActionTemplate>>| {
        templates
            .as_ref()
            .map(|templates| {
                templates
                    .iter()
                    .map(|template| render_action_template(job_name, field, template, &context))
                    .filter(|path| !path.as_ref().is_ok_and(String::is_empty))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()
    };
    let artifact_paths = render_paths("artifact path", &templates.artifact_paths)?;
    let cache_paths = render_paths("cache path", &templates.cache_paths)?;

    let display_name = templates
        .display_name
        .as_ref()
        .map(|template| render_action_template(job_name, "name", template, &context))
        .transpose()?
        .filter(|name| !name.is_empty());
    if let Some(name) = &display_name {
        if name.chars().count() > JOB_DISPLAY_NAME_MAX || name.chars().any(char::is_control) {
            return Err(rg_core::error::invalid_request(format!(
                "job '{job_name}' has an unusable name: it must be at most \
                 {JOB_DISPLAY_NAME_MAX} characters, without control characters"
            )));
        }
    }

    Ok(ResolvedActionJobFields {
        image,
        tags,
        environment,
        artifact_name,
        artifact_paths,
        cache_paths,
        display_name,
    })
}

/// Longest `jobs.<id>.name` a job is shown under, after rendering.
const JOB_DISPLAY_NAME_MAX: usize = 255;

fn matrix_variable_name(key: &str) -> String {
    format!("MATRIX_{}", key.to_ascii_uppercase().replace('-', "_"))
}

fn expand_matrix(job_name: &str, config: &config::JobConfig) -> Result<Vec<MatrixVariant>> {
    // Same split as `validate_execution_semantics`: an unusable `matrix:` block
    // is the user's file being wrong, not this server failing, and it is only
    // reached from the pipeline build inside `trigger_pipeline` — where a bare
    // `anyhow` became a sanitized 500 that named neither the job nor the limit.
    let refused =
        |reason: String| rg_core::error::invalid_request(format!("job '{job_name}' {reason}"));
    let combinations = if let Some(spec) = &config.actions_matrix {
        let matrix = spec.resolve(|_| None).map_err(refused)?;
        Some(matrix::combinations(&matrix).map_err(refused)?)
    } else if let Some(matrix) = &config.matrix {
        if matrix.values().any(Vec::is_empty) {
            return Err(refused("has an empty matrix dimension".into()));
        }
        let count = matrix
            .values()
            .try_fold(1usize, |total, values| total.checked_mul(values.len()))
            .ok_or_else(|| {
                refused(format!(
                    "matrix is too large to expand; maximum is {} variants",
                    matrix::MAX_MATRIX_VARIANTS
                ))
            })?;
        if count > matrix::MAX_MATRIX_VARIANTS {
            return Err(refused(format!(
                "matrix expands to {count} variants; maximum is {}",
                matrix::MAX_MATRIX_VARIANTS
            )));
        }
        let mut product = vec![matrix::Combination::new()];
        for (key, values) in matrix {
            product = product
                .into_iter()
                .flat_map(|combination| {
                    values.iter().map(move |value| {
                        let mut combination = combination.clone();
                        combination.push((key.clone(), value.clone()));
                        combination
                    })
                })
                .collect();
        }
        Some(product)
    } else {
        None
    };
    matrix_variants(job_name, config, combinations)
}

/// One job row per combination, named and labelled by its coordinates;
/// `None` is the job without a matrix.
fn matrix_variants(
    job_name: &str,
    config: &config::JobConfig,
    combinations: Option<Vec<matrix::Combination>>,
) -> Result<Vec<MatrixVariant>> {
    let base: std::collections::BTreeMap<String, String> = config
        .variables
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let Some(combinations) = combinations else {
        return Ok(vec![MatrixVariant {
            name: job_name.to_owned(),
            variables: base,
            coordinates: Default::default(),
        }]);
    };
    let mut variants = Vec::with_capacity(combinations.len());
    for combination in combinations {
        let mut variables = base.clone();
        for (key, value) in &combination {
            // A matrix key is what the variant is labelled with, and its
            // variable is the derived `MATRIX_<KEY>`: `node-version` is a
            // common key and becomes `MATRIX_NODE_VERSION`. That derived name
            // has to be one a process can carry; the key itself only becomes a
            // variable as well when it already is.
            let derived = matrix_variable_name(key);
            if !rg_process::job_environment::valid_environment_name(&derived) {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{job_name}' matrix key '{key}' gives the variable name '{derived}', which \
                     is not a valid environment variable name (letters, digits and '_', and '-' in \
                     the key)"
                )));
            }
            if rg_process::job_environment::valid_environment_name(key) {
                variables.insert(key.clone(), value.clone());
            }
            variables.insert(derived, value.clone());
        }
        let name = format!(
            "{job_name} [{}]",
            combination
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        // The label grows with the values, so a job whose own name fits can
        // still have a variant whose name does not.
        check_stored_name("the matrix job name", &name)?;
        variants.push(MatrixVariant {
            name,
            variables,
            coordinates: combination.into_iter().collect(),
        });
    }
    Ok(variants)
}

/// The `<owner>/<name>` a pipeline is running for.
///
/// Carried down to [`gitea_actions::WorkflowContext`] so `${{ github.repository }}`
/// and `${{ github.repository_owner }}` resolve to something. It is resolved once
/// in [`trigger_pipeline_with_barrier_and_engine`] from the database rather
/// than passed in by each producer: five call sites construct
/// [`TriggerPipelineParams`], and a field
/// every one of them has to remember is a field one of them will forget — which
/// is how both halves of this identity came to be `String::new() // filled later`
/// in the first place (card_054e997a46e6).
#[derive(Debug, Clone, Copy)]
pub struct RepositoryName<'a> {
    pub owner: &'a str,
    pub name: &'a str,
}

#[derive(Clone, Copy)]
struct WorkflowInvocation<'a> {
    event: &'a str,
    base_branch: Option<&'a str>,
    /// See [`rg_core::ci::TriggerPipelineParams::head_branch`].
    head_branch: Option<&'a str>,
    previous_sha: Option<&'a str>,
    inputs: Option<&'a std::collections::HashMap<String, String>>,
    /// The cron entry a `schedule` tick fired for; see
    /// [`rg_core::ci::TriggerPipelineParams::schedule`].
    schedule: Option<rg_core::ci::ScheduledRun<'a>>,
    /// Where `uses: owner/repo@ref` and `uses: ./path` are loaded from. `None`
    /// loads nothing: every such step stays a `uses:` the workflow check
    /// refuses, which is what a reader with no trigger behind it — a test, a
    /// schema probe — gets.
    actions: Option<&'a dyn actions::ActionLoader>,
    /// What `vars.*` reads: the repository's configuration variables over its
    /// organization's. `None` leaves every `vars.*` for the workflow check to
    /// refuse, which is what a reader with no repository behind it gets.
    vars: Option<&'a std::collections::BTreeMap<String, String>>,
}

/// Read CI configuration from the repo at the given commit.
///
/// Tries formats in order:
/// 1. `.gitea/workflows/*.yml` (Gitea Actions format)
/// 2. `.plombir-git-ci.yml` (native format)
///
/// For Gitea Actions workflows, multiple files are merged into a single `CiConfig`.
/// Jobs from different workflow files are placed in separate stages.
///
/// The fallback from the first format to the second happens only when
/// `.gitea/workflows` is absent, or holds no workflow triggered by this event.
/// A workflow file that *is* there but cannot be read or parsed is an error
/// naming the file and the reason — never a silent "no CI config found".
///
/// The errors split in two, and the split is load-bearing. Anything about the
/// *content* committed to the repository — a file that is not UTF-8, YAML that
/// does not parse, no config at that commit at all — carries
/// [`rg_core::error::InvalidRequest`], so the manual-trigger and retry routes
/// answer `400` with the reason in the body. Anything about *reaching* that
/// content — the repository not opening, a commit or tree that will not resolve,
/// an object the database cannot hand over — stays a bare `anyhow` and therefore
/// a `5xx`: the caller's file is fine and retrying is the right advice. Those
/// messages also carry absolute filesystem paths, which is the other reason they
/// must never take the branch that reaches the client (H-05).
#[cfg(test)]
fn read_ci_config(
    repo_path: &std::path::Path,
    repository: RepositoryName<'_>,
    commit_sha: &str,
    ref_name: &str,
    event: &str,
    base_branch: Option<&str>,
    previous_sha: Option<&str>,
) -> Result<CiConfig> {
    read_ci_config_with_inputs(
        repo_path,
        repository,
        commit_sha,
        ref_name,
        WorkflowInvocation {
            event,
            base_branch,
            head_branch: None,
            previous_sha,
            inputs: None,
            schedule: None,
            actions: None,
            vars: None,
        },
    )
}

fn read_ci_config_with_inputs(
    repo_path: &std::path::Path,
    repository: RepositoryName<'_>,
    commit_sha: &str,
    ref_name: &str,
    invocation: WorkflowInvocation<'_>,
) -> Result<CiConfig> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let tree = tree_at_commit(&repo, commit_sha)?;

    // Try Gitea Actions format first
    let gitea = try_read_gitea_workflows(&repo, repository, commit_sha, ref_name, invocation)?;
    let workflows_untriggered = matches!(gitea, GiteaWorkflows::NoneTriggered);
    if let GiteaWorkflows::Config(config) = gitea {
        tracing::info!("Using Gitea Actions workflow from {}/", WORKFLOW_DIR);
        return Ok(config);
    }

    // A tick names one workflow file, and the native format has no `on:` at
    // all: falling through would run `.plombir-git-ci.yml` — a pipeline nobody
    // scheduled — every time a cron entry that no longer exists comes round.
    if invocation.event == rg_core::ci::SCHEDULE_EVENT {
        let named = invocation
            .schedule
            .map(|run| {
                format!(
                    "{WORKFLOW_DIR}/{} with `- cron: '{}'`",
                    run.workflow, run.cron
                )
            })
            .unwrap_or_else(|| "no workflow file at all".to_string());
        return Err(rg_core::error::invalid_request(format!(
            "this scheduled run is for {named}, which commit {commit_sha} does not declare"
        )));
    }

    // A native config has no `inputs` context. Falling through after accepting
    // values here would report a successful manual run that discarded every
    // caller-supplied value.
    if invocation.inputs.is_some_and(|inputs| !inputs.is_empty()) {
        return Err(rg_core::error::invalid_request(
            "workflow_dispatch inputs were provided, but no matching Gitea Actions workflow accepted them",
        ));
    }

    // Fall back to the native CI config. The entry is carried out of this
    // lookup rather than looked up a second time: the second call ended in
    // `.expect("the CI config was found above")`, an invariant that held only
    // because two independent operations happened to agree about a tree the
    // client committed.
    let (ci_filename, entry) = [".plombir-git-ci.yml"]
        .into_iter()
        .find_map(|name| {
            tree.lookup_entry_by_path(name)
                .with_context(|| format!("failed to look up {name} at commit {commit_sha}"))
                .transpose()
                .map(|entry| entry.map(|entry| (name, entry)))
        })
        .transpose()?
        .ok_or_else(|| {
            // Workflows that exist but sit out this event are not "no config",
            // and not a broken one either: there is simply nothing to run, the
            // way an `only:` filter excluding every job is (card_1d3af3ad4f55).
            // Automatic producers publish no pipeline for it; a manual run or a
            // retry still answers `400` with this text, which is the difference
            // between fixing an `on:` filter and hunting for a file that is
            // already there.
            if workflows_untriggered {
                anyhow::Error::new(rg_core::ci::NoMatchingCiJobs::untriggered(
                    ref_name,
                    format!(
                        "no workflow in {}/ is triggered by event {} on {}, and no native CI config (.plombir-git-ci.yml) at commit {}",
                        WORKFLOW_DIR,
                        invocation.event,
                        ref_name,
                        commit_sha
                    ),
                ))
            } else {
                rg_core::error::invalid_request(format!(
                    "no CI config found (.gitea/workflows/*.yml or .plombir-git-ci.yml) at commit {}",
                    commit_sha
                ))
            }
        })?;

    // What the entry IS is written in its committed mode; an object header only
    // describes what its oid happens to point at. A gitlink (mode `160000`)
    // carries the oid of a commit that lives in the submodule's own repository
    // and is therefore normally absent from this object store, so asking for its
    // header first turned a healthy submodule named `.plombir-git-ci.yml` into
    // `failed to read CI config object header` — a storage 5xx for a tree shape
    // only the client can change. Classify by mode before any object lookup, the
    // way the workflow directory below already does.
    let not_a_file = match entry.mode().kind() {
        gix::object::tree::EntryKind::Blob | gix::object::tree::EntryKind::BlobExecutable => None,
        gix::object::tree::EntryKind::Tree => Some("a directory"),
        gix::object::tree::EntryKind::Link => Some("a symlink"),
        gix::object::tree::EntryKind::Commit => Some("a submodule"),
    };
    if let Some(shape) = not_a_file {
        return Err(rg_core::error::invalid_request(format!(
            "{ci_filename} at commit {commit_sha} is {shape}, not a file"
        )));
    }

    // An object that is simply gone is storage losing what the tree still
    // points at, and stays a server fault. The header is read for its size
    // anyway; the kind is the backstop for a tree whose mode and object
    // disagree — pushable, because receive-pack does not fsck what it accepts,
    // and correctable only by pushing a sound tree.
    let header = repo
        .find_header(entry.oid())
        .with_context(|| format!("failed to read CI config object header {ci_filename}"))?;
    if header.kind() != gix::object::Kind::Blob {
        return Err(rg_core::error::invalid_request(format!(
            "{ci_filename} at commit {commit_sha} is committed as a file, but its object is a {}",
            header.kind()
        )));
    }
    if header.size() > MAX_CI_CONFIG_BYTES {
        return Err(rg_core::error::invalid_request(format!(
            "{ci_filename} is larger than {MAX_CI_CONFIG_BYTES} bytes"
        )));
    }

    let object = entry
        .object()
        .with_context(|| format!("failed to read CI config object {ci_filename}"))?;
    // The object came out of the database fine; it is simply not a file. That
    // is the committed tree's shape, so it is the client's to fix.
    let blob = object
        .try_into_blob()
        .map_err(|_| rg_core::error::invalid_request(format!("{ci_filename} is not a file")))?;

    let ci_yml = String::from_utf8(blob.data.to_vec()).map_err(|_| {
        rg_core::error::invalid_request(format!("{ci_filename} is not valid UTF-8"))
    })?;

    // The reason (with its line/column) goes into the message, not into a
    // `with_context` source: callers log this with `Display`, and the client
    // that just typed the broken YAML is shown this text verbatim. Dumping the
    // whole file here used to bury the actual complaint. `rg_core::yaml`
    // reports only a line/column and its own complaint — no filesystem path and
    // no snippet of the file — so it is safe to hand over (H-05).
    let config: CiConfig = rg_core::yaml::from_str(&ci_yml).map_err(|e| {
        rg_core::error::invalid_request(format!("failed to parse {ci_filename}: {e}"))
    })?;

    Ok(config)
}

/// Read the validated `workflow_dispatch` forms committed at `commit_sha`.
///
/// This follows the same source loader, parser, and trigger-schema validator as
/// [`try_read_gitea_workflows`]. A broken committed workflow is therefore an
/// `InvalidRequest` naming its file, not an empty form that lets the web send a
/// request the real trigger will reject a moment later.
pub fn workflow_dispatch_schema(
    repo_path: &std::path::Path,
    commit_sha: &str,
) -> Result<rg_core::ci::WorkflowDispatchSchema> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository at {}", repo_path.display()))?;
    let Some(workflow_sources) = load_workflow_sources(&repo, commit_sha)? else {
        return Ok(rg_core::ci::WorkflowDispatchSchema::default());
    };
    let workflows = parse_gitea_workflows(&workflow_sources)?;
    Ok(WorkflowDispatchContract::from_workflows(&workflows)?.into_schema())
}

/// The `on: schedule` entries committed at `commit_sha`, one item per workflow
/// file that declares any, in file-name order.
///
/// Each file is judged on its own. One that does not parse, or whose `on:`
/// clause is refused, contributes nothing and is logged — the push that
/// committed it was already refused with the reason, and every *other* file's
/// schedule still stands. Whether the run a tick produces can be built is the
/// trigger's question, asked with the whole set as every event asks it.
pub fn scheduled_workflows(
    repo_path: &std::path::Path,
    commit_sha: &str,
) -> Result<Vec<rg_core::ci::WorkflowSchedule>> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository at {}", repo_path.display()))?;
    let Some(sources) = load_workflow_sources(&repo, commit_sha)? else {
        return Ok(Vec::new());
    };
    let mut schedules = Vec::new();
    for (name, yml) in sorted_workflows(&sources) {
        let workflow = match gitea_actions::GiteaWorkflow::parse(yml) {
            Ok(workflow) => workflow,
            Err(error) => {
                tracing::warn!(
                    workflow = %name,
                    commit_sha,
                    "a workflow that does not parse declares no schedule: {error:#}"
                );
                continue;
            }
        };
        if let Err(error) = workflow.validate_supported_triggers() {
            tracing::warn!(
                workflow = %name,
                commit_sha,
                "a workflow whose `on:` is refused declares no schedule: {error:#}"
            );
            continue;
        }
        let crons = workflow.schedules();
        if !crons.is_empty() {
            schedules.push(rg_core::ci::WorkflowSchedule {
                workflow: name.clone(),
                crons,
            });
        }
    }
    Ok(schedules)
}

/// [`read_ci_config`] in the shape the tests ask it.
///
/// The repository identity only reaches the expression context, and every test
/// in this file is about the config that comes out — so they name a fixed
/// `owner/repo` here instead of thirty times each. The one test that *is* about
/// the identity asserts it through `WorkflowContext` directly.
#[cfg(test)]
fn read_ci_config_for_test(
    repo_path: &std::path::Path,
    commit_sha: &str,
    ref_name: &str,
    event: &str,
    base_branch: Option<&str>,
    previous_sha: Option<&str>,
) -> Result<CiConfig> {
    read_ci_config(
        repo_path,
        RepositoryName {
            owner: "owner",
            name: "repo",
        },
        commit_sha,
        ref_name,
        event,
        base_branch,
        previous_sha,
    )
}

/// Resolve a commit once, then use tree lookup APIs whose `Option` means only
/// that a path is absent. `rev_parse_single("commit:path")` mixed a missing
/// path with every ref/object-store error behind one `Err` value.
fn tree_at_commit<'repo>(
    repo: &'repo gix::Repository,
    commit_sha: &str,
) -> Result<gix::Tree<'repo>> {
    repo.rev_parse_single(commit_sha)
        .with_context(|| format!("failed to resolve CI commit {commit_sha}"))?
        .object()
        .with_context(|| format!("failed to read CI commit {commit_sha}"))?
        .peel_to_tree()
        .with_context(|| format!("failed to read CI tree at commit {commit_sha}"))
}

/// Directory holding Gitea Actions workflow files, relative to the repo root.
const WORKFLOW_DIR: &str = ".gitea/workflows";

/// Largest native CI config or individual Actions workflow held for parsing.
///
/// CI configuration is read on every automatic or manual trigger, and parsing
/// needs the complete YAML document. One MiB is already far above a workflow a
/// person can reasonably review while keeping a committed blob from choosing
/// an unbounded allocation in the server.
const MAX_CI_CONFIG_BYTES: u64 = 1024 * 1024;

/// Largest complete set of Actions workflow sources retained for one trigger.
///
/// Local reusable workflows require the immutable set to remain available
/// while it is parsed. Bound that necessary aggregate independently from the
/// per-file ceiling so many individually acceptable files cannot multiply it.
const MAX_WORKFLOW_TOTAL_BYTES: u64 = 16 * 1024 * 1024;

/// Independent backstop for repositories made of tiny or empty workflows.
const MAX_WORKFLOW_FILE_COUNT: usize = 256;

/// Outcome of looking for Gitea Actions workflows at a commit.
enum GiteaWorkflows {
    /// The commit has no `.gitea/workflows` directory at all.
    Absent,
    /// The directory is there, but no workflow in it is triggered by this event.
    NoneTriggered,
    /// Merged configuration of every workflow triggered by this event.
    Config(CiConfig),
}

/// The one pipeline-wide request contract formed by every workflow that asks
/// for `workflow_dispatch` at this commit.
///
/// Plombir Git merges all matching workflow files into one pipeline, so the HTTP
/// request necessarily carries one input map. The declarations remain local:
/// each workflow receives only the keys it declared, while a key declared by
/// none of them is rejected once against the aggregate. The schema endpoint
/// builds this same value. Identical declarations of one name collapse into one
/// form field; incompatible declarations are rejected with both source files,
/// which keeps the form and trigger from independently inventing aggregation
/// rules or choosing a winner by iteration order.
struct WorkflowDispatchContract {
    workflows: Vec<rg_core::ci::WorkflowDispatchWorkflow>,
    inputs: Vec<rg_core::ci::WorkflowDispatchInput>,
    input_names_by_file: std::collections::HashMap<String, std::collections::HashSet<String>>,
}

impl WorkflowDispatchContract {
    fn from_workflows(workflows: &[(String, gitea_actions::GiteaWorkflow)]) -> Result<Self> {
        let mut schemas = Vec::new();
        let mut aggregate = std::collections::BTreeMap::<
            String,
            (rg_core::ci::WorkflowDispatchInput, String),
        >::new();
        let mut input_names_by_file = std::collections::HashMap::new();
        for (file_name, workflow) in workflows {
            let Some(inputs) = workflow.workflow_dispatch_input_schema().map_err(|error| {
                rg_core::error::invalid_request(format!(
                    "invalid inputs for {WORKFLOW_DIR}/{file_name}: {error:#}"
                ))
            })?
            else {
                continue;
            };
            let path = format!("{WORKFLOW_DIR}/{file_name}");
            for input in &inputs {
                match aggregate.entry(input.name.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((input.clone(), path.clone()));
                    }
                    std::collections::btree_map::Entry::Occupied(entry)
                        if entry.get().0 != *input =>
                    {
                        let mut paths = [entry.get().1.as_str(), path.as_str()];
                        paths.sort_unstable();
                        return Err(rg_core::error::invalid_request(format!(
                            "workflow_dispatch input '{}' has incompatible declarations in {} and {}",
                            input.name, paths[0], paths[1]
                        )));
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {}
                }
            }
            input_names_by_file.insert(
                file_name.clone(),
                inputs.iter().map(|input| input.name.clone()).collect(),
            );
            schemas.push(rg_core::ci::WorkflowDispatchWorkflow {
                path,
                name: workflow
                    .name
                    .clone()
                    .unwrap_or_else(|| workflow_prefix(file_name).to_owned()),
                inputs,
            });
        }
        Ok(Self {
            workflows: schemas,
            inputs: aggregate
                .into_values()
                .map(|(input, _path)| input)
                .collect(),
            input_names_by_file,
        })
    }

    fn into_schema(self) -> rg_core::ci::WorkflowDispatchSchema {
        rg_core::ci::WorkflowDispatchSchema {
            workflows: self.workflows,
            inputs: self.inputs,
        }
    }

    fn validate_provided(
        &self,
        provided: &std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let mut unknown = provided
            .keys()
            .filter(|name| {
                !self
                    .input_names_by_file
                    .values()
                    .any(|declared| declared.contains(*name))
            })
            .cloned()
            .collect::<Vec<_>>();
        unknown.sort();
        if unknown.is_empty() {
            return Ok(());
        }

        let mut paths = self
            .workflows
            .iter()
            .map(|workflow| workflow.path.as_str())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        anyhow::bail!(
            "workflow_dispatch received undeclared input(s): {}; none are declared by the matching workflows: {}",
            unknown.join(", "),
            paths.join(", ")
        )
    }

    fn inputs_for(
        &self,
        file_name: &str,
        provided: &std::collections::HashMap<String, String>,
    ) -> std::collections::HashMap<String, String> {
        let Some(declared) = self.input_names_by_file.get(file_name) else {
            return std::collections::HashMap::new();
        };
        provided
            .iter()
            .filter(|(name, _)| declared.contains(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

/// Parse and validate every committed workflow before event matching.
///
/// A workflow that names a trigger Plombir Git cannot emit will never match, so
/// validation placed inside the triggered branch would make that declaration
/// silently unreachable. Both the schema probe and the real trigger go through
/// this helper to preserve the same fail-loud boundary and file-qualified
/// diagnostics.
fn parse_gitea_workflows(
    workflow_sources: &std::collections::HashMap<String, String>,
) -> Result<Vec<(String, gitea_actions::GiteaWorkflow)>> {
    sorted_workflows(workflow_sources)
        .into_iter()
        .map(|(file_name, yml)| {
            let workflow = gitea_actions::GiteaWorkflow::parse(yml).map_err(|error| {
                rg_core::error::invalid_request(format!(
                    "failed to parse {WORKFLOW_DIR}/{file_name}: {error}"
                ))
            })?;
            workflow.validate_supported_triggers().map_err(|error| {
                rg_core::error::invalid_request(format!(
                    "unsupported trigger in {WORKFLOW_DIR}/{file_name}: {error:#}"
                ))
            })?;
            Ok((file_name.clone(), workflow))
        })
        .collect()
}

/// Try to find and parse Gitea Actions workflow files in `.gitea/workflows/`.
///
/// `Absent` / `NoneTriggered` are the two legitimate fallbacks to the native
/// `.plombir-git-ci.yml`, and they are told apart so the caller can explain which
/// one happened.
///
/// Anything else — a workflow that is not valid UTF-8, does not parse as YAML,
/// references a missing reusable workflow, or uses an unsupported action — is an
/// `Err` naming the offending file, so a typo in a workflow surfaces as a
/// parsing error instead of being mistaken for "no CI config at all".
fn try_read_gitea_workflows(
    repo: &gix::Repository,
    repository: RepositoryName<'_>,
    commit_sha: &str,
    ref_name: &str,
    invocation: WorkflowInvocation<'_>,
) -> Result<GiteaWorkflows> {
    let Some(workflow_sources) = load_workflow_sources(repo, commit_sha)? else {
        // Nothing at that path in this commit: the native format is next in line.
        return Ok(GiteaWorkflows::Absent);
    };

    let match_branch = event_match_branch(repo, invocation.base_branch)?;
    let changed = gitea_actions::ChangedPaths::of_event(
        repo,
        invocation.event,
        &match_branch,
        invocation.previous_sha,
        commit_sha,
    );

    let workflows = parse_gitea_workflows(&workflow_sources)?;
    let dispatch_contract = if invocation.event == rg_core::ci::WORKFLOW_DISPATCH_EVENT {
        let contract = WorkflowDispatchContract::from_workflows(&workflows)?;
        if contract.workflows.is_empty() {
            None
        } else {
            contract
                .validate_provided(
                    invocation
                        .inputs
                        .unwrap_or(&std::collections::HashMap::new()),
                )
                .map_err(|error| rg_core::error::invalid_request(format!("{error:#}")))?;
            Some(contract)
        }
    } else {
        None
    };

    let mut all_jobs: std::collections::HashMap<String, config::JobConfig> =
        std::collections::HashMap::new();
    let mut all_stages: Vec<String> = Vec::new();
    // `(workflow file, resolved concurrency)` for every triggered workflow that
    // declared a `concurrency:` block. See `merge_concurrency` for why the list
    // is kept rather than folded as it goes.
    let mut declared_concurrency: Vec<(String, config::ConcurrencyConfig)> = Vec::new();

    // A tick is for one `- cron:` entry of one file, and nothing else runs for
    // it — not the other workflows that also declare `schedule`, not a second
    // file that happens to spell the same cron.
    let scheduled = invocation
        .schedule
        .filter(|_| invocation.event == rg_core::ci::SCHEDULE_EVENT);
    let (head_ref, base_ref) = rg_core::ci::pull_request_refs(
        invocation.event,
        invocation.head_branch,
        invocation.base_branch,
    );
    for (name, mut workflow) in workflows {
        // Check if this workflow should be triggered
        let triggered = match scheduled {
            Some(run) => name == run.workflow && workflow.matches_schedule(run.cron),
            None => workflow.matches_event(invocation.event, ref_name, &match_branch, &changed),
        };
        if !triggered {
            continue;
        }
        if let Some(contract) = &dispatch_contract {
            let provided = contract.inputs_for(
                &name,
                invocation
                    .inputs
                    .unwrap_or(&std::collections::HashMap::new()),
            );
            workflow.resolve_dispatch_inputs(&provided).map_err(|e| {
                rg_core::error::invalid_request(format!(
                    "invalid inputs for {WORKFLOW_DIR}/{name}: {e:#}"
                ))
            })?;
        }
        // Before the reusable workflows are expanded: each of those reads its
        // own `inputs.*` as it is expanded, not this file's.
        workflow.resolve_condition_inputs().map_err(|reason| {
            rg_core::error::invalid_request(format!(
                "unsupported workflow {WORKFLOW_DIR}/{name}: {reason}"
            ))
        })?;
        let mut workflow = workflow
            .expand_local_reusable_workflows(&workflow_sources)
            .map_err(|e| {
                rg_core::error::invalid_request(format!(
                    "failed to expand {WORKFLOW_DIR}/{name}: {e:#}"
                ))
            })?;
        // After the reusable workflows, so the jobs they bring load their
        // actions too; before the check, so what it judges is the steps that
        // will run rather than the `uses:` that named them.
        if let Some(loader) = invocation.actions {
            actions::expand_actions(&mut workflow, loader)?.map_err(|reason| {
                rg_core::error::invalid_request(format!(
                    "cannot load the actions of {WORKFLOW_DIR}/{name}: {reason}"
                ))
            })?;
        }
        // After the expansions, so a job a reusable workflow or an action
        // brings reads the event of this run too.
        workflow
            .resolve_event_inputs(invocation.event)
            .map_err(|reason| {
                rg_core::error::invalid_request(format!(
                    "unsupported workflow {WORKFLOW_DIR}/{name}: {reason}"
                ))
            })?;
        // After the actions, so a step an action brings reads them too; before
        // the check, which refuses any `vars.*` still standing.
        if let Some(vars) = invocation.vars {
            workflow.resolve_vars(vars).map_err(|reason| {
                rg_core::error::invalid_request(format!(
                    "cannot read the variables of {WORKFLOW_DIR}/{name}: {reason}"
                ))
            })?;
        }
        workflow.validate_supported_actions().map_err(|e| {
            rg_core::error::invalid_request(format!(
                "unsupported workflow {WORKFLOW_DIR}/{name}: {e:#}"
            ))
        })?;
        // After the expansion, not before it: the stage pass reads the
        // prefixed, flattened job names, and a `needs:` entry naming a job that
        // does not exist among *those* is dropped rather than refused — the job
        // then runs in stage 0, next to whatever it declared it was waiting for
        // (card_85ba100789a8).
        workflow.validate_job_dependencies().map_err(|e| {
            rg_core::error::invalid_request(format!(
                "invalid job dependencies in {WORKFLOW_DIR}/{name}: {e:#}"
            ))
        })?;

        tracing::info!("Triggering workflow from {}/{}", WORKFLOW_DIR, name);

        let ctx = gitea_actions::WorkflowContext {
            ref_name: ref_name.to_string(),
            sha: commit_sha.to_string(),
            event: invocation.event.to_string(),
            repo_owner: repository.owner.to_string(),
            repo_name: repository.name.to_string(),
            event_schedule: scheduled.map(|run| run.cron.to_string()),
            head_ref: head_ref.to_string(),
            base_ref: base_ref.to_string(),
        };

        let mut wf_config = workflow.to_ci_config(&ctx);

        // `concurrency.group` is expanded here rather than at trigger time: it
        // is built from `${{ github.* }}`, and only this loop still holds the
        // workflow that `github.workflow` names.
        if let Some(mut concurrency) = wf_config.concurrency.take() {
            let label = workflow
                .name
                .clone()
                .unwrap_or_else(|| workflow_prefix(&name).to_string());
            concurrency.group =
                gitea_actions::expand_concurrency_group(&concurrency.group, &ctx, &label);
            declared_concurrency.push((name.clone(), concurrency));
        }

        // Prefix job names with workflow filename to avoid collisions
        let wf_prefix = workflow_prefix(&name);
        let mut renamed_jobs = std::collections::HashMap::new();
        for (job_name, mut job) in wf_config.jobs {
            let new_name = format!("{}/{}", wf_prefix, job_name);
            // The jobs a job needs are renamed with it, so the names its steps
            // resolve `needs.*` against are the names the pipeline carries.
            if let Some(context) = job.job_context.as_mut() {
                for needed in context.needs.values_mut() {
                    *needed = format!("{wf_prefix}/{needed}");
                }
            }
            // Prefix stage names too
            if let Some(ref stage) = job.stage {
                job.stage = Some(format!("{}/{}", wf_prefix, stage));
            }
            renamed_jobs.insert(new_name, job);
        }
        wf_config.jobs = renamed_jobs;

        // Add stages
        if let Some(ref stages) = wf_config.stages {
            for stage in stages {
                all_stages.push(format!("{}/{}", wf_prefix, stage));
            }
        }

        // Merge jobs
        all_jobs.extend(wf_config.jobs);
    }

    if all_jobs.is_empty() {
        tracing::debug!(
            "No workflow in {}/ is triggered by event {} on {}; trying the native CI config",
            WORKFLOW_DIR,
            invocation.event,
            ref_name
        );
        return Ok(GiteaWorkflows::NoneTriggered);
    }

    Ok(GiteaWorkflows::Config(CiConfig {
        stages: Some(all_stages),
        concurrency: merge_concurrency(declared_concurrency)?,
        jobs: all_jobs,
        actions_workflow: true,
    }))
}

/// The one `concurrency:` the merged pipeline can carry, or an error naming the
/// workflows that disagree.
///
/// In Actions, `concurrency` lives on a workflow; here every workflow triggered
/// by one event is merged into a single pipeline, and a pipeline has one
/// `concurrency_group` column. The old code resolved that mismatch by dropping
/// the field on the floor — parsed, mapped, and then `None` with a comment —
/// so a repository declaring `cancel-in-progress: true` got neither
/// serialization nor cancellation, and no error saying why (card_f4309bc397b2).
///
/// Two of the three ways out are wrong for this engine. Picking one workflow's
/// group silently subjects the *other* workflows' jobs to somebody else's
/// cancellation — the exact defect commit b4bf9db removed from the lookup.
/// Inventing a composite group ("A+B") invents serialization semantics nobody
/// declared. So: agreement is honoured, disagreement is refused by name, which
/// is the same answer this engine already gives for an unsupported trigger or
/// an unsupported action.
fn merge_concurrency(
    declared: Vec<(String, config::ConcurrencyConfig)>,
) -> Result<Option<config::ConcurrencyConfig>> {
    let mut declared = declared.into_iter();
    let Some((first_file, first)) = declared.next() else {
        return Ok(None);
    };

    for (file, other) in declared {
        if other.group != first.group || other.cancel_in_progress != first.cancel_in_progress {
            return Err(rg_core::error::invalid_request(format!(
                "{WORKFLOW_DIR}/{first_file} and {WORKFLOW_DIR}/{file} are both triggered by this \
                 event and declare different `concurrency:` blocks (group '{}' \
                 cancel-in-progress: {} vs group '{}' cancel-in-progress: {}). \
                 Plombir Git runs every workflow triggered by one event as a single pipeline, which \
                 carries one concurrency group — make the blocks agree, or split the workflows \
                 onto different events.",
                first.group, first.cancel_in_progress, other.group, other.cancel_in_progress
            )));
        }
    }

    Ok(Some(first))
}

/// Read every `*.yml` / `*.yaml` blob under [`WORKFLOW_DIR`] at `commit_sha`.
///
/// `None` means the directory is not in this commit at all — the one outcome
/// that legitimately falls back to the native config. Everything else that goes
/// wrong is an `Err` naming the file: a workflow that exists but cannot be read
/// must never be mistaken for "no CI config here".
///
/// Sources are loaded as a set rather than one at a time so a workflow can
/// resolve a repository-local reusable workflow from the same immutable tree.
fn load_workflow_sources(
    repo: &gix::Repository,
    commit_sha: &str,
) -> Result<Option<std::collections::HashMap<String, String>>> {
    load_workflow_sources_with_limits(
        repo,
        commit_sha,
        MAX_WORKFLOW_FILE_COUNT,
        MAX_WORKFLOW_TOTAL_BYTES,
    )
}

fn load_workflow_sources_with_limits(
    repo: &gix::Repository,
    commit_sha: &str,
    max_file_count: usize,
    max_total_bytes: u64,
) -> Result<Option<std::collections::HashMap<String, String>>> {
    let tree = tree_at_commit(repo, commit_sha)?;
    let Some(workflow_dir) = tree.lookup_entry_by_path(WORKFLOW_DIR).with_context(|| {
        format!(
            "failed to look up {} at commit {}",
            WORKFLOW_DIR, commit_sha
        )
    })?
    else {
        return Ok(None);
    };

    // Classified from the committed mode before the object is fetched, for the
    // same reason the native config reader above does it: a gitlink here holds
    // the oid of a commit belonging to the submodule's own repository, and that
    // oid is legitimately absent from this object store. Fetching first reported
    // a submodule mounted at `.gitea/workflows` as `failed to read .gitea/
    // workflows` — a storage 5xx for a tree shape only the client can change.
    let not_a_directory = match workflow_dir.mode().kind() {
        gix::object::tree::EntryKind::Tree => None,
        gix::object::tree::EntryKind::Blob | gix::object::tree::EntryKind::BlobExecutable => {
            Some("a file")
        }
        gix::object::tree::EntryKind::Link => Some("a symlink"),
        gix::object::tree::EntryKind::Commit => Some("a submodule"),
    };
    if let Some(shape) = not_a_directory {
        return Err(rg_core::error::invalid_request(format!(
            "{WORKFLOW_DIR} exists at commit {commit_sha} but is {shape}, not a directory"
        )));
    }

    let object = workflow_dir
        .object()
        .with_context(|| format!("failed to read {} at commit {}", WORKFLOW_DIR, commit_sha))?;
    // Past the mode check this is a tree whose mode and object disagree: still
    // the committed shape, and still fixed by pushing a sound one.
    let tree = object.try_into_tree().map_err(|_| {
        rg_core::error::invalid_request(format!(
            "{WORKFLOW_DIR} at commit {commit_sha} is committed as a directory, but its object is not a tree"
        ))
    })?;

    let mut workflow_sources = std::collections::HashMap::new();
    let mut workflow_file_count = 0usize;
    let mut workflow_total_bytes = 0u64;
    for entry in tree.iter() {
        let entry = entry
            .with_context(|| format!("failed to list {} at commit {}", WORKFLOW_DIR, commit_sha))?;
        let name = entry.filename().to_string();
        if !name.ends_with(".yml") && !name.ends_with(".yaml") {
            continue;
        }
        let mode = entry.mode();
        if !mode.is_blob() && !mode.is_executable() {
            // A directory or submodule that happens to be named `*.yml` is not a
            // workflow; say so instead of failing the whole pipeline over it.
            tracing::warn!("Skipping {}/{}: not a regular file", WORKFLOW_DIR, name);
            continue;
        }

        let workflow_path = format!("{WORKFLOW_DIR}/{name}");
        workflow_file_count = workflow_file_count
            .checked_add(1)
            .filter(|count| *count <= max_file_count)
            .ok_or_else(|| {
                rg_core::error::invalid_request(format!(
                    "{WORKFLOW_DIR} contains more than {max_file_count} workflow files; limit reached at {workflow_path}"
                ))
            })?;

        let header = repo
            .find_header(entry.oid())
            .with_context(|| format!("failed to read object header for {workflow_path}"))?;
        if header.kind() != gix::object::Kind::Blob {
            return Err(rg_core::error::invalid_request(format!(
                "{workflow_path} is not a file"
            )));
        }
        let blob_size = header.size();
        if blob_size > MAX_CI_CONFIG_BYTES {
            return Err(rg_core::error::invalid_request(format!(
                "{workflow_path} is larger than {MAX_CI_CONFIG_BYTES} bytes"
            )));
        }
        workflow_total_bytes = workflow_total_bytes
            .checked_add(blob_size)
            .filter(|total| *total <= max_total_bytes)
            .ok_or_else(|| {
                rg_core::error::invalid_request(format!(
                    "workflow files exceed the {max_total_bytes}-byte total limit at {workflow_path}"
                ))
            })?;

        let entry_object = repo.find_object(entry.oid()).with_context(|| {
            format!(
                "failed to read {}/{} from the object database",
                WORKFLOW_DIR, name
            )
        })?;
        let blob = entry_object.try_into_blob().map_err(|_| {
            rg_core::error::invalid_request(format!("{WORKFLOW_DIR}/{name} is not a file"))
        })?;
        let yml = String::from_utf8(blob.data.to_vec()).map_err(|_| {
            rg_core::error::invalid_request(format!("{WORKFLOW_DIR}/{name} is not valid UTF-8"))
        })?;
        workflow_sources.insert(name, yml);
    }
    Ok(Some(workflow_sources))
}

/// Workflow sources by filename, in a deterministic order.
///
/// Stage ordering of the merged config — and which broken file is reported
/// first — must not depend on hash-map iteration order.
/// A workflow file's basename without its YAML extension — the prefix its jobs
/// and stages carry into the merged pipeline, and the label `github.workflow`
/// falls back to when the file declares no `name:`.
fn workflow_prefix(file_name: &str) -> &str {
    file_name.trim_end_matches(".yml").trim_end_matches(".yaml")
}

fn sorted_workflows(
    sources: &std::collections::HashMap<String, String>,
) -> Vec<(&String, &String)> {
    let mut workflows: Vec<(&String, &String)> = sources.iter().collect();
    workflows.sort_by_key(|(name, _)| *name);
    workflows
}

/// The branch `on:`-filters are matched against for this event.
///
/// The caller's `base_branch` when it has one (a PR's target branch), the
/// repository's default branch otherwise.
fn event_match_branch(repo: &gix::Repository, base_branch: Option<&str>) -> Result<String> {
    match base_branch {
        Some(base_branch) => Ok(base_branch.to_string()),
        None => get_default_branch(repo),
    }
}

/// Whether any workflow at `commit_sha` is triggered by `event`.
///
/// Cheaper and narrower than [`read_ci_config`]: it only asks the `on:` block,
/// so a workflow that this event does not select is never expanded or job-validated.
/// A file that fails to parse or declares an unsupported trigger cannot answer,
/// so the checked caller receives a typed configuration refusal. The legacy
/// bool probe still logs that error and returns `false`; automatic PR producers
/// use the checked form so the refusal can become a durable failed pipeline.
/// The event query in the shape the tests ask it. Push-like events have no
/// previous revision and fall back to the commit's own diff; pull requests use
/// the merge base of their target branch and head.
#[cfg(test)]
fn workflow_matches_event_at(
    repo_path: &std::path::Path,
    commit_sha: &str,
    event: &str,
    ref_name: &str,
    base_branch: Option<&str>,
) -> Result<bool> {
    workflow_matches_event(rg_core::ci::WorkflowEventQuery {
        repo_path,
        commit_sha,
        event,
        ref_name,
        base_branch,
        previous_sha: None,
    })
}

fn workflow_matches_event(query: rg_core::ci::WorkflowEventQuery<'_>) -> Result<bool> {
    let rg_core::ci::WorkflowEventQuery {
        repo_path,
        commit_sha,
        event,
        ref_name,
        base_branch,
        previous_sha,
    } = query;
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let Some(sources) = load_workflow_sources(&repo, commit_sha)? else {
        return Ok(false);
    };
    let match_branch = event_match_branch(&repo, base_branch)?;
    let changed = gitea_actions::ChangedPaths::of_event(
        &repo,
        event,
        &match_branch,
        previous_sha,
        commit_sha,
    );

    for (name, yml) in sorted_workflows(&sources) {
        let workflow = gitea_actions::GiteaWorkflow::parse(yml).map_err(|error| {
            rg_core::error::invalid_request(format!(
                "failed to parse {WORKFLOW_DIR}/{name} while matching event {event}: {error}"
            ))
        })?;
        workflow.validate_supported_triggers().map_err(|error| {
            rg_core::error::invalid_request(format!(
                "unsupported trigger in {WORKFLOW_DIR}/{name}: {error:#}"
            ))
        })?;
        if workflow.matches_event(event, ref_name, &match_branch, &changed) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The branch a detached `HEAD` is matched against, for lack of a better name.
const DETACHED_HEAD_BRANCH: &str = "main";

/// Get the default branch name of the repository.
///
/// `HEAD` is the only source: a symbolic `HEAD` names the branch, and an unborn
/// `HEAD` — a repository whose branch has no commit yet — still names the branch
/// it is waiting for, which is what `git symbolic-ref HEAD` reports. A detached
/// `HEAD` names no branch at all, and that single state is the documented
/// fallback to [`DETACHED_HEAD_BRANCH`]; it is logged, because the name is a
/// guess and the `on:`-filters are matched against it.
///
/// Everything else is an error. A `HEAD` that cannot be read, a target ref that
/// is corrupt, or a branch name that is not UTF-8 used to land in the very same
/// fallback as an honestly branch-less repository — so a workflow filtered on
/// `main` would run, and one filtered on the real default branch would be
/// silently skipped, with nothing in the log either way.
fn get_default_branch(repo: &gix::Repository) -> Result<String> {
    let head = repo
        .head()
        .context("failed to read HEAD while resolving the default branch")?;

    let Some(referent) = head.referent_name() else {
        tracing::warn!(
            "repository HEAD is detached; matching workflow branch filters against {}",
            DETACHED_HEAD_BRANCH
        );
        return Ok(DETACHED_HEAD_BRANCH.to_string());
    };

    let short = referent.shorten();
    short
        .to_str()
        .map(str::to_string)
        .with_context(|| format!("default branch name is not valid UTF-8: {short:?}"))
}

// M-14: has_ci_config moved to rg_core::ci::has_ci_config and re-exported above.

/// Stand-in for `rg_http::ws::NotificationHub` — this crate cannot depend on the
/// HTTP layer, and the seam under test is the `PushNotifier` trait anyway.
#[cfg(test)]
pub(crate) mod test_notifier {
    #[derive(Default)]
    pub(crate) struct RecordingNotifier {
        events: std::sync::Mutex<Vec<(i64, String)>>,
    }

    impl RecordingNotifier {
        pub(crate) fn events(&self) -> Vec<(i64, String)> {
            self.events.lock().expect("recorder mutex").clone()
        }
    }

    impl rg_core::push_hooks::PushNotifier for RecordingNotifier {
        fn notify(&self, user_id: i64, event_type: &str, _data: serde_json::Value) {
            self.events
                .lock()
                .expect("recorder mutex")
                .push((user_id, event_type.to_string()));
        }
    }

    /// A wiring bundle whose notifier is the returned recorder.
    pub(crate) fn wiring() -> (std::sync::Arc<RecordingNotifier>, super::CiNotifications) {
        let recorder = std::sync::Arc::new(RecordingNotifier::default());
        let notifications = super::CiNotifications {
            notifier: Some(recorder.clone()),
            smtp_config: Some(rg_core::email::SmtpConfig {
                host: "smtp.example.com".into(),
                port: 587,
                user: "plombir-git".into(),
                pass: "unused".into(),
                from: "ci@example.com".into(),
            }),
        };
        (recorder, notifications)
    }
}

/// The hooks a finished pipeline runs must reach the same sinks the HTTP layer's
/// hooks do. Both CI-completion paths used to build their `PostPushContext` with
/// `notifier: None` / `smtp_config: None`, so a merge that auto-merge landed on
/// green CI produced no real-time `push` event and no mail, while the identical
/// merge over REST produced both (card_85b8d59246b5).
#[cfg(test)]
mod notification_wiring_tests {
    use super::test_notifier::wiring;
    use super::*;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    #[test]
    fn the_completion_path_hands_its_hooks_the_process_hub_and_smtp() {
        let (recorder, notifications) = wiring();
        let engine = CiEngine::with_notifications_and_job_timeout(notifications, 731);

        let context = post_push_context(
            std::path::Path::new("/srv/repos"),
            false,
            false,
            false,
            None,
            None,
            None,
            &engine,
        );

        let notifier = context
            .notifier
            .expect("a merge CI unblocks owes the same real-time events as a merge over REST");
        notifier.notify(7, "push", serde_json::json!({}));
        assert_eq!(recorder.events(), vec![(7, "push".to_string())]);
        assert!(
            context.smtp_config.is_some(),
            "the 'CI pipeline triggered' mail is part of the same effect set"
        );
    }

    /// The engine's shutdown signal must reach the runner it builds.
    ///
    /// The runner honouring the signal and the engine handing it over are two
    /// separate absences, and the runner's own test proves only the first: it
    /// constructs the runner itself. Without this, deleting
    /// `runner.set_shutdown(...)` from `build_internal_runner` leaves every
    /// runner the server actually spawns deaf, and every test still green
    /// (card_34368880dc20).
    #[tokio::test]
    async fn the_shutdown_signal_reaches_the_runner_the_engine_builds() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let wired = CiEngine::new().with_shutdown(shutdown_rx);

        let runner = build_internal_runner(
            db.clone(),
            std::path::Path::new("/srv/repos"),
            std::path::Path::new("/srv/repos/o/r.git"),
            1,
            1,
            false,
            false,
            None,
            None,
            None,
            &wired,
        );
        assert!(
            runner.hears_shutdown(),
            "the runner the engine spawns cannot observe the signal the process fans out"
        );

        // And the other way: an engine with no signal — a one-off `rg-cli`
        // command, a test — must not invent one, or a pipeline would stop on a
        // channel nobody owns.
        let standalone = build_internal_runner(
            db,
            std::path::Path::new("/srv/repos"),
            std::path::Path::new("/srv/repos/o/r.git"),
            1,
            1,
            false,
            false,
            None,
            None,
            None,
            &CiEngine::new(),
        );
        assert!(!standalone.hears_shutdown());
    }

    /// The pipeline is spawned onto the CI tracker, not into the void.
    ///
    /// Read out of the source because the alternative is not observable from a
    /// test: a process-global tracker is shared by every test in this binary,
    /// so `len()` says nothing, and the failure this guards against —
    /// `tokio::spawn` again — produces a task that works perfectly right up
    /// until the one moment it is asked to stop. The signal reaching the runner
    /// is not a substitute: an untracked runner observes the stop and is then
    /// severed mid-unwind, before the row it is handing back is written
    /// (card_34368880dc20).
    #[test]
    fn the_embedded_pipeline_is_spawned_onto_the_ci_tracker() {
        let source = include_str!("lib.rs");

        let bare = rust_source::production_function_call_sites(
            source,
            "spawn_internal_runner",
            &["tokio::spawn"],
        );
        assert!(
            bare.is_empty(),
            "`spawn_internal_runner` spawns the pipeline with a bare `tokio::spawn` at line {:?}              — a task owned by nobody, severed by SIGTERM wherever it stands",
            bare.iter().map(|call| call.line).collect::<Vec<_>>()
        );

        let tracked = rust_source::production_function_call_sites(
            source,
            "spawn_internal_runner",
            &["rg_core::task_tracker::ci_tracker"],
        );
        assert!(
            !tracked.is_empty(),
            "`spawn_internal_runner` must spawn through `ci_tracker()`, which is what the stop \
             path waits on"
        );
    }

    /// The claim on the pipeline is taken before the task, not inside it.
    ///
    /// Unlike the tracker above, this one *is* observable from a test: the
    /// registry is keyed by pipeline id, so an id no other test uses answers for
    /// this call alone. The ordering matters as much as the claim — a lease
    /// taken inside the spawned task leaves a window where a watchdog tick reads
    /// the pipeline as unowned and starts a second runner beside this one, which
    /// is the double execution the whole registry exists to prevent
    /// (card_111ac7923d7c).
    #[tokio::test]
    async fn the_embedded_runner_claims_its_pipeline_before_it_is_spawned() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        let pipeline_id = 987_654_321;
        assert!(
            !rg_core::ci::embedded_runners::has_embedded_runner(pipeline_id),
            "the test picked a pipeline id somebody else is already using"
        );

        spawn_internal_runner(
            &db,
            std::path::Path::new("/nonexistent"),
            std::path::Path::new("/nonexistent/owner/repo.git"),
            1,
            pipeline_id,
            false,
            false,
            None,
            None,
            None,
            &CiEngine::new(),
        );

        assert!(
            rg_core::ci::embedded_runners::has_embedded_runner(pipeline_id),
            "`spawn_internal_runner` returned without claiming its pipeline — a watchdog tick \
             landing here would read it as abandoned and start a second runner"
        );
    }

    #[tokio::test]
    async fn the_operator_timeout_reaches_both_embedded_runner_modes() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        let engine = CiEngine::with_notifications_and_job_timeout(CiNotifications::default(), 731);

        for docker_enabled in [false, true] {
            let runner = build_internal_runner(
                db.clone(),
                std::path::Path::new("/srv/repos"),
                std::path::Path::new("/srv/repos/o/r.git"),
                1,
                2,
                docker_enabled,
                false,
                None,
                None,
                None,
                &engine,
            );
            assert_eq!(
                runner.job_timeout_secs, 731,
                "runner mode docker_enabled={docker_enabled} replaced the operator timeout"
            );
        }
    }

    /// The merge commit gets a pipeline of its own, and that pipeline can unblock
    /// the next merge, so the wiring has to survive an arbitrary number of hops
    /// rather than only the first. This pins [`nested_engine`]'s own contract;
    /// that `post_push_context` feeds it the *real* wiring is pinned by the
    /// notifier assertion above (both read the same argument).
    #[test]
    fn the_engine_the_hooks_trigger_through_keeps_the_same_wiring() {
        let (recorder, notifications) = wiring();

        let configured = CiEngine::with_notifications_and_job_timeout(notifications, 731);
        let mut engine = nested_engine(&configured);
        for hop in 1..=3 {
            assert_eq!(
                engine.job_timeout_secs(),
                731,
                "hop {hop} replaced the operator's embedded-runner timeout"
            );
            let notifier = engine
                .notifications
                .notifier
                .clone()
                .unwrap_or_else(|| panic!("hop {hop} lost the notification hub"));
            notifier.notify(hop, "ci_triggered", serde_json::json!({}));
            assert!(
                engine.notifications.smtp_config.is_some(),
                "hop {hop} lost the SMTP configuration"
            );
            engine = nested_engine(&engine);
        }

        assert_eq!(
            recorder.events(),
            vec![
                (1, "ci_triggered".to_string()),
                (2, "ci_triggered".to_string()),
                (3, "ci_triggered".to_string()),
            ]
        );
    }

    /// The default engine is the one a process without a hub builds, and it must
    /// stay silent rather than pretend — the assertion above would pass on any
    /// engine if `CiNotifications` were populated from somewhere else.
    #[test]
    fn an_unwired_engine_carries_no_sinks() {
        let engine = CiEngine::new();
        let context = post_push_context(
            std::path::Path::new("/srv/repos"),
            false,
            false,
            false,
            None,
            None,
            None,
            &engine,
        );
        assert!(context.notifier.is_none());
        assert!(context.smtp_config.is_none());
    }
}

#[cfg(test)]
mod matrix_tests {
    use super::*;
    use sea_orm::{NotSet, Set};
    use std::collections::{BTreeMap, HashMap};

    #[allow(dead_code)]
    pub(super) mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn config(matrix: BTreeMap<String, Vec<String>>) -> config::JobConfig {
        config::JobConfig {
            stage: Some("test".into()),
            script: vec!["echo ok".into()],
            image: None,
            only: None,
            variables: Some(HashMap::from([("BASE".into(), "yes".into())])),
            when: None,
            condition: None,
            environment: None,
            allow_failure: None,
            timeout_seconds: None,
            tags: None,
            matrix: Some(matrix),
            cache: None,
            artifacts: None,
            action_templates: None,
            steps: None,
            job_context: None,
            actions_matrix: None,
            matrix_strategy: None,
        }
    }

    /// A matrix key that is no variable name (`node-version`) still has a
    /// value in the job fields resolved per variant: they read the variant's
    /// coordinates, not its variables, which only carry `MATRIX_NODE_VERSION`.
    #[test]
    fn a_dashed_matrix_key_resolves_in_runs_on() {
        let workflow = gitea_actions::GiteaWorkflow::parse(
            "on: push\njobs:\n  test:\n    runs-on: node-${{ matrix.node-version }}\n    strategy:\n      matrix:\n        node-version: [18]\n    steps:\n      - run: echo ok\n",
        )
        .unwrap();
        let config = workflow.to_ci_config(&gitea_actions::WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
            event_schedule: None,
            head_ref: String::new(),
            base_ref: String::new(),
        });
        let job = &config.jobs["test"];
        let variant = expand_matrix("test", job).unwrap().remove(0);
        let fields = resolve_action_job_fields(
            "test",
            job,
            &variant,
            "refs/heads/main",
            "push",
            "abc",
            RepositoryName {
                owner: "o",
                name: "r",
            },
        )
        .unwrap();
        assert_eq!(fields.tags, Some(vec!["node-18".to_string()]));
    }

    #[test]
    fn expands_cartesian_matrix_with_deterministic_names_and_variables() {
        let variants = expand_matrix(
            "test",
            &config(BTreeMap::from([
                ("os".into(), vec!["linux".into(), "macos".into()]),
                ("rust".into(), vec!["stable".into(), "beta".into()]),
            ])),
        )
        .unwrap();
        assert_eq!(variants.len(), 4);
        assert_eq!(variants[0].name, "test [os=linux, rust=stable]");
        assert_eq!(variants[0].variables["MATRIX_OS"], "linux");
        assert_eq!(variants[0].variables["BASE"], "yes");
    }

    #[test]
    fn rejects_excessive_matrix() {
        let error = expand_matrix(
            "huge",
            &config(BTreeMap::from([
                ("a".into(), (0..17).map(|v| v.to_string()).collect()),
                ("b".into(), (0..17).map(|v| v.to_string()).collect()),
            ])),
        )
        .unwrap_err();
        assert!(error.to_string().contains("maximum is 256"));
        // Reached from the pipeline build inside `trigger_pipeline`, so an
        // oversized matrix is the client's file being wrong, not a server fault.
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "an oversized matrix must be a 400: {error:#}"
        );
    }

    #[tokio::test]
    async fn matrix_job_conditions_persist_and_skip_only_false_variants() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".plombir-git-ci.yml"),
            "stages: [test, deploy]\nconditional:\n  stage: test\n  if: matrix.os == 'linux' && github.ref_name == 'main'\n  script: [echo ok]\n  matrix:\n    os: [linux, macos]\ndeploy:\n  stage: deploy\n  if: github.ref_name == 'main'\n  script: [echo deploy]\n",
        ).unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(temp.path())).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(temp.path())
            )
            .unwrap()
            .success());
        assert!(git
            .run(&["add", ".plombir-git-ci.yml"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "conditional"], Some(temp.path()))
            .unwrap()
            .success());
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_string();
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("conditions.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "condition-owner",
            "condition@example.com",
            "unused",
            "Condition Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("conditions".into()),
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
        .unwrap();
        let pipeline_id = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .unwrap();
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 3);
        let runnable = jobs
            .iter()
            .find(|job| job.name.contains("os=linux"))
            .unwrap();
        assert_eq!(runnable.status, "pending");
        let skipped = jobs
            .iter()
            .find(|job| job.name.contains("os=macos"))
            .unwrap();
        assert_eq!(skipped.status, "skipped");
        assert!(skipped
            .if_condition
            .as_deref()
            .unwrap()
            .contains("matrix.os"));

        rg_db::ops::pipeline_ops::update_job_result(
            &db,
            runnable.id,
            "assigned",
            None,
            None,
            Some(chrono::Utc::now().naive_utc()),
            None,
        )
        .await
        .unwrap();
        assert!(
            rg_db::ops::pipeline_ops::find_pending_job_matching_labels(&db, repo.id, &[])
                .await
                .unwrap()
                .is_none()
        );
        rg_db::ops::pipeline_ops::update_job_result(
            &db,
            runnable.id,
            "failed",
            Some(1),
            None,
            None,
            Some(chrono::Utc::now().naive_utc()),
        )
        .await
        .unwrap();
        assert_eq!(
            rg_db::ops::pipeline_ops::try_update_stage(&db, runnable.stage_id)
                .await
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::try_update_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .into_iter()
                .find(|job| job.name == "deploy")
                .unwrap()
                .status,
            "skipped"
        );

        let all_skipped_pipeline_id = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/dev",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .unwrap();
        let all_skipped_pipeline =
            rg_db::ops::pipeline_ops::get_pipeline(&db, all_skipped_pipeline_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(all_skipped_pipeline.status, "success");
        assert!(
            rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, all_skipped_pipeline_id)
                .await
                .unwrap()
                .iter()
                .all(|job| job.status == "skipped")
        );
    }

    /// A pipeline that fails half-way through being written leaves nothing
    /// behind — and the rows it had already written were never schedulable in
    /// the first place.
    ///
    /// Both halves matter. Without the first, the pipeline row survives with a
    /// graph that is a subset of the declared one, the runners finish that
    /// subset and the status cascade calls the pipeline `success`. Without the
    /// second, a runner polling between two `create_job` calls picks up a job of
    /// a pipeline that is about to be rolled back.
    #[tokio::test]
    async fn a_failed_pipeline_build_leaves_nothing_a_runner_can_pick_up() {
        use sea_orm::{EntityTrait, PaginatorTrait};

        let temp = tempfile::tempdir().unwrap();
        // `first` is a perfectly good job. `second` fails in `expand_matrix`,
        // which runs *while jobs are being written* — an empty matrix dimension
        // is not part of `validate_execution_semantics`, so the failure lands
        // after the pipeline row and both stages already exist.
        std::fs::write(
            temp.path().join(".plombir-git-ci.yml"),
            "stages: [build, test]\nfirst:\n  stage: build\n  script: [echo one]\nsecond:\n  stage: test\n  script: [echo two]\n  matrix:\n    arch: []\n",
        )
        .unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(temp.path())).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(temp.path())
            )
            .unwrap()
            .success());
        assert!(git
            .run(&["add", ".plombir-git-ci.yml"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "half-built"], Some(temp.path()))
            .unwrap()
            .success());
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_string();
        // Two pooled connections on purpose: the second half of this test reads
        // through the pool while a transaction holds the first, which is exactly
        // the shape of a runner polling during a pipeline build.
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("half-built.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            2,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "half-built-owner",
            "half-built@example.com",
            "unused",
            "Half Built Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("half-built".into()),
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
        .unwrap();

        let error = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .unwrap_err();
        // The original failure reaches the caller — the rollback does not
        // rewrite it into a generic database error.
        let reported = format!("{error:#}");
        assert!(
            reported.contains("empty matrix dimension"),
            "the config error should survive the rollback, got: {reported}"
        );

        // Nothing survived the failure: no pipeline to show green, no stage, no
        // job for a runner to take.
        assert!(
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap()
                .0
                .is_empty(),
            "a pipeline that could not be built must not stay in the database"
        );
        assert_eq!(
            rg_db::entities::pipeline_stage::Entity::find()
                .count(&db)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            rg_db::entities::pipeline_job::Entity::find()
                .count(&db)
                .await
                .unwrap(),
            0
        );
        assert!(
            rg_db::ops::pipeline_ops::find_pending_job_matching_labels(&db, repo.id, &[])
                .await
                .unwrap()
                .is_none()
        );

        // The other half: a graph that is still being written is invisible to
        // the query the runners poll with, so there is no window in which an
        // incomplete pipeline can be scheduled.
        let tx = db.begin().await.unwrap();
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &tx,
            repo.id,
            &sha,
            "refs/heads/main",
            "push",
            Some(user.id),
        )
        .await
        .unwrap();
        let stage = rg_db::ops::pipeline_ops::create_stage(&tx, pipeline.id, "build", 0)
            .await
            .unwrap();
        rg_db::ops::pipeline_ops::create_job(
            &tx, stage.id, "first", "echo one", None, None, None, None, None, None, false, None,
            None, None,
        )
        .await
        .unwrap();
        assert!(
            rg_db::ops::pipeline_ops::find_pending_job_matching_labels(&db, repo.id, &[])
                .await
                .unwrap()
                .is_none(),
            "an uncommitted job must not be schedulable"
        );
        tx.rollback().await.unwrap();
        assert_eq!(
            rg_db::entities::pipeline_job::Entity::find()
                .count(&db)
                .await
                .unwrap(),
            0
        );
    }

    async fn concurrency_fixture(
        config: &'static [u8],
        database_name: &str,
    ) -> (
        tempfile::TempDir,
        String,
        rg_db::DatabaseConnection,
        rg_db::entities::user::Model,
        rg_db::entities::repository::Model,
    ) {
        trigger_fixture(&[(".plombir-git-ci.yml", config)], database_name).await
    }

    /// [`concurrency_fixture`] for any set of committed files.
    async fn trigger_fixture(
        files: &[(&str, &[u8])],
        database_name: &str,
    ) -> (
        tempfile::TempDir,
        String,
        rg_db::DatabaseConnection,
        rg_db::entities::user::Model,
        rg_db::entities::repository::Model,
    ) {
        let (temp, sha) = commit_repo(files);
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join(database_name).display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "atomic-concurrency-owner",
            "atomic-concurrency@example.com",
            "unused",
            "Atomic Concurrency Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("atomic-concurrency".into()),
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
        .unwrap();
        (temp, sha, db, user, repo)
    }

    fn concurrency_trigger<'a>(
        db: &'a rg_db::DatabaseConnection,
        repo_path: &'a std::path::Path,
        repo_id: i64,
        sha: &'a str,
        user_id: i64,
        trigger_type: &'a str,
    ) -> TriggerPipelineParams<'a> {
        TriggerPipelineParams {
            db,
            repo_root: repo_path,
            repo_path,
            repo_id,
            commit_sha: sha,
            ref_name: "refs/heads/main",
            trigger_type,
            base_branch: None,
            head_branch: None,
            previous_sha: None,
            inputs: None,
            triggered_by: Some(user_id),
            docker_enabled: false,
            external_runners: true,
            allow_host_runner: false,
            jwt_secret: Some("secret"),
            encryption_key: Some("secret"),
            external_url: None,
            schedule: None,
        }
    }

    /// card_d92cd3260864, on the production path: `.plombir-git-ci.yml` without a
    /// `stages:` key parses, and every one of its jobs resolved to a stage the
    /// graph never created. `create_jobs` dropped them one `warn!` at a time,
    /// the pipeline was published holding nothing, and `run_pipeline` walked its
    /// stage list in zero iterations and settled it `success` — a green commit
    /// status, on a branch-protection gate, for a run that executed no command.
    ///
    /// This is the mutation anchor for the fix: take the `default` synthesis out
    /// of `resolved_stage_order` and the trigger below fails instead, because
    /// the validator then finds a stage nothing declares.
    #[tokio::test]
    async fn a_config_that_declares_no_stages_still_publishes_its_jobs() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"build:\n  script: [echo one]\ncheck:\n  script: [echo two]\n",
            "implicit-default-stage.db",
        )
        .await;

        let pipeline_id = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect("a file that lists no stages is still a file this engine can run");

        let stages = rg_db::ops::pipeline_ops::list_stages_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(
            stages
                .iter()
                .map(|stage| stage.name.as_str())
                .collect::<Vec<_>>(),
            vec!["default"],
            "the stage the docs promise a stageless job has to exist for it to run in"
        );

        let mut jobs = rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, stages[0].id)
            .await
            .unwrap()
            .into_iter()
            .map(|job| job.name)
            .collect::<Vec<_>>();
        jobs.sort();
        assert_eq!(
            jobs,
            vec!["build".to_string(), "check".to_string()],
            "every job the author committed must reach the pipeline, or the run proves nothing"
        );
    }

    /// card_1d3af3ad4f55: a repository whose one workflow is `on: pull_request`
    /// has nothing to run on a push. The push producer used to publish a red
    /// "CI configuration rejected" pipeline on the head of every pull request
    /// branch it saw pushed — after the pull request's own run, so the merge
    /// gate judged the red one. Now the engine answers "no work", which the
    /// producer publishes nothing for, and the same file still runs for the
    /// pull request.
    #[tokio::test]
    async fn a_push_no_workflow_is_triggered_by_publishes_nothing() {
        let (temp, sha, db, user, repo) = trigger_fixture(
            &[(
                ".gitea/workflows/ci.yml",
                b"on: pull_request\njobs:\n  test:\n    steps:\n      - run: echo checked\n"
                    as &[u8],
            )],
            "push-untriggered.db",
        )
        .await;

        let error = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect_err("no workflow is triggered by push");
        assert!(
            error
                .downcast_ref::<rg_core::ci::NoMatchingCiJobs>()
                .is_some(),
            "the producer must be able to tell \"nothing to run\" from a broken file: {error:#}"
        );
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_none(),
            "an InvalidRequest is what the producer publishes as a red pipeline: {error:#}"
        );
        let (_, total) =
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 10)
                .await
                .unwrap();
        assert_eq!(total, 0, "a push with nothing to run published a pipeline");

        let mut pull_request = concurrency_trigger(
            &db,
            temp.path(),
            repo.id,
            &sha,
            user.id,
            rg_core::pull_request::PULL_REQUEST_EVENT,
        );
        pull_request.ref_name = "refs/pull/1/head";
        pull_request.base_branch = Some("main");
        trigger_pipeline_with_barrier(pull_request, &CiNotifications::default(), None)
            .await
            .expect("the same workflow runs for the pull request");
    }

    /// card_6860d4a03e16: `only:` used to run inside `create_jobs`, after the
    /// pipeline and all stages had already been inserted. If every job was
    /// filtered out, the empty stage roll-up and the embedded runner both used
    /// vacuous truth to publish `success`. Worse, a fixed concurrency group
    /// could cancel a real run before publishing that green empty replacement.
    ///
    /// Mutation anchor: moving the selector back into `create_jobs` makes the
    /// feature trigger cancel `main` and leaves a second, jobless pipeline.
    #[tokio::test]
    async fn only_selecting_no_jobs_publishes_nothing_and_cancels_nothing() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"concurrency:\n  group: deploy\n  cancel_in_progress: true\nstages: [test]\nbuild:\n  stage: test\n  only: [main]\n  script: [echo checked]\n",
            "only-selects-nothing.db",
        )
        .await;

        let main_pipeline = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect("main is selected by `only:`");

        let mut feature = concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push");
        feature.ref_name = "refs/heads/feature";
        let error = trigger_pipeline_with_barrier(feature, &CiNotifications::default(), None)
            .await
            .expect_err("a ref excluded by every job must not publish a pipeline");
        let no_match = error
            .downcast_ref::<rg_core::ci::NoMatchingCiJobs>()
            .unwrap_or_else(|| {
                panic!("the producer must be able to distinguish no-match: {error:#}")
            });
        assert_eq!(no_match.ref_name, "refs/heads/feature");

        let (pipelines, total) =
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap();
        assert_eq!(total, 1, "the excluded ref published a second pipeline");
        assert_eq!(pipelines[0].id, main_pipeline);
        assert_eq!(
            pipelines[0].status, "pending",
            "selection must happen before concurrency cancellation"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, main_pipeline)
                .await
                .unwrap()
                .len(),
            1,
            "the only persisted graph is the real main run"
        );
    }

    /// The other half of card_d92cd3260864: a stage name the file never listed
    /// cost the job its run and nothing else — the pipeline went green having
    /// skipped it. The trigger now stops before the first row is written, so the
    /// author gets the refusal instead of a pipeline missing a job.
    #[tokio::test]
    async fn a_job_naming_an_undeclared_stage_stops_the_trigger_instead_of_the_job() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"stages: [build]\ndeploy:\n  stage: biuld\n  script: [echo one]\n",
            "undeclared-stage.db",
        )
        .await;

        let error = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect_err("a job the engine cannot place must not produce a runnable pipeline");
        let message = format!("{error:#}");
        assert!(
            message.contains("deploy") && message.contains("biuld") && message.contains("build"),
            "the author has to be told the job, the stage it asked for and the stages they \
             declared: {message}"
        );

        assert!(
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap()
                .0
                .is_empty(),
            "the refusal must land before the pipeline row, not after it"
        );
    }

    /// card_857c0d4ab7a6: `image:` used to reach `docker run` as written, where
    /// `--privileged` is a flag. The trigger refuses it before the pipeline row
    /// exists, so the author sees the rule instead of a failed job.
    #[tokio::test]
    async fn an_image_that_is_not_a_reference_stops_the_trigger() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"build:\n  image: '--privileged'\n  script: [echo one]\n",
            "flag-image.db",
        )
        .await;

        let error = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect_err("an image the executors would refuse must not publish a pipeline");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "the refusal is the author's to fix: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(
            message.contains("build") && message.contains("'--privileged'"),
            "the refusal must name the job and the value: {message}"
        );
        assert!(
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap()
                .0
                .is_empty()
        );
    }

    /// card_a79579f79c29: a hyphenated matrix key and a variable the runner
    /// sets itself (`CI`) are ordinary in the wild and must keep triggering.
    /// The matrix variable is the derived `MATRIX_NODE_VERSION`; the raw key is
    /// not a name a process can carry and is not stored as one.
    #[tokio::test]
    async fn a_hyphenated_matrix_key_and_a_reserved_variable_still_trigger() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"build:\n  script: [echo one]\n  variables:\n    CI: 'true'\n    LANG: C.UTF-8\n  matrix:\n    node-version: ['18', '20']\n",
            "hyphen-matrix.db",
        )
        .await;

        let pipeline_id = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect("a hyphenated matrix key and `CI: true` must not refuse the pipeline");

        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        let mut versions: Vec<String> = jobs
            .iter()
            .map(|job| {
                let variables: std::collections::BTreeMap<String, String> =
                    serde_json::from_str(job.variables.as_deref().expect("job variables")).unwrap();
                assert!(
                    !variables.contains_key("node-version"),
                    "the raw key is not a variable name: {variables:?}"
                );
                assert_eq!(variables.get("CI").map(String::as_str), Some("true"));
                variables["MATRIX_NODE_VERSION"].clone()
            })
            .collect();
        versions.sort();
        assert_eq!(versions, ["18", "20"]);
    }

    /// The 400 is kept for a name no process can carry.
    #[tokio::test]
    async fn a_variable_name_no_process_can_carry_stops_the_trigger() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"build:\n  script: [echo one]\n  variables:\n    BAD-NAME: x\n",
            "bad-variable-name.db",
        )
        .await;

        let error = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect_err("a variable name a process cannot carry must be refused");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "{error:#}"
        );
        assert!(format!("{error:#}").contains("'BAD-NAME'"), "{error:#}");
    }

    async fn seed_environment(db: &rg_db::DatabaseConnection, repo_id: i64, name: &str) {
        rg_db::ops::ci_environment_ops::create_with_approvers(
            db,
            rg_db::entities::ci_environment::ActiveModel {
                id: NotSet,
                repo_id: Set(repo_id),
                name: Set(name.to_string()),
                protected: Set(true),
                required_approvals: Set(1),
                allowed_approver_ids: Set(None),
                created_at: Set(chrono::Utc::now()),
                updated_at: Set(chrono::Utc::now()),
            },
            Vec::new(),
        )
        .await
        .expect("the repository's protected environment is created");
    }

    /// card_adb623830190: the approval gate is read off the environment row, so
    /// a name with no row was read as no gate. `environment: producton` did not
    /// fail the deploy its author had gated behind the protected `production` —
    /// it released it, straight to a runner, with `environment_id` NULL and no
    /// signal anywhere: the environment is never auto-created, so the typo did
    /// not even appear in the repository's environment list.
    ///
    /// This is the mutation anchor for the fix: let `resolve_job_environment`
    /// answer `Ok(None)`-style again — drop the refusal and pass
    /// `environment.as_ref()` to `attach_job` — and this test goes green on a
    /// pipeline that deploys without approval.
    #[tokio::test]
    async fn a_job_naming_an_unknown_environment_stops_the_trigger_instead_of_deploying() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"stages: [deploy]\nrelease:\n  stage: deploy\n  environment: producton\n  script: [echo one]\n",
            "unknown-environment.db",
        )
        .await;
        seed_environment(&db, repo.id, "production").await;

        let error = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect_err("a job whose environment this repository has no row for must not be queued");
        let message = format!("{error:#}");
        assert!(
            message.contains("release")
                && message.contains("producton")
                && message.contains("production"),
            "the author has to be told the job, the environment it asked for and the ones the \
             repository has: {message}"
        );

        assert!(
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap()
                .0
                .is_empty(),
            "the refusal must land before the pipeline row, not after it"
        );
    }

    /// The other half of card_adb623830190: the refusal above must cost the
    /// working case nothing. A job naming an environment that *does* exist and
    /// *is* protected still reaches the queue gated, never `pending`.
    #[tokio::test]
    async fn a_job_naming_a_protected_environment_still_waits_for_approval() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"stages: [deploy]\nrelease:\n  stage: deploy\n  environment: production\n  script: [echo one]\n",
            "protected-environment.db",
        )
        .await;
        seed_environment(&db, repo.id, "production").await;

        let pipeline_id = trigger_pipeline_with_barrier(
            concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push"),
            &CiNotifications::default(),
            None,
        )
        .await
        .expect("an environment the repository declares is a pipeline this engine can build");

        let stages = rg_db::ops::pipeline_ops::list_stages_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, stages[0].id)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 1, "the file declares exactly one job");
        assert_eq!(
            jobs[0].status, "waiting_approval",
            "a protected environment gates its job; anything else is the deploy running unapproved"
        );
        assert!(
            jobs[0].environment_id.is_some(),
            "the gate is read back off this id — a job carrying only the name cannot be approved"
        );
    }

    /// card_61e4278d15e3: two producers rendezvous at the old empty-group
    /// check. The database lock admits one complete graph and makes the other
    /// producer observe it as a typed conflict; no loser rows are ever written.
    #[tokio::test]
    async fn concurrent_free_group_without_cancellation_publishes_one_complete_graph() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"concurrency:\n  group: deploy-production\nstages: [build]\nbuild:\n  stage: build\n  script: [echo one]\n",
            "atomic-refusal.db",
        )
        .await;
        let barrier = tokio::sync::Barrier::new(2);
        let notifications = CiNotifications::default();

        let (first, second) = tokio::join!(
            trigger_pipeline_with_barrier(
                concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push",),
                &notifications,
                Some(&barrier),
            ),
            trigger_pipeline_with_barrier(
                concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "manual",),
                &notifications,
                Some(&barrier),
            )
        );

        let outcomes = [first, second];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        let loser = outcomes
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one producer must lose the group");
        assert!(
            loser.downcast_ref::<rg_core::error::Conflict>().is_some(),
            "the loser must receive the same typed 409 as any busy group: {loser:#}"
        );

        let pipelines =
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap()
                .0;
        assert_eq!(pipelines.len(), 1, "the loser must leave no pipeline row");
        let stages = rg_db::ops::pipeline_ops::list_stages_by_pipeline(&db, pipelines[0].id)
            .await
            .unwrap();
        assert_eq!(
            stages.len(),
            1,
            "the winner publishes its complete stage set"
        );
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, stages[0].id)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 1, "the winner publishes its complete job set");
        assert_eq!(
            rg_db::ops::pipeline_ops::find_active_pipelines_by_group(
                &db,
                repo.id,
                "deploy-production",
            )
            .await
            .unwrap()
            .len(),
            1
        );
    }

    /// With `cancel_in_progress`, both triggers are valid: the second admitted
    /// producer replaces the first under the same transaction lock. Its commit
    /// leaves one active graph and a fully canceled predecessor, never two
    /// active roots that merely happen to be complete.
    #[tokio::test]
    async fn concurrent_free_group_with_cancellation_replaces_the_first_complete_graph() {
        let (temp, sha, db, user, repo) = concurrency_fixture(
            b"concurrency:\n  group: deploy-production\n  cancel_in_progress: true\nstages: [build]\nbuild:\n  stage: build\n  script: [echo one]\n",
            "atomic-replacement.db",
        )
        .await;
        let barrier = tokio::sync::Barrier::new(2);
        let notifications = CiNotifications::default();

        let (first, second) = tokio::join!(
            trigger_pipeline_with_barrier(
                concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "push",),
                &notifications,
                Some(&barrier),
            ),
            trigger_pipeline_with_barrier(
                concurrency_trigger(&db, temp.path(), repo.id, &sha, user.id, "manual",),
                &notifications,
                Some(&barrier),
            )
        );
        let first_id = first.expect("first producer finishes");
        let second_id = second.expect("second producer finishes");
        assert_ne!(first_id, second_id);

        let pipelines =
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap()
                .0;
        assert_eq!(pipelines.len(), 2);
        let active = pipelines
            .iter()
            .filter(|pipeline| pipeline.status != "canceled")
            .collect::<Vec<_>>();
        let canceled = pipelines
            .iter()
            .filter(|pipeline| pipeline.status == "canceled")
            .collect::<Vec<_>>();
        assert_eq!(active.len(), 1, "one graph owns the group after drain");
        assert_eq!(
            canceled.len(),
            1,
            "the displaced graph is retained as canceled"
        );

        let canceled_stages =
            rg_db::ops::pipeline_ops::list_stages_by_pipeline(&db, canceled[0].id)
                .await
                .unwrap();
        assert_eq!(canceled_stages.len(), 1);
        assert_eq!(canceled_stages[0].status, "canceled");
        let canceled_jobs =
            rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, canceled_stages[0].id)
                .await
                .unwrap();
        assert_eq!(canceled_jobs.len(), 1);
        assert_eq!(canceled_jobs[0].status, "canceled");
        assert_eq!(
            rg_db::ops::pipeline_ops::find_active_pipelines_by_group(
                &db,
                repo.id,
                "deploy-production",
            )
            .await
            .unwrap()
            .len(),
            1
        );
    }

    /// `cancel_in_progress: true` promises the concurrency group runs one
    /// pipeline at a time. A cancellation write the database refuses has to
    /// stop the trigger — otherwise the promise buys the opposite of what it
    /// says: the old chain keeps running and a second one joins it.
    #[tokio::test]
    async fn a_refused_concurrency_cancellation_does_not_start_a_replacement_pipeline() {
        use sea_orm::ConnectionTrait;

        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"concurrency:\n  group: ${{ ref }}\n  cancel_in_progress: true\nbuild:\n  script: [echo one]\n"
                as &[u8],
        )]);
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("concurrency.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "concurrency-owner",
            "concurrency@example.com",
            "unused",
            "Concurrency Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("concurrency".into()),
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
        .unwrap();

        // The in-progress pipeline the next push is supposed to replace. It has
        // to carry the group the config resolves to (`${{ ref }}`), because the
        // group — not the ref — is what the trigger looks up.
        let in_progress = rg_db::ops::pipeline_ops::create_pipeline_row(
            &db,
            rg_db::ops::pipeline_ops::NewPipeline {
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                triggered_by: Some(user.id),
                concurrency_group: Some("refs/heads/main"),
                dispatch_inputs: None,
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                schedule: None,
            },
        )
        .await
        .unwrap();

        // Scalpel fault injection: ONLY the cancellation write is refused.
        // Inserting a pipeline with its stages and jobs still works, so nothing
        // but the fix itself stands between this trigger and a second active
        // chain — dropping a table would have failed the creation too and the
        // test would pass for the wrong reason.
        db.execute_unprepared(
            "CREATE TRIGGER refuse_cancellation BEFORE UPDATE ON pipelines \
             FOR EACH ROW WHEN NEW.status = 'canceled' \
             BEGIN SELECT RAISE(ABORT, 'injected cancellation failure'); END;",
        )
        .await
        .expect("install the cancellation fault");

        let error = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .unwrap_err();

        // The caller learns both what was refused and what did not happen
        // because of it — the reason no longer lives only in the log.
        let reported = format!("{error:#}");
        assert!(
            reported.contains("the replacement pipeline was not started"),
            "the caller must learn the trigger stopped on the cancellation: {reported}"
        );
        assert!(
            reported.contains("injected cancellation failure"),
            "the database reason must survive to the caller: {reported}"
        );

        // The count is the whole point: a failed cancellation must not grow the
        // number of active pipelines on the ref.
        let (pipelines, _) =
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap();
        assert_eq!(
            pipelines.len(),
            1,
            "a refused cancellation must not be followed by a replacement pipeline"
        );
        assert_eq!(pipelines[0].id, in_progress.id);
        assert_eq!(
            pipelines[0].status, "pending",
            "the pipeline that could not be canceled stays exactly as it was"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(&db, repo.id, "refs/heads/main")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// The other half of the same branch: with `cancel_in_progress: false` a
    /// busy group is a refusal the caller can act on, and it must not answer
    /// with the code a crashed server uses. The test keeps both halves apart —
    /// a busy group is a `Conflict` (409), a storage failure on the very same
    /// call path is not — because that split is the whole point: a 500 tells a
    /// retrying client "this will pass on its own", and a busy group only
    /// passes when the pipeline ahead of it finishes.
    #[tokio::test]
    async fn a_busy_concurrency_group_is_a_conflict_not_a_server_failure() {
        use sea_orm::ConnectionTrait;

        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"concurrency:\n  group: ${{ ref }}\nbuild:\n  script: [echo one]\n" as &[u8],
        )]);
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("busy-group.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "busy-owner",
            "busy@example.com",
            "unused",
            "Busy Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("busy".into()),
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
        .unwrap();

        // The pipeline that holds the group — same resolved name the config
        // under test declares.
        let in_progress = rg_db::ops::pipeline_ops::create_pipeline_row(
            &db,
            rg_db::ops::pipeline_ops::NewPipeline {
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                triggered_by: Some(user.id),
                concurrency_group: Some("refs/heads/main"),
                dispatch_inputs: None,
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                schedule: None,
            },
        )
        .await
        .unwrap();

        let error = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "manual",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .unwrap_err();

        let conflict = error
            .downcast_ref::<rg_core::error::Conflict>()
            .unwrap_or_else(|| {
                panic!("a busy concurrency group must be a Conflict (409), got: {error:#}")
            });
        // `Conflict`'s message is the one the HTTP layer renders verbatim, so
        // the instruction has to live *in it* — not in a `.context(...)` layer
        // the sanitizer drops.
        let message = conflict.to_string();
        assert!(
            message.contains("has 1 active pipeline"),
            "the refusal must say what is holding the group: {message}"
        );
        assert!(
            message.contains("cancel_in_progress: true"),
            "the way out has to reach the client, not just the log: {message}"
        );

        // The refusal is a refusal: nothing was started.
        assert_eq!(
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap()
                .0
                .len(),
            1
        );

        // Second half of the split. A ref with no active pipeline clears the
        // concurrency check, and only the pipeline write fails — that is a
        // server-side failure and must NOT be dressed up as a busy group, or
        // the 409 stops meaning "wait for the one ahead of you".
        db.execute_unprepared(
            "CREATE TRIGGER refuse_pipeline_insert BEFORE INSERT ON pipelines \
             FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;",
        )
        .await
        .expect("install the storage fault");

        let storage_error = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/other",
                trigger_type: "manual",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .unwrap_err();
        assert!(
            storage_error
                .downcast_ref::<rg_core::error::Conflict>()
                .is_none(),
            "a refused write is ours, not the caller's: {storage_error:#}"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo.id, 0, 100)
                .await
                .unwrap()
                .0
                .len(),
            1,
            "the failed write left nothing behind"
        );
        assert_eq!(in_progress.status, "pending");
    }

    /// card_4c5214698ae9: the group is what serializes, and only the group.
    ///
    /// `concurrency.group` was resolved, logged and thrown away — the search for
    /// "what has to finish first" ran on `ref_name`. That is wrong in both
    /// directions at once, so this drives both:
    ///
    /// * a fixed group shared by two branches has to serialize *across* them —
    ///   writing `group: deploy-production` instead of `${{ ref }}` is precisely
    ///   the request not to deploy two branches at the same time;
    /// * a pipeline belonging to a *different* group on the same branch is
    ///   somebody else's work and must survive untouched, even under
    ///   `cancel_in_progress: true`.
    #[tokio::test]
    async fn concurrency_serializes_by_group_and_leaves_other_groups_alone() {
        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"concurrency:\n  group: deploy-production\n  cancel_in_progress: true\nbuild:\n  script: [echo one]\n"
                as &[u8],
        )]);
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("group.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "group-owner",
            "group@example.com",
            "unused",
            "Group Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("group".into()),
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
        .unwrap();

        let trigger = |ref_name: &'static str| {
            let db = db.clone();
            let path = temp.path().to_path_buf();
            let sha = sha.clone();
            let repo_id = repo.id;
            let user_id = user.id;
            async move {
                trigger_pipeline(
                    TriggerPipelineParams {
                        db: &db,
                        repo_root: &path,
                        repo_path: &path,
                        repo_id,
                        commit_sha: &sha,
                        ref_name,
                        trigger_type: "push",
                        base_branch: None,
                        head_branch: None,
                        previous_sha: None,
                        inputs: None,
                        triggered_by: Some(user_id),
                        docker_enabled: false,
                        external_runners: true,
                        allow_host_runner: false,
                        jwt_secret: Some("secret"),
                        encryption_key: Some("secret"),
                        external_url: None,
                        schedule: None,
                    },
                    &CiNotifications::default(),
                )
                .await
            }
        };

        let seed = |ref_name: &'static str, group: &'static str| {
            let db = db.clone();
            let sha = sha.clone();
            let repo_id = repo.id;
            let user_id = user.id;
            async move {
                rg_db::ops::pipeline_ops::create_pipeline_row(
                    &db,
                    rg_db::ops::pipeline_ops::NewPipeline {
                        repo_id,
                        commit_sha: &sha,
                        ref_name,
                        trigger_type: "push",
                        triggered_by: Some(user_id),
                        concurrency_group: Some(group),
                        dispatch_inputs: None,
                        base_branch: None,
                        head_branch: None,
                        previous_sha: None,
                        schedule: None,
                    },
                )
                .await
                .unwrap()
            }
        };
        let status = |id: i64| {
            let db = db.clone();
            async move {
                rg_db::ops::pipeline_ops::get_pipeline(&db, id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status
            }
        };

        // A deploy already running on `main`, and somebody else's nightly audit
        // on the very same branch under its own group.
        let deploy_on_main = seed("refs/heads/main", "deploy-production").await;
        let stranger = seed("refs/heads/main", "nightly-audit").await;

        // The push lands on a *different* branch, and the group is a fixed name
        // both branches declare. Matching on the ref found nothing here, so two
        // deploys of the same environment ran side by side.
        let on_release = trigger("refs/heads/release")
            .await
            .expect("a cancelling group lets the replacement start");
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, on_release)
                .await
                .unwrap()
                .unwrap()
                .concurrency_group
                .as_deref(),
            Some("deploy-production"),
            "the resolved group has to be recorded, or the next trigger cannot find it"
        );
        assert_eq!(
            status(deploy_on_main.id).await,
            "canceled",
            "a fixed group did not serialize across branches — the ref, not the group, was matched"
        );
        assert_eq!(
            status(stranger.id).await,
            "pending",
            "another group's pipeline was cancelled by a trigger that never named it"
        );

        // The other direction, on one branch: a push to `main` cancels the
        // deploy it replaces and still leaves the nightly audit alone, even
        // though all three share the ref.
        let deploy_again = seed("refs/heads/main", "deploy-production").await;
        trigger("refs/heads/main").await.expect("push builds");
        assert_eq!(
            status(deploy_again.id).await,
            "canceled",
            "the pipeline of this very group survived a cancelling trigger"
        );
        assert_eq!(
            status(stranger.id).await,
            "pending",
            "a same-branch pipeline of another group was cancelled"
        );
    }

    #[test]
    fn repository_reader_expands_local_reusable_workflow_at_commit() {
        let temp = tempfile::tempdir().unwrap();
        let workflows = temp.path().join(".gitea/workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("main.yml"),
            "on: push\njobs:\n  shared:\n    uses: ./.gitea/workflows/shared.yml\n    with:\n      target: staging\n",
        ).unwrap();
        std::fs::write(
            workflows.join("shared.yml"),
            "on:\n  workflow_call:\n    inputs:\n      target:\n        required: true\n        type: string\n      attempts:\n        type: number\n        default: 2\njobs:\n  build:\n    steps:\n      - run: echo '${{ inputs.target }} ${{ inputs.attempts }}'\n",
        ).unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(temp.path())).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(temp.path())
            )
            .unwrap()
            .success());
        assert!(git
            .run(&["add", ".gitea/workflows"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "workflows"], Some(temp.path()))
            .unwrap()
            .success());
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        let config =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap();
        let job = config.jobs.get("main/shared/build").unwrap();
        assert!(job
            .script
            .iter()
            .any(|line| line.contains("${INPUT_TARGET}")));
        assert_eq!(job.variables.as_ref().unwrap()["INPUT_TARGET"], "staging");
        assert_eq!(job.variables.as_ref().unwrap()["INPUT_ATTEMPTS"], "2");
    }

    /// Three lists have to agree about `github.*`: the validator that decides
    /// whether a condition may be written at all, and the two context builders
    /// that decide what it means — one for step conditions at conversion time,
    /// one for job conditions when the graph is written. A name the validator
    /// accepts and a builder does not answer evaluates to the empty string, and
    /// `github.repository == 'acme/api'` then reports a confident `false`
    /// (card_054e997a46e6, which was that state for two of the six).
    ///
    /// Asserted in both directions, because each catches a different mistake:
    /// forgetting to answer a canonical name, and answering a name the validator
    /// would have rejected.
    ///
    /// Two runs are asked, a scheduled one and a pull request, because some
    /// keys have a value only for one event — `github.event.schedule` for the
    /// first, `github.head_ref` / `github.base_ref` for the second — and every
    /// key has to be answered with a value by one of them.
    #[test]
    fn every_condition_key_the_validator_accepts_is_answered_by_both_contexts() {
        let contexts = |event: &str, schedule: Option<&str>, head: &str, base: &str| {
            let job = job_condition_context(
                &JobRowContext {
                    repo_id: 1,
                    repository: RepositoryName {
                        owner: "acme",
                        name: "api",
                    },
                    commit_sha: "abc123",
                    ref_name: "refs/heads/main",
                    trigger_type: event,
                    schedule_cron: schedule,
                    head_ref: head,
                    base_ref: base,
                },
                &std::collections::BTreeMap::new(),
                &std::collections::BTreeMap::new(),
            );
            let step = gitea_actions::actions_condition_context(
                &gitea_actions::WorkflowContext {
                    ref_name: "refs/heads/main".into(),
                    sha: "abc123".into(),
                    event: event.into(),
                    repo_owner: "acme".into(),
                    repo_name: "api".into(),
                    event_schedule: schedule.map(str::to_string),
                    head_ref: head.into(),
                    base_ref: base.into(),
                },
                &std::collections::HashMap::new(),
            );
            [("job", job), ("step", step)]
        };
        let runs = [
            contexts("schedule", Some("0 3 * * *"), "", ""),
            contexts("pull_request", None, "feature", "main"),
        ];

        for key in condition::GITHUB_CONTEXT_KEYS {
            for scope in ["job", "step"] {
                let answers = runs
                    .iter()
                    .flatten()
                    .filter(|(label, _)| *label == scope)
                    .map(|(_, context)| {
                        context.get(key).unwrap_or_else(|| {
                            panic!(
                                "the validator accepts `{key}` in an `if:` condition, but the \
                                 {scope} context has no answer for it — the condition would \
                                 compare against the empty string and report a plain false"
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                assert!(
                    answers.iter().any(|value| !value.is_empty()),
                    "the {scope} context answers `{key}` with the empty string for every event, \
                     which is what a `// filled later` placeholder looks like from a condition"
                );
            }
        }

        for (label, context) in runs.iter().flatten() {
            for key in context.keys().filter(|key| key.starts_with("github.")) {
                assert!(
                    condition::GITHUB_CONTEXT_KEYS.contains(&key.as_str()),
                    "the {label} context answers `{key}`, which the validator rejects — a \
                     condition using it is refused as unsupported, so the answer is unreachable"
                );
            }
        }
    }

    /// A job condition may name the repository the pipeline belongs to, and the
    /// name it gets is the one in the database.
    ///
    /// `WorkflowContext.repo_owner` / `repo_name` were declared, filled with
    /// `String::new() // filled later` by the only producer, and read by nobody
    /// — so `github.repository` was the empty string and any comparison against
    /// it was a plain `false`, indistinguishable from a condition that correctly
    /// did not match (card_054e997a46e6). Two properties are asserted together
    /// because either alone can pass on an accident:
    ///
    ///  * the job whose condition names `<owner>/<name>` runs, and the job that
    ///    names another repository is skipped — an empty identity fails the first;
    ///  * the identity comes from the repository *row*, not from the directory:
    ///    the fixture's working tree lives in a random temp directory whose name
    ///    matches neither, so a path-derived answer fails the same assertion.
    #[tokio::test]
    async fn a_job_condition_can_name_the_repository_the_pipeline_belongs_to() {
        // An Actions workflow, because the identity has to arrive at *both*
        // hops: the step conditions are resolved while the workflow is converted
        // (`WorkflowContext`), the job conditions when the graph is written
        // (`job_condition_context`). Filling one and leaving the other empty is
        // exactly the state this card found.
        let (repo_dir, sha) = commit_repo(&[(
            ".gitea/workflows/ci.yml",
            b"name: CI\non: push\njobs:\n  mine:\n    runs-on: ubuntu-latest\n    if: github.repository == 'acme/api'\n    steps:\n      - if: github.repository_owner == 'acme'\n        run: echo my-owner\n      - if: github.repository == 'someone/else'\n        run: echo not-mine\n  theirs:\n    runs-on: ubuntu-latest\n    if: github.repository == 'someone/else'\n    steps:\n      - run: echo nope\n" as &[u8],
        )]);
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                repo_dir.path().join("identity.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let org_owner = rg_db::ops::user_ops::create_user(
            &db,
            "acme-admin",
            "admin@example.com",
            "unused",
            "Acme Admin",
        )
        .await
        .unwrap();
        // An organization repository is the case a path-or-column shortcut gets
        // wrong: the row's `owner_id` is the organization's *owner*, while the
        // namespace is the organization's name.
        let org = rg_db::ops::org_ops::create_org(&db, "acme", None, None, org_owner.id, "public")
            .await
            .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(org_owner.id),
                name: Set("api".into()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".into()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(Some(org.id)),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .unwrap();

        let pipeline_id = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: repo_dir.path(),
                repo_path: repo_dir.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(org_owner.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .unwrap();

        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        let mine = jobs.iter().find(|job| job.name == "ci/mine").unwrap();
        assert_eq!(
            mine.status, "pending",
            "`github.repository == 'acme/api'` did not match the repository the pipeline is for"
        );
        let theirs = jobs.iter().find(|job| job.name == "ci/theirs").unwrap();
        assert_eq!(
            theirs.status, "skipped",
            "a condition naming another repository must not run here"
        );
        // The step half: kept when it names this repository's owner, dropped
        // when it names another repository.
        assert!(
            mine.script.contains("echo my-owner"),
            "the step condition naming this owner was dropped: {}",
            mine.script
        );
        assert!(
            !mine.script.contains("echo not-mine"),
            "a step condition naming another repository was kept: {}",
            mine.script
        );
    }

    /// Commit `files` (relative path → contents) into a fresh repo and return
    /// the temp dir plus the commit sha.
    pub(super) fn commit_repo(files: &[(&str, &[u8])]) -> (tempfile::TempDir, String) {
        let temp = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let target = temp.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(temp.path())).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(temp.path())
            )
            .unwrap()
            .success());
        assert!(git
            .run(&["add", "-A"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "fixture"], Some(temp.path()))
            .unwrap()
            .success());
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        (temp, sha)
    }

    fn commit_owned_repo(files: &[(String, Vec<u8>)]) -> (tempfile::TempDir, String) {
        let borrowed = files
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_slice()))
            .collect::<Vec<_>>();
        commit_repo(&borrowed)
    }

    fn fenced_text_after<'a>(document: &'a str, marker: &str) -> &'a str {
        assert_eq!(
            document.match_indices(marker).count(),
            1,
            "the documentation must carry exactly one `{marker}` inventory"
        );
        let after_marker = document
            .split_once(marker)
            .expect("the inventory marker was counted above")
            .1;
        let after_open = after_marker
            .split_once("```text\n")
            .unwrap_or_else(|| panic!("`{marker}` is not followed by a text fence"))
            .1;
        after_open
            .split_once("\n```")
            .unwrap_or_else(|| panic!("the text fence after `{marker}` is not closed"))
            .0
            .trim()
    }

    fn remove_loose_object(repo_path: &std::path::Path, object_id: &str) {
        let object_path = repo_path
            .join(".git/objects")
            .join(&object_id[..2])
            .join(&object_id[2..]);
        assert!(
            object_path.exists(),
            "fixture must keep {object_id} as a loose object"
        );
        std::fs::remove_file(object_path).expect("remove fixture object");
    }

    #[test]
    fn broken_workflow_yaml_reports_the_file_and_the_parse_error() {
        // Present-but-broken must never degrade into "no CI config found":
        // the `.plombir-git-ci.yml` fallback below would otherwise hide the typo.
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/ci.yml",
                b"on: push\njobs:\n  build:\n   steps:\n  - run: echo broken\n" as &[u8],
            ),
            (".plombir-git-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(".gitea/workflows/ci.yml"),
            "error must name the offending file: {rendered}"
        );
        assert!(
            !rendered.contains("no CI config found"),
            "a broken workflow must not be reported as a missing config: {rendered}"
        );
        // The YAML reason (with its position) has to survive to the log line.
        assert!(
            rendered.contains("line") || rendered.contains("column"),
            "error must carry the parse reason: {rendered}"
        );
    }

    #[test]
    fn non_utf8_workflow_reports_the_file_instead_of_parsing_an_empty_string() {
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/ci.yml", &[0xff, 0xfe, b'o', b'n', b':']),
            (".plombir-git-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(".gitea/workflows/ci.yml") && rendered.contains("UTF-8"),
            "error must name the file and the encoding problem: {rendered}"
        );
    }

    #[test]
    fn an_oversized_native_config_is_refused_before_parsing() {
        let oversized = vec![b'#'; MAX_CI_CONFIG_BYTES as usize + 1];
        let (temp, sha) = commit_repo(&[(".plombir-git-ci.yml", oversized.as_slice())]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("a native config over the ceiling must not be loaded for parsing");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "a repository-owned config is a client-correctable refusal: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(message.contains(".plombir-git-ci.yml"), "{message}");
        assert!(
            message.contains(&format!("larger than {MAX_CI_CONFIG_BYTES} bytes")),
            "{message}"
        );
    }

    /// A repository whose entry at `path` is a real mode-160000 gitlink. The
    /// submodule's objects live under `.git/modules/`, so its commit oid is
    /// legitimately absent from the parent's object store — the shape that made
    /// the parent look corrupt. The returned temp dir owns both repositories.
    fn commit_repo_with_submodule_at(
        path: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        let outer = dir.path().join("outer");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        for repo in [&inner, &outer] {
            assert!(git
                .run(&["init", "-q", "-b", "main", repo.to_str().unwrap()], None)
                .unwrap()
                .success());
            for args in [
                ["config", "user.name", "CI"],
                ["config", "user.email", "ci@example.com"],
                ["config", "commit.gpgsign", "false"],
            ] {
                assert!(git.run(&args, Some(repo)).unwrap().success());
            }
        }

        std::fs::write(inner.join("inner.txt"), "vendored\n").unwrap();
        assert!(git.run(&["add", "-A"], Some(&inner)).unwrap().success());
        assert!(git
            .run(&["commit", "-qm", "inner"], Some(&inner))
            .unwrap()
            .success());

        assert!(git
            .run(
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "-q",
                    inner.to_str().unwrap(),
                    path,
                ],
                Some(&outer),
            )
            .unwrap()
            .success());
        assert!(git.run(&["add", "-A"], Some(&outer)).unwrap().success());
        assert!(git
            .run(&["commit", "-qm", "gitlink"], Some(&outer))
            .unwrap()
            .success());
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(&outer))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();

        let repo = rg_git::repository::open(&outer).expect("the parent repository must open");
        let entry = tree_at_commit(&repo, &sha)
            .expect("the parent tree must be readable")
            .lookup_entry_by_path(path)
            .expect("the parent tree must decode")
            .expect("the gitlink must be in the parent tree");
        assert!(
            entry.mode().is_commit(),
            "the fixture must commit `{path}` as a gitlink, not as {}",
            entry.mode().as_str()
        );
        assert!(
            repo.find_header(entry.oid()).is_err(),
            "the fixture must keep the gitlink target foreign to the parent object store"
        );
        drop(repo);

        (dir, outer, sha)
    }

    /// `card_e805b4f8ac12` — a gitlink's oid belongs to the submodule's own
    /// repository, so its absence here is healthy. Reading the object header
    /// before classifying the committed mode reported that shape as
    /// `failed to read CI config object header`: a storage 5xx for a tree only
    /// the client can change.
    #[test]
    fn a_gitlink_named_like_the_native_config_is_a_client_error() {
        let (_dir, repo_path, sha) = commit_repo_with_submodule_at(".plombir-git-ci.yml");

        let error =
            read_ci_config_for_test(&repo_path, &sha, "refs/heads/main", "push", None, None)
                .expect_err("a submodule cannot be read as a native CI config");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "the committed tree shape is the client's to fix: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(
            message.contains(".plombir-git-ci.yml")
                && message.contains("is a submodule, not a file"),
            "{message}"
        );
        assert!(
            !message.contains("object header"),
            "a healthy submodule must not be reported as an unreadable object: {message}"
        );
    }

    /// The sideways half of `card_e805b4f8ac12`: the Gitea loader classified
    /// each `*.yml` inside the directory by mode, but fetched the object of the
    /// directory entry itself before looking at its mode. A submodule mounted at
    /// `.gitea/workflows` is the same healthy-but-foreign oid, so it came back
    /// as `failed to read .gitea/workflows` instead of a refusal naming what the
    /// repository actually committed.
    #[test]
    fn a_gitlink_mounted_at_the_workflow_directory_is_a_client_error() {
        let (_dir, repo_path, sha) = commit_repo_with_submodule_at(WORKFLOW_DIR);

        let error =
            read_ci_config_for_test(&repo_path, &sha, "refs/heads/main", "push", None, None)
                .expect_err("a submodule cannot be read as the workflow directory");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "the committed tree shape is the client's to fix: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(
            message.contains(WORKFLOW_DIR) && message.contains("is a submodule, not a directory"),
            "{message}"
        );
        assert!(
            !message.contains("failed to read"),
            "a healthy submodule must not be reported as an unreadable object: {message}"
        );
    }

    /// The mode check must not swallow the case it sits in front of: an entry
    /// that really is committed as a file and whose object is gone is the object
    /// store failing, and has to stay a server error.
    #[test]
    fn a_lost_native_config_object_stays_a_server_error() {
        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo native]\n" as &[u8],
        )]);
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let blob = git
            .run(
                &["rev-parse", "HEAD:.plombir-git-ci.yml"],
                Some(temp.path()),
            )
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        remove_loose_object(temp.path(), &blob);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("a config blob that is gone must not be read");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_none(),
            "a lost object is the server's fault, not the committer's: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(
            message.contains(".plombir-git-ci.yml"),
            "the server error must still name the file: {message}"
        );
    }

    /// The other half of the same boundary. Classifying by mode must not become
    /// a licence to trust the mode: an entry recorded as a file whose object is
    /// a tree is a shape `git mktree` refuses but a hand-built object can carry,
    /// and receive-pack does not fsck what it accepts. It has to be refused by
    /// name — never parsed as YAML, never a storage failure.
    #[test]
    fn a_blob_mode_entry_over_a_tree_object_is_refused_by_name() {
        let (temp, _sha) = commit_repo(&[("sub/x.txt", b"x\n" as &[u8])]);
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let tree_oid = git
            .run(&["rev-parse", "HEAD:sub"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();

        // `git mktree` refuses a type it can disprove, so the tree object is
        // assembled byte-for-byte and stored with `--literally`.
        let mut raw = b"100644 .plombir-git-ci.yml\0".to_vec();
        raw.extend((0..tree_oid.len()).step_by(2).map(|i| {
            u8::from_str_radix(&tree_oid[i..i + 2], 16).expect("git prints a hex object id")
        }));
        let raw_path = temp.path().join("raw.tree");
        std::fs::write(&raw_path, &raw).unwrap();
        let corrupt_tree = git
            .run(
                &[
                    "hash-object",
                    "-w",
                    "-t",
                    "tree",
                    "--literally",
                    raw_path.to_str().unwrap(),
                ],
                Some(temp.path()),
            )
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        let sha = git
            .run(
                &["commit-tree", &corrupt_tree, "-m", "corrupt"],
                Some(temp.path()),
            )
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("a blob-mode entry over a tree object must not be parsed as a config");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "an unsound tree is pushed, and pushing a sound one is the fix: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(
            message.contains(".plombir-git-ci.yml") && message.contains("committed as a file"),
            "{message}"
        );
        assert!(
            !message.contains("failed to read"),
            "the entry was readable; only its shape is wrong: {message}"
        );
    }

    #[test]
    fn an_oversized_actions_workflow_is_refused_by_name() {
        let oversized = vec![b'#'; MAX_CI_CONFIG_BYTES as usize + 1];
        let (temp, sha) = commit_repo(&[(".gitea/workflows/oversized.yml", oversized.as_slice())]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("a workflow over the per-file ceiling must not be loaded");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "a repository-owned workflow is a client-correctable refusal: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(
            message.contains(".gitea/workflows/oversized.yml"),
            "{message}"
        );
        assert!(
            message.contains(&format!("larger than {MAX_CI_CONFIG_BYTES} bytes")),
            "{message}"
        );
    }

    #[test]
    fn workflow_source_count_has_an_independent_backstop() {
        let files = vec![
            (".gitea/workflows/a.yml".to_string(), Vec::new()),
            (".gitea/workflows/b.yml".to_string(), Vec::new()),
        ];
        let (temp, sha) = commit_owned_repo(&files);
        let repo = rg_git::repository::open(temp.path()).unwrap();

        let error = load_workflow_sources_with_limits(&repo, &sha, 1, u64::MAX)
            .expect_err("the second tiny workflow must cross the file-count backstop");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "a repository-owned workflow set is a client-correctable refusal: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(message.contains("more than 1 workflow files"), "{message}");
        assert!(message.contains(".gitea/workflows/b.yml"), "{message}");
    }

    #[test]
    fn workflow_sources_spend_one_aggregate_byte_budget() {
        let first = b"on: push\n".to_vec();
        let second = b"on: push\n".to_vec();
        let max_total_bytes = (first.len() + second.len() - 1) as u64;
        let files = vec![
            (".gitea/workflows/a.yml".to_string(), first),
            (".gitea/workflows/b.yml".to_string(), second),
        ];
        let (temp, sha) = commit_owned_repo(&files);
        let repo = rg_git::repository::open(temp.path()).unwrap();

        let error = load_workflow_sources_with_limits(&repo, &sha, usize::MAX, max_total_bytes)
            .expect_err("the second workflow must cross the set's aggregate byte budget");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "a repository-owned workflow set is a client-correctable refusal: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(
            message.contains(&format!("{max_total_bytes}-byte total limit")),
            "{message}"
        );
        assert!(message.contains(".gitea/workflows/b.yml"), "{message}");
    }

    /// Behavior alone cannot distinguish an early ceiling from the same check
    /// after the allocation it was meant to prevent. Pin the production order
    /// at both blob-reading boundaries, and pin the production workflow wrapper
    /// to the named aggregate limits used above.
    #[test]
    fn ci_source_budgets_are_spent_before_blob_reads() {
        fn function<'a>(source: &'a str, signature: &str, next_signature: &str) -> &'a str {
            let start = source
                .find(signature)
                .unwrap_or_else(|| panic!("production source no longer contains `{signature}`"));
            let body = &source[start..];
            let end = body
                .find(next_signature)
                .unwrap_or_else(|| panic!("cannot bound `{signature}` before `{next_signature}`"));
            &body[..end]
        }

        let source = rust_source::production_rust_code_only(include_str!("lib.rs"));

        let native = function(
            &source,
            "fn read_ci_config_with_inputs(",
            "pub fn workflow_dispatch_schema(",
        );
        let native_header = native
            .find("find_header(")
            .expect("the native config reader must inspect the object header");
        let native_ceiling = native
            .find("MAX_CI_CONFIG_BYTES")
            .expect("the native config reader must spend its named ceiling");
        let native_read = native
            .find(".object()")
            .expect("the native config reader no longer uses the asserted blob-read boundary");
        assert!(
            native_header < native_ceiling && native_ceiling < native_read,
            "the native config ceiling must be spent from the object header before `.object()` materializes the blob"
        );

        let wrapper = function(
            &source,
            "fn load_workflow_sources(",
            "fn load_workflow_sources_with_limits(",
        );
        assert!(
            wrapper.contains("MAX_WORKFLOW_FILE_COUNT")
                && wrapper.contains("MAX_WORKFLOW_TOTAL_BYTES"),
            "the production workflow loader must install both named aggregate limits"
        );

        let workflows = function(
            &source,
            "fn load_workflow_sources_with_limits(",
            "fn workflow_prefix(",
        );
        let count = workflows
            .find("workflow_file_count = workflow_file_count")
            .expect("the workflow traversal no longer spends its file-count backstop");
        let header = workflows
            .find("find_header(")
            .expect("the workflow loader must inspect each object's header");
        let ceiling = workflows
            .find("MAX_CI_CONFIG_BYTES")
            .expect("the workflow loader must spend the per-file ceiling");
        let aggregate = workflows
            .find("workflow_total_bytes = workflow_total_bytes")
            .expect("the workflow loader no longer spends one aggregate byte budget");
        let read = workflows
            .find("repo.find_object(")
            .expect("the workflow loader no longer uses the asserted blob-read boundary");
        assert!(
            count < header && header < ceiling && ceiling < aggregate && aggregate < read,
            "workflow count, per-file bytes, and aggregate bytes must all be spent before `find_object` materializes the next blob"
        );
    }

    #[test]
    fn ci_source_budgets_are_the_ones_repository_authors_are_told() {
        let native = include_str!("../../../docs/ci.md");
        assert_eq!(
            fenced_text_after(native, "<!-- inventory: native-ci-size-ceiling -->"),
            MAX_CI_CONFIG_BYTES.to_string(),
            "the native CI reference must state the ceiling enforced before its blob read"
        );

        let actions = include_str!("../../../docs/gitea-actions.md");
        assert_eq!(
            fenced_text_after(
                actions,
                "<!-- inventory: actions-workflow-source-limits -->"
            ),
            format!(
                "per-file-bytes={MAX_CI_CONFIG_BYTES}\n\
                 total-bytes={MAX_WORKFLOW_TOTAL_BYTES}\n\
                 file-count={MAX_WORKFLOW_FILE_COUNT}"
            ),
            "the Actions reference must state all three source budgets the loader enforces"
        );
    }

    #[test]
    fn unsupported_action_in_a_workflow_is_reported_not_swallowed() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/ci.yml",
            b"on: push\njobs:\n  build:\n    steps:\n      - uses: actions/setup-node@v4\n"
                as &[u8],
        )]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap_err();
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(".gitea/workflows/ci.yml") && rendered.contains("setup-node"),
            "error must name the file and the unsupported action: {rendered}"
        );
    }

    /// card_85ba100789a8, on the path a push actually takes: a `needs:` naming
    /// a job that does not exist has to stop the read, because the alternative
    /// is not an error but the *removal* of the dependency — the job is swept
    /// into stage 0 and runs beside the one it declared it was waiting for.
    ///
    /// The repository also carries a native `.plombir-git-ci.yml`: a refusal that
    /// fell through to it would build a green pipeline out of a file the
    /// committer never triggered.
    #[test]
    fn a_needs_naming_a_missing_job_stops_the_read_instead_of_running_the_job_early() {
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/ci.yml",
                b"on: push\njobs:\n  build:\n    steps:\n      - run: cargo build\n  deploy:\n    needs: [buidl]\n    steps:\n      - run: deploy.sh\n" as &[u8],
            ),
            (".plombir-git-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("a needs: target that does not exist must not build a pipeline");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "a typo in the committed workflow is the client's to fix: {error:#}"
        );
        let rendered = format!("{error:#}");
        for expected in [".gitea/workflows/ci.yml", "buidl", "build, deploy"] {
            assert!(
                rendered.contains(expected),
                "the committer has to learn which file, which name and what exists \
                 (missing {expected:?}): {rendered}"
            );
        }
        assert!(
            !rendered.contains("echo native"),
            "the native config must not be reported as what ran: {rendered}"
        );
    }

    #[test]
    fn broken_native_config_reports_the_reason_not_the_whole_file() {
        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo ok]\n   nested: bad\n" as &[u8],
        )]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap_err();
        let rendered = format!("{error}");
        assert!(
            rendered.contains(".plombir-git-ci.yml") && rendered.contains("line"),
            "error must name the file and the parse position: {rendered}"
        );
        assert!(
            !rendered.contains("echo ok"),
            "the file body must not be dumped into the message: {rendered}"
        );
    }

    #[test]
    fn missing_workflow_directory_still_falls_back_to_the_native_config() {
        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo native]\n" as &[u8],
        )]);

        let config =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap();
        assert!(config.jobs.contains_key("build"));
    }

    #[test]
    fn workflow_not_matching_the_event_falls_back_to_the_native_config() {
        // Valid workflow, wrong event: still a legitimate fallback, not an error.
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/tags.yml",
                b"on:\n  push:\n    tags: [v*]\njobs:\n  build:\n    steps:\n      - run: echo tagged\n" as &[u8],
            ),
            (".plombir-git-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);

        let config =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap();
        assert!(config.jobs.contains_key("build"));
        assert!(!config.jobs.keys().any(|name| name.starts_with("tags/")));
    }

    #[test]
    fn untriggered_workflow_without_native_config_says_so_instead_of_no_config() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/tags.yml",
            b"on:\n  push:\n    tags: [v*]\njobs:\n  build:\n    steps:\n      - run: echo tagged\n"
                as &[u8],
        )]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap_err();
        let rendered = format!("{error}");
        assert!(
            rendered.contains(".gitea/workflows") && rendered.contains("refs/heads/main"),
            "error must point at the untriggered workflows and the ref: {rendered}"
        );
        assert!(
            !rendered.contains("no CI config found"),
            "a workflow that exists but is not triggered is not a missing config: {rendered}"
        );
        // card_1d3af3ad4f55: nothing to run, not a broken file. As an
        // `InvalidRequest` the push and pull-request producers published it as
        // a red pipeline; as `NoMatchingCiJobs` they publish nothing, and a
        // manual run or retry still answers `400` with the text above.
        let no_match = error
            .downcast_ref::<rg_core::ci::NoMatchingCiJobs>()
            .unwrap_or_else(|| {
                panic!("an untriggered workflow is no work, not an error: {error:#}")
            });
        assert_eq!(no_match.ref_name, "refs/heads/main");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_none(),
            "an untriggered workflow must not read as a configuration failure: {error:#}"
        );
    }

    #[test]
    fn no_config_at_all_still_reports_no_ci_config_found() {
        let (temp, sha) = commit_repo(&[("README.md", b"nothing to build\n" as &[u8])]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap_err();
        assert!(
            error.to_string().contains("no CI config found"),
            "genuinely missing config keeps its own message: {error:#}"
        );
    }

    #[test]
    fn a_missing_native_config_object_is_not_no_config() {
        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo native]\n" as &[u8],
        )]);
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let object_id = git
            .run(
                &["rev-parse", &format!("{sha}:.plombir-git-ci.yml")],
                Some(temp.path()),
            )
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        remove_loose_object(temp.path(), &object_id);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("a dangling config entry must not become absent config");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("failed to read CI config object"),
            "{rendered}"
        );
        assert!(!rendered.contains("no CI config found"), "{rendered}");
    }

    #[test]
    fn a_missing_workflow_object_does_not_fall_back_to_native_config() {
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/ci.yml",
                b"on: push\njobs:\n  build:\n    steps:\n      - run: echo workflow\n" as &[u8],
            ),
            (".plombir-git-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let object_id = git
            .run(
                &["rev-parse", &format!("{sha}:.gitea/workflows/ci.yml")],
                Some(temp.path()),
            )
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        remove_loose_object(temp.path(), &object_id);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("a dangling workflow must not select the native fallback");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("failed to read object header for .gitea/workflows/ci.yml"),
            "{rendered}"
        );
    }

    /// The rule above, reached through the door a push actually comes in by.
    ///
    /// The unit tests next to `validate_runner_routing` prove what it decides;
    /// this one proves `trigger_pipeline` asks it at all — which is the half
    /// that was missing for the whole life of the defect, and the half a unit
    /// test can never see.
    #[tokio::test]
    async fn a_tagged_native_job_is_refused_by_the_trigger_that_cannot_route_it() {
        async fn trigger(
            repo_path: &std::path::Path,
            sha: &str,
            external_runners: bool,
        ) -> anyhow::Error {
            // Every case below fails before the first query, so the connection
            // only has to exist.
            let db = rg_db::connect_with_pool(
                "sqlite::memory:",
                rg_db::TEST_CONNECT_TIMEOUT_SECS,
                rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
                rg_db::DEFAULT_MAX_CONNECTIONS,
            )
            .await
            .unwrap();
            trigger_pipeline(
                TriggerPipelineParams {
                    db: &db,
                    repo_root: repo_path,
                    repo_path,
                    repo_id: 1,
                    commit_sha: sha,
                    ref_name: "refs/heads/main",
                    trigger_type: "manual",
                    base_branch: None,
                    head_branch: None,
                    previous_sha: None,
                    inputs: None,
                    triggered_by: Some(1),
                    docker_enabled: true,
                    external_runners,
                    allow_host_runner: false,
                    jwt_secret: Some("secret"),
                    encryption_key: Some("secret"),
                    external_url: None,
                    schedule: None,
                },
                &CiNotifications::default(),
            )
            .await
            .expect_err("this fixture has no tables, so no case here can build a pipeline")
        }

        let (repo, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo ok]\n  image: alpine\n  tags: [gpu]\n" as &[u8],
        )]);

        let error = trigger(repo.path(), &sha, false).await;
        let invalid = error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .unwrap_or_else(|| panic!("an unroutable `tags:` must be a 400, got: {error:#}"));
        let message = invalid.to_string();
        for expected in ["build", "tags:", "gpu", "ci.external_runners"] {
            assert!(
                message.contains(expected),
                "the author has to learn which job, which key, which labels and the remedy \
                 (missing {expected:?}): {message}"
            );
        }

        // The same file on an instance that does have labelled runners gets
        // past this rule and dies on the empty fixture database instead — which
        // is how this test tells "refused for its tags" apart from "refused for
        // any reason at all".
        let accepted = trigger(repo.path(), &sha, true).await;
        assert!(
            !format!("{accepted:#}").contains("ci.external_runners"),
            "an instance with external runners must not refuse a job for asking for one: \
             {accepted:#}"
        );
    }

    /// A CI config the client committed wrong is the client's mistake, and the
    /// answer has to say what is wrong with it. As a bare `anyhow` every one of
    /// these reached `AppError::from` with nothing to classify by and came back
    /// a `500` whose body the H-05 sanitizer had already replaced with
    /// "Internal server error" — so pressing "Run pipeline" on a repository with
    /// `when: allways` in it reported a server crash and named neither the job
    /// nor the field.
    ///
    /// Both halves of the split are asserted, because the fix is worth nothing
    /// unless the other half still fails loudly: a config that is committed and
    /// correct but whose blob the object database cannot hand over is *ours*,
    /// stays a 5xx, and must not be dressed up as the client's typo.
    #[tokio::test]
    async fn a_broken_ci_config_is_the_clients_mistake_not_a_server_failure() {
        // Every case below fails in step 1 of `trigger_pipeline`, before the
        // first query, so the connection only has to exist.
        async fn trigger(repo_path: &std::path::Path, sha: &str) -> anyhow::Error {
            let db = rg_db::connect_with_pool(
                "sqlite::memory:",
                rg_db::TEST_CONNECT_TIMEOUT_SECS,
                rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
                rg_db::DEFAULT_MAX_CONNECTIONS,
            )
            .await
            .unwrap();
            trigger_pipeline(
                TriggerPipelineParams {
                    db: &db,
                    repo_root: repo_path,
                    repo_path,
                    repo_id: 1,
                    commit_sha: sha,
                    ref_name: "refs/heads/main",
                    trigger_type: "manual",
                    base_branch: None,
                    head_branch: None,
                    previous_sha: None,
                    inputs: None,
                    triggered_by: Some(1),
                    docker_enabled: false,
                    external_runners: true,
                    allow_host_runner: false,
                    jwt_secret: Some("secret"),
                    encryption_key: Some("secret"),
                    external_url: None,
                    schedule: None,
                },
                &CiNotifications::default(),
            )
            .await
            .expect_err("a broken CI config must not build a pipeline")
        }

        // 1. A typo in `when:` — the config parses, and the rule it breaks is
        //    known exactly. That is the definition of "checked, and it is not
        //    allowed", and it has to arrive as such.
        let (typo, typo_sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo ok]\n  when: allways\n" as &[u8],
        )]);
        let error = trigger(typo.path(), &typo_sha).await;
        let invalid = error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .unwrap_or_else(|| panic!("a rejected `when:` must be a 400, got: {error:#}"));
        // `InvalidRequest`'s own `Display` is what the HTTP layer renders
        // verbatim, so everything the client needs has to live *in it* — not in
        // a `.context(...)` layer the sanitizer drops.
        let message = invalid.to_string();
        for expected in ["build", "when", "allways", "on_success"] {
            assert!(
                message.contains(expected),
                "the client has to learn which job, which field and what is allowed \
                 (missing {expected:?}): {message}"
            );
        }

        // 2. YAML that does not parse at all. The reason travels; the server's
        //    filesystem does not (H-05).
        let (broken, broken_sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo ok\n" as &[u8],
        )]);
        let error = trigger(broken.path(), &broken_sha).await;
        let invalid = error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .unwrap_or_else(|| panic!("unparseable YAML must be a 400, got: {error:#}"));
        let message = invalid.to_string();
        assert!(
            message.contains(".plombir-git-ci.yml"),
            "the answer names the offending file: {message}"
        );
        let repo_dir = broken.path().to_string_lossy().into_owned();
        assert!(
            !message.contains(&repo_dir),
            "H-05: where the repository lives on disk must not reach the client: {message}"
        );

        // 3. The other half. The config is committed and valid; its blob is gone
        //    from the object database. Answering 400 here would tell the client
        //    to fix a file that is already correct, and would hide the outage.
        let (dangling, dangling_sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo ok]\n" as &[u8],
        )]);
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let object_id = git
            .run(
                &["rev-parse", &format!("{dangling_sha}:.plombir-git-ci.yml")],
                Some(dangling.path()),
            )
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        remove_loose_object(dangling.path(), &object_id);

        let error = trigger(dangling.path(), &dangling_sha).await;
        let rendered = format!("{error:#}");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_none(),
            "an unreadable object is the server's failure, not the client's: {rendered}"
        );
        assert!(
            rendered.contains("failed to read CI config object"),
            "the 5xx has to be the config read failing, not something further down: {rendered}"
        );
    }

    /// card_444a8741da37: an event-filter key without a consumer must be
    /// refused while the committed workflow is read, before matching can turn
    /// it into an absent filter or the native fallback can run instead.
    #[test]
    fn an_unknown_event_filter_is_reported_with_its_file_and_key() {
        for (event, filter, expected) in [
            ("pull_request", "types: [labeled]", "pull_request.types"),
            ("push", "branch: [main]", "push.branch"),
        ] {
            let workflow = format!(
                "on:\n  {event}:\n    {filter}\njobs:\n  build:\n    steps:\n      - run: echo workflow\n"
            );
            let (temp, sha) = commit_repo(&[
                (".gitea/workflows/ci.yml", workflow.as_bytes()),
                (".plombir-git-ci.yml", b"build:\n  script: [echo native]\n"),
            ]);

            let error =
                read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", event, None, None)
                    .expect_err("an unknown event filter must stop workflow selection");
            let invalid = error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .unwrap_or_else(|| panic!("an unsupported filter must be a 400, got: {error:#}"));
            let message = invalid.to_string();
            for expected in [".gitea/workflows/ci.yml", expected, "paths-ignore"] {
                assert!(
                    message.contains(expected),
                    "the committer must learn which file, key and filters are supported \
                     (missing {expected:?}): {message}"
                );
            }
            assert!(
                !message.contains("echo native"),
                "a refused workflow must not fall through to the native config: {message}"
            );
        }
    }

    /// A name the workflow file chooses is stored in `VARCHAR(255)`, which on
    /// MySQL refused a longer one at insert time: the author was told the
    /// server failed. The ceiling is now the file's rule on every backend, and
    /// the refusal names the key (card_51b860efc19c).
    #[tokio::test]
    async fn a_name_too_long_for_its_column_is_refused_with_the_key() {
        async fn trigger(file: String) -> anyhow::Error {
            let (repo, sha) = commit_repo(&[(".plombir-git-ci.yml", file.as_bytes())]);
            // Refused before the first query, like every other rule the file
            // breaks, so a connection without tables is enough.
            let db = rg_db::connect_with_pool(
                "sqlite::memory:",
                rg_db::TEST_CONNECT_TIMEOUT_SECS,
                rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
                rg_db::DEFAULT_MAX_CONNECTIONS,
            )
            .await
            .unwrap();
            trigger_pipeline(
                TriggerPipelineParams {
                    db: &db,
                    repo_root: repo.path(),
                    repo_path: repo.path(),
                    repo_id: 1,
                    commit_sha: &sha,
                    ref_name: "refs/heads/main",
                    trigger_type: "manual",
                    base_branch: None,
                    head_branch: None,
                    previous_sha: None,
                    inputs: None,
                    triggered_by: Some(1),
                    docker_enabled: false,
                    external_runners: true,
                    allow_host_runner: false,
                    jwt_secret: Some("secret"),
                    encryption_key: Some("secret"),
                    external_url: None,
                    schedule: None,
                },
                &CiNotifications::default(),
            )
            .await
            .expect_err("a name past the column must not build a pipeline")
        }
        fn refusal(error: &anyhow::Error) -> String {
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .unwrap_or_else(|| panic!("a name past the column is a 400, got: {error:#}"))
                .to_string()
        }

        let long_job = "j".repeat(300);
        let message = refusal(&trigger(format!("{long_job}:\n  script: [echo ok]\n")).await);
        assert!(
            message.contains("the job name") && message.contains("300 characters"),
            "{message}"
        );
        assert!(
            !message.contains(&long_job),
            "the refusal quotes the start of the name, not all of it: {message}"
        );

        let long_stage = "s".repeat(256);
        let message = refusal(
            &trigger(format!(
                "stages: [{long_stage}]\nbuild:\n  stage: {long_stage}\n  script: [echo ok]\n"
            ))
            .await,
        );
        assert!(
            message.contains("the stage name") && message.contains("256 characters"),
            "{message}"
        );

        // At the ceiling the file is fine as far as names go: what stops it
        // here is the table-less connection, not a refusal of the name.
        let at_ceiling = "j".repeat(rg_core::text_limits::MAX_LINE_CHARS);
        let error = trigger(format!("{at_ceiling}:\n  script: [echo ok]\n")).await;
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_none(),
            "255 characters fit the column: {error:#}"
        );
    }

    /// A matrix variant is named `job [key=value, …]`, so the values decide its
    /// length; a job whose own name fits can still produce one that does not.
    #[test]
    fn a_matrix_variant_name_too_long_for_its_column_is_refused() {
        let error = expand_matrix(
            "build",
            &config(BTreeMap::from([("target".into(), vec!["t".repeat(250)])])),
        )
        .expect_err("`build [target=t…]` is past 255 characters");
        let message = error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .unwrap_or_else(|| panic!("the file is wrong, not the server: {error:#}"))
            .to_string();
        assert!(message.contains("the matrix job name"), "{message}");

        let fits = 255 - "build [target=]".len();
        let variants = expand_matrix(
            "build",
            &config(BTreeMap::from([("target".into(), vec!["t".repeat(fits)])])),
        )
        .expect("a variant name of exactly 255 characters fits");
        assert_eq!(variants[0].name.chars().count(), 255);
    }

    /// The group is checked once resolved: `${{ github.ref }}` is short in the
    /// file and as long as the branch in the row.
    #[test]
    fn a_concurrency_group_too_long_once_resolved_is_refused() {
        let group = |template: &str, ref_name: &str| {
            resolved_concurrency_group(
                Some(&config::ConcurrencyConfig {
                    group: template.to_string(),
                    cancel_in_progress: false,
                }),
                ref_name,
            )
        };
        let long_branch = format!("refs/heads/{}", "b".repeat(250));
        let error = group("deploy-${{ github.ref }}", &long_branch)
            .expect_err("deploy-refs/heads/bbb… is past 255 characters");
        let message = error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .unwrap_or_else(|| panic!("the file is wrong, not the server: {error:#}"))
            .to_string();
        assert!(message.contains("concurrency.group"), "{message}");

        let fits = format!("refs/heads/{}", "b".repeat(255 - "x-refs/heads/".len()));
        let resolved = group("x-${{ github.ref }}", &fits)
            .expect("a group of exactly 255 characters fits")
            .expect("the block declares a group");
        assert_eq!(resolved.chars().count(), 255);
    }

    /// card_e949057aaa0d, on the path a push actually takes: an input nothing
    /// consumes must stop the trigger, not produce a job that ran none of what
    /// the workflow asked for.
    ///
    /// The repository also carries a native `.plombir-git-ci.yml`, so a refusal
    /// that quietly fell through to it would look like a green pipeline. The
    /// assertion is that the trigger fails, names the key, and never gets far
    /// enough to build anything.
    #[tokio::test]
    async fn an_action_input_with_no_consumer_stops_the_trigger_by_name() {
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/ci.yml",
                b"on: push\njobs:\n  build:\n    steps:\n      - uses: actions/checkout@v4\n        with:\n          submodules: true\n      - run: echo workflow\n" as &[u8],
            ),
            (".plombir-git-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);

        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        let error = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: temp.path(),
                repo_id: 1,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(1),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .expect_err("an input nothing implements must not build a pipeline");

        let invalid = error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .unwrap_or_else(|| panic!("a workflow feature we cannot run is a 400, got: {error:#}"));
        let message = invalid.to_string();
        for expected in [".gitea/workflows/ci.yml", "submodules", "fetch-depth"] {
            assert!(
                message.contains(expected),
                "the committer has to learn which file, which key and what is honoured \
                 (missing {expected:?}): {message}"
            );
        }
        assert!(
            !message.contains("echo native"),
            "the native config must not be reported as what ran: {message}"
        );
    }

    #[test]
    fn merged_workflow_stages_are_ordered_deterministically() {
        let workflow = |job: &str| {
            format!("on: push\njobs:\n  {job}:\n    steps:\n      - run: echo {job}\n").into_bytes()
        };
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/a.yml", &workflow("first")),
            (".gitea/workflows/b.yml", &workflow("second")),
            (".gitea/workflows/c.yml", &workflow("third")),
        ]);

        let expected =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap()
                .stages
                .unwrap();
        assert_eq!(expected.len(), 3);
        for _ in 0..8 {
            let stages =
                read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                    .unwrap()
                    .stages
                    .unwrap();
            assert_eq!(stages, expected, "stage order must not depend on hashing");
        }
    }

    /// card_f4309bc397b2: `concurrency:` was parsed from the workflow, mapped
    /// into `ConcurrencyConfig`, and then set to `None` by the merge with a
    /// comment where the feature should have been. A repository running its CI
    /// in Actions format got neither serialization nor cancellation, and no
    /// error saying so.
    #[test]
    fn a_workflow_concurrency_block_survives_the_merge() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/deploy.yml",
            b"on: push\nconcurrency:\n  group: deploy-production\n  cancel-in-progress: true\n\
              jobs:\n  ship:\n    steps:\n      - run: echo ship\n" as &[u8],
        )]);

        let concurrency =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap()
                .concurrency
                .expect("the workflow declared a concurrency block");
        assert_eq!(concurrency.group, "deploy-production");
        assert!(
            concurrency.cancel_in_progress,
            "cancel-in-progress was declared and must reach the trigger"
        );
    }

    /// The canonical Actions spelling. `resolve_concurrency_group` only knew
    /// `${{ ref }}` / `${{ branch }}`, so forwarding the group without teaching
    /// something about `github.*` would have swapped "does not work" for "every
    /// ref of this repository is one group" — the worse of the two.
    ///
    /// The un-spaced `${{github.ref_name}}` is in here on purpose: Actions
    /// accepts it, and a fixed-string replace of `"${{ github.ref_name }}"`
    /// silently does not.
    #[test]
    fn a_concurrency_group_expands_the_workflow_name_and_the_ref() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/deploy.yml",
            b"name: Deploy\non: push\n\
              concurrency:\n  group: ${{ github.workflow }}-${{github.ref_name}}\n\
              jobs:\n  ship:\n    steps:\n      - run: echo ship\n" as &[u8],
        )]);

        let concurrency =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap()
                .concurrency
                .expect("the workflow declared a concurrency block");
        assert_eq!(
            concurrency.group, "Deploy-main",
            "an unexpanded group is a literal shared by every ref"
        );
    }

    /// A workflow without `name:` still has to produce a group that differs
    /// from its neighbour's — `github.workflow` falls back to the file.
    #[test]
    fn an_unnamed_workflow_falls_back_to_its_filename_as_the_workflow_label() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/nightly.yml",
            b"on: push\nconcurrency:\n  group: ${{ github.workflow }}\n\
              jobs:\n  audit:\n    steps:\n      - run: echo audit\n" as &[u8],
        )]);

        let concurrency =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap()
                .concurrency
                .expect("the workflow declared a concurrency block");
        assert_eq!(concurrency.group, "nightly");
    }

    /// Agreement is the case the merge can represent: two workflows asking for
    /// the same group get it, rather than the field vanishing because there
    /// were two of them.
    #[test]
    fn two_workflows_declaring_the_same_group_keep_it() {
        let workflow = |job: &str| {
            format!(
                "on: push\nconcurrency:\n  group: deploy-production\n  cancel-in-progress: true\n\
                 jobs:\n  {job}:\n    steps:\n      - run: echo {job}\n"
            )
            .into_bytes()
        };
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/a.yml", &workflow("first")),
            (".gitea/workflows/b.yml", &workflow("second")),
        ]);

        let concurrency =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap()
                .concurrency
                .expect("both workflows declared the same block");
        assert_eq!(concurrency.group, "deploy-production");
        assert!(concurrency.cancel_in_progress);
    }

    /// Disagreement is refused by name. Picking one of the two would subject
    /// the other workflow's jobs to a cancellation nobody declared for them —
    /// the defect commit b4bf9db removed from the lookup, reintroduced through
    /// the merge.
    #[test]
    fn two_workflows_declaring_different_groups_are_refused_by_name() {
        let workflow = |job: &str, group: &str| {
            format!(
                "on: push\nconcurrency:\n  group: {group}\n\
                 jobs:\n  {job}:\n    steps:\n      - run: echo {job}\n"
            )
            .into_bytes()
        };
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/a.yml", &workflow("first", "alpha")),
            (".gitea/workflows/b.yml", &workflow("second", "beta")),
        ]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("two different groups cannot be merged into one pipeline");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "the committed workflows are wrong, not the server: {error:#}"
        );
        let message = format!("{error:#}");
        for expected in ["a.yml", "b.yml", "alpha", "beta"] {
            assert!(
                message.contains(expected),
                "the refusal must name what disagrees, missing {expected}: {message}"
            );
        }
    }

    /// Two workflows on the same group that disagree about *cancellation* are
    /// the same ambiguity: the pipeline can only be cancelled or not.
    #[test]
    fn two_workflows_disagreeing_about_cancellation_are_refused() {
        let workflow = |job: &str, cancel: bool| {
            format!(
                "on: push\nconcurrency:\n  group: shared\n  cancel-in-progress: {cancel}\n\
                 jobs:\n  {job}:\n    steps:\n      - run: echo {job}\n"
            )
            .into_bytes()
        };
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/a.yml", &workflow("first", true)),
            (".gitea/workflows/b.yml", &workflow("second", false)),
        ]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("one pipeline cannot be both cancellable and not");
        assert!(
            format!("{error:#}").contains("cancel-in-progress"),
            "the refusal must say which half disagrees: {error:#}"
        );
    }

    /// A group this engine cannot finish resolving must stop the trigger, not
    /// serialize on the template text. `github.actor` is a name this engine
    /// has no value for.
    #[test]
    fn an_unresolved_group_expression_is_refused_not_serialized_on() {
        let error = resolved_concurrency_group(
            Some(&config::ConcurrencyConfig {
                group: "ci-${{ github.actor || github.ref }}".to_string(),
                cancel_in_progress: true,
            }),
            "refs/heads/main",
        )
        .expect_err("an unresolved group must not become a literal one");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "the committed group is wrong, not the server: {error:#}"
        );

        assert_eq!(
            resolved_concurrency_group(
                Some(&config::ConcurrencyConfig {
                    group: "ci-${{ branch }}".to_string(),
                    cancel_in_progress: false,
                }),
                "refs/heads/main",
            )
            .expect("the native spelling resolves")
            .as_deref(),
            Some("ci-main"),
        );
        assert_eq!(
            resolved_concurrency_group(None, "refs/heads/main").expect("no block, no group"),
            None,
            "a workflow that declared nothing must stay out of every group"
        );
    }

    /// card_074d93bfe327: the repository the whole defect is about — its entire
    /// CI is one `on: pull_request` workflow. Under the `push` event it has
    /// nothing to run (that is not a bug, it asked for PRs); under
    /// `pull_request` it must produce its jobs.
    #[test]
    fn a_pull_request_only_workflow_runs_for_the_pull_request_event() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/pr.yml",
            b"on: pull_request\njobs:\n  verify:\n    steps:\n      - run: echo reviewed\n"
                as &[u8],
        )]);

        let config = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/1/head",
            "pull_request",
            Some("main"),
            None,
        )
        .expect("an on: pull_request workflow must be selected by the pull_request event");
        assert!(
            config.jobs.contains_key("pr/verify"),
            "the workflow's job must be in the config: {:?}",
            config.jobs.keys().collect::<Vec<_>>()
        );

        // The same repository under `push` has nothing to offer, and says so
        // rather than claiming there is no CI config at all.
        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap_err();
        assert!(
            error.to_string().contains("pull_request") || error.to_string().contains("triggered"),
            "the push event must report an untriggered workflow: {error:#}"
        );
    }

    /// A `branches:` filter under `on: pull_request` is matched against the
    /// branch the PR *targets*. Nothing carried that branch into the matcher, so
    /// it fell back to the repository's default — and a PR into `develop` was
    /// judged as if it were a PR into `main`.
    #[test]
    fn the_pull_request_branch_filter_matches_the_base_branch_not_the_default_one() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/pr.yml",
            b"on:\n  pull_request:\n    branches: [develop]\njobs:\n  verify:\n    steps:\n      - run: echo reviewed\n" as &[u8],
        )]);

        let config = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/7/head",
            "pull_request",
            Some("develop"),
            None,
        )
        .expect("a PR into develop must select the workflow filtered on develop");
        assert!(config.jobs.contains_key("pr/verify"));

        // The fixture's default branch is whatever `git init` produced, never
        // `develop`, so without the base branch the filter used to reject this.
        let error = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/7/head",
            "pull_request",
            Some("main"),
            None,
        )
        .unwrap_err();
        assert!(
            !format!("{error:#}").contains("no CI config found"),
            "a PR into another branch is untriggered, not configuration-less: {error:#}"
        );
    }

    /// A PR trigger asks the fallible gate because a typed configuration error
    /// has to reach its best-effort caller and become a visible failed run. The
    /// legacy bool gate deliberately keeps its fail-closed `false` behavior.
    #[test]
    fn the_event_gate_propagates_a_typed_workflow_refusal_instead_of_plain_false() {
        let (repo, sha) = commit_repo(&[(
            ".gitea/workflows/pr.yml",
            b"on:\n  pull_request:\n    types: [closed]\njobs:\n  verify:\n    steps:\n      - run: echo reviewed\n"
                as &[u8],
        )]);

        let engine = CiEngine::new();
        let error = rg_core::ci::CiTrigger::has_workflow_for_event_checked(
            &engine,
            rg_core::ci::WorkflowEventQuery {
                repo_path: repo.path(),
                commit_sha: &sha,
                event: "pull_request",
                ref_name: "refs/pull/1/head",
                base_branch: Some("main"),
                previous_sha: None,
            },
        )
        .expect_err("an unsupported event filter must not look like no matching workflow");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "repository-owned syntax must remain a typed configuration refusal: {error:#}"
        );
        let rendered = format!("{error:#}");
        assert!(rendered.contains(".gitea/workflows/pr.yml"), "{rendered}");
        assert!(rendered.contains("types"), "{rendered}");
    }

    /// The gate the producer asks before creating anything. It must answer for
    /// the *event*, not for "is there any CI here at all" — a repository with a
    /// native config and no workflow would otherwise get a duplicate pipeline on
    /// every PR open and every PR sync.
    #[test]
    fn the_event_gate_answers_per_event_not_per_repository() {
        let (workflow_repo, workflow_sha) = commit_repo(&[(
            ".gitea/workflows/pr.yml",
            b"on: pull_request\njobs:\n  verify:\n    steps:\n      - run: echo reviewed\n"
                as &[u8],
        )]);
        assert!(workflow_matches_event_at(
            workflow_repo.path(),
            &workflow_sha,
            "pull_request",
            "refs/pull/1/head",
            Some("main")
        )
        .unwrap());
        assert!(!workflow_matches_event_at(
            workflow_repo.path(),
            &workflow_sha,
            "push",
            "refs/heads/main",
            None
        )
        .unwrap());

        let (native_repo, native_sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  script: [echo native]\n" as &[u8],
        )]);
        assert!(
            rg_core::ci::has_ci_config(native_repo.path(), &native_sha),
            "the fixture must look like a repository with CI"
        );
        assert!(
            !workflow_matches_event_at(
                native_repo.path(),
                &native_sha,
                "pull_request",
                "refs/pull/1/head",
                Some("main")
            )
            .unwrap(),
            "a native config declares no events — it must not answer for pull_request"
        );
    }

    /// Run a git command in `repo` and assert it succeeded.
    fn git(repo: &std::path::Path, args: &[&str]) {
        let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(
            gateway.run(args, Some(repo)).unwrap().success(),
            "git {args:?} must succeed"
        );
    }

    /// A workflow whose `branches:` filter only accepts `branch`, so the config
    /// is produced exactly when the matcher resolved that branch.
    fn pull_request_workflow(branch: &str) -> Vec<u8> {
        format!(
            "on:\n  pull_request:\n    branches: [{branch}]\njobs:\n  verify:\n    steps:\n      - run: echo reviewed\n"
        )
        .into_bytes()
    }

    /// The fallback the matcher used when `base_branch` is absent read `HEAD`
    /// and, on any failure, answered `main`. Here `HEAD` is readable and names
    /// something else — the filter must follow it.
    #[test]
    fn the_default_branch_filter_follows_head_instead_of_assuming_main() {
        let (temp, sha) =
            commit_repo(&[(".gitea/workflows/pr.yml", &pull_request_workflow("release"))]);
        git(temp.path(), &["branch", "-m", "release"]);

        let config = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/3/head",
            "pull_request",
            None,
            None,
        )
        .expect("the repository's own default branch must satisfy the filter");
        assert!(config.jobs.contains_key("pr/verify"));
    }

    /// The defect: `if let Ok(Some(..))` folded a `HEAD` that cannot be resolved
    /// into the very same `main` an honestly branch-less repository gets. The
    /// filter then judged a broken repository as if its default branch were
    /// `main` — right answer by luck here, wrong answer anywhere else, and
    /// nothing in the log either way.
    #[test]
    fn an_unreadable_head_is_an_error_not_the_default_branch_main() {
        let (temp, sha) =
            commit_repo(&[(".gitea/workflows/pr.yml", &pull_request_workflow("main"))]);
        git(temp.path(), &["branch", "-m", "main"]);

        // Baseline: with a readable HEAD this repository does match.
        read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/3/head",
            "pull_request",
            None,
            None,
        )
        .expect("fixture must match while HEAD is intact");
        assert!(workflow_matches_event_at(
            temp.path(),
            &sha,
            "pull_request",
            "refs/pull/3/head",
            None
        )
        .unwrap());

        // HEAD still says `ref: refs/heads/main`; the branch it points at is
        // no longer a ref at all.
        std::fs::write(temp.path().join(".git/refs/heads/main"), b"not a ref\n").unwrap();

        let error = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/3/head",
            "pull_request",
            None,
            None,
        )
        .expect_err("an unresolvable HEAD must not be matched as `main`");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("HEAD"),
            "the error must name what could not be read: {rendered}"
        );

        let error =
            workflow_matches_event_at(temp.path(), &sha, "pull_request", "refs/pull/3/head", None)
                .expect_err("the event gate must not answer from a guessed default branch");
        assert!(format!("{error:#}").contains("HEAD"), "{error:#}");
    }

    /// An unborn `HEAD` — the branch has no commit yet — still names the branch
    /// it is waiting for, and that name is the default branch. The old fallback
    /// discarded it and said `main`.
    #[test]
    fn an_unborn_head_still_names_its_branch() {
        let (temp, sha) =
            commit_repo(&[(".gitea/workflows/pr.yml", &pull_request_workflow("future"))]);
        // Points HEAD at a branch that does not exist while keeping the objects.
        git(temp.path(), &["checkout", "--orphan", "future"]);

        let config = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/3/head",
            "pull_request",
            None,
            None,
        )
        .expect("an unborn HEAD names the default branch just like a born one");
        assert!(config.jobs.contains_key("pr/verify"));
    }

    /// The one documented fallback: a detached `HEAD` names no branch, so there
    /// is nothing to match against and `main` is the agreed stand-in.
    #[test]
    fn a_detached_head_falls_back_to_the_documented_branch() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/pr.yml",
            &pull_request_workflow(DETACHED_HEAD_BRANCH),
        )]);
        // The default branch is deliberately not the fallback name, so a match
        // can only come from the fallback itself.
        git(temp.path(), &["branch", "-m", "trunk"]);
        git(temp.path(), &["checkout", "--detach"]);

        let config = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/3/head",
            "pull_request",
            None,
            None,
        )
        .expect("a detached HEAD keeps the documented fallback");
        assert!(config.jobs.contains_key("pr/verify"));
    }

    /// card_d92cd3260864: the stage map was built from `stages:` alone, and a
    /// job it could not place was dropped behind a server-side `warn!`. The two
    /// halves of the fix are checked here on the config the author wrote: the
    /// stage nobody has to declare is synthesized, and the stage they *did*
    /// declare wrong is refused by name instead of costing the job its run.
    #[test]
    fn a_stage_no_file_declares_is_either_synthesized_or_refused_by_name() {
        let stageless = |stage: Option<&str>, stages: Option<Vec<String>>| {
            let mut job = config(BTreeMap::new());
            job.stage = stage.map(str::to_owned);
            CiConfig {
                stages,
                concurrency: None,
                jobs: HashMap::from([("deploy".into(), job)]),
                actions_workflow: false,
            }
        };

        // A file that declares only jobs is a valid file: every job resolves to
        // `default`, and `default` is the stage this engine creates for them.
        let implicit = stageless(None, None);
        validate_execution_semantics(&implicit)
            .expect("a job that names no stage belongs to the stage the docs promise it");
        assert_eq!(resolved_stage_order(&implicit), vec!["default".to_string()]);

        // Spelled out, `default` keeps the position the author gave it rather
        // than being appended a second time.
        let spelled = stageless(
            Some("default"),
            Some(vec!["default".into(), "publish".into()]),
        );
        validate_execution_semantics(&spelled).expect("`default` may be declared like any stage");
        assert_eq!(
            resolved_stage_order(&spelled),
            vec!["default".to_string(), "publish".to_string()]
        );

        // Declared stages plus a job that named none: the synthesized stage
        // runs after the ones the file ordered.
        let mixed = stageless(None, Some(vec!["build".into()]));
        assert_eq!(
            resolved_stage_order(&mixed),
            vec!["build".to_string(), "default".to_string()]
        );

        // The typo. Dropping the job was the old answer; naming it is the new
        // one, and the message has to carry what the author can compare.
        let typo = stageless(Some("biuld"), Some(vec!["build".into(), "test".into()]));
        let error = validate_execution_semantics(&typo)
            .expect_err("a job whose stage does not exist must not be silently skipped");
        let message = format!("{error:#}");
        for expected in ["deploy", "biuld", "build", "test"] {
            assert!(
                message.contains(expected),
                "the rejection must name the job, its stage and the declared stages: {message}"
            );
        }
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "the author's own file is the author's to fix, so it must not become a 500: {message}"
        );

        // Two stages of one name: the second takes the jobs and the first can
        // never receive any, and the file cannot say which was meant.
        let duplicate = stageless(
            Some("build"),
            Some(vec!["build".into(), "test".into(), "build".into()]),
        );
        let error = validate_execution_semantics(&duplicate)
            .expect_err("a stage declared twice leaves one of the two unreachable");
        assert!(
            format!("{error:#}").contains("build"),
            "the rejection must name the duplicated stage: {error:#}"
        );

        // No jobs at all is the other spelling of the empty pipeline.
        let jobless = CiConfig {
            stages: Some(vec!["build".into()]),
            concurrency: None,
            jobs: HashMap::new(),
            actions_workflow: false,
        };
        assert!(
            validate_execution_semantics(&jobless).is_err(),
            "a config with no jobs must not become a pipeline that reports success"
        );
    }

    #[test]
    fn manual_jobs_are_supported_and_invalid_execution_policies_fail_closed() {
        let mut job = config(BTreeMap::new());
        job.when = Some("manual".into());
        let config = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job.clone())]),
            actions_workflow: false,
        };
        validate_execution_semantics(&config).unwrap();
        job.when = Some("delayed".into());
        let invalid_when = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job.clone())]),
            actions_workflow: false,
        };
        assert!(validate_execution_semantics(&invalid_when).is_err());
        job.when = None;
        job.timeout_seconds = Some(0);
        let config = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job)]),
            actions_workflow: false,
        };
        assert!(validate_execution_semantics(&config).is_err());
    }

    /// `tags:` names the labels a runner must carry, and the only code that ever
    /// matches them is the poll route external runners use. With
    /// `ci.external_runners` off there is no poll — the in-process runner takes
    /// every job of its stage and does not read the column at all — so a job
    /// routed at a GPU machine ran on the server instead (card_97271ae84c0c).
    #[test]
    fn a_hand_written_tag_is_refused_when_this_instance_has_no_runner_to_honour_it() {
        let tagged = |tags: Option<Vec<String>>, actions_workflow: bool| {
            let mut job = config(BTreeMap::new());
            job.tags = tags;
            CiConfig {
                stages: Some(vec!["test".into()]),
                concurrency: None,
                jobs: HashMap::from([("deploy".into(), job)]),
                actions_workflow,
            }
        };

        let labels = rg_core::ci::default_runner_labels();
        let error =
            validate_runner_routing(&tagged(Some(vec!["gpu".into()]), false), false, &labels)
                .expect_err(
                    "a tagged job must not be handed to a runner that does not carry the label",
                );
        let message = format!("{error:#}");
        assert!(
            message.contains("deploy") && message.contains("gpu") && message.contains("tags:"),
            "the refusal must name the job, the key and the labels it asked for: {message}"
        );
        assert!(
            message.contains("ci.external_runners") && message.contains("ci.runner_labels"),
            "the refusal must name both remedies: {message}"
        );

        // With external runners on, the poll route matches these labels for
        // real, which is the arrangement the key was written for.
        validate_runner_routing(&tagged(Some(vec!["gpu".into()]), false), true, &labels)
            .expect("an instance with external runners honours tags");

        // No labels asked for: any runner may take it, including this one.
        for untagged in [None, Some(Vec::new())] {
            validate_runner_routing(&tagged(untagged, false), false, &labels)
                .expect("a job that names no labels is every runner's to run");
        }

        // A label this instance declares it carries is honoured by running the
        // job here — refusing it would be refusing the operator's own answer.
        validate_runner_routing(
            &tagged(Some(vec!["self-hosted".into()]), false),
            false,
            &labels,
        )
        .expect("a label this instance carries is honoured by running the job on it");

        // And an instance that declares it answers to nothing refuses every
        // label, including the ones the default list would have claimed.
        validate_runner_routing(&tagged(Some(vec!["self-hosted".into()]), false), false, &[])
            .expect_err("an empty ci.runner_labels answers to nothing");
    }

    /// The same column, the other author. An Actions workflow's `runs-on:` is
    /// translated into `tags` ([`gitea_actions::GiteaRunsOn`]) and is mandatory
    /// GitHub syntax, so nearly every workflow carries `ubuntu-latest` as
    /// boilerplate. Reading that as "run this elsewhere" would refuse the whole
    /// Actions engine on a default instance.
    #[test]
    fn a_workflows_runs_on_is_not_read_as_a_routing_instruction() {
        let mut job = config(BTreeMap::new());
        job.tags = Some(vec!["ubuntu-latest".into()]);
        let translated = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job)]),
            actions_workflow: true,
        };

        validate_runner_routing(&translated, false, &rg_core::ci::default_runner_labels())
            .expect("`runs-on: ubuntu-latest` must not refuse an Actions workflow in-process");

        // Not even one this instance cannot honour: refusing here would take
        // the workflow's other jobs down with it, for a key its author rarely
        // chose. That job is refused by the runner instead — see
        // `runner::tests::a_job_labelled_for_another_machine_is_refused_by_the_runner`.
        let mut elsewhere = config(BTreeMap::new());
        elsewhere.tags = Some(vec!["my-gpu-box".into()]);
        let translated = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), elsewhere)]),
            actions_workflow: true,
        };
        validate_runner_routing(&translated, false, &rg_core::ci::default_runner_labels()).expect(
            "an unhonourable `runs-on:` is the runner's refusal to give, not the trigger's",
        );
    }

    fn config_with_timeout(timeout: Option<i64>) -> CiConfig {
        let mut job = config(BTreeMap::new());
        job.timeout_seconds = timeout;
        CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job)]),
            actions_workflow: false,
        }
    }

    /// The whole `1..=86400` range is one rule with one answer. The upper end
    /// and `0` were already refused; a negative value was not — it left
    /// validation as an accepted job whose timeout no runner could honour, and
    /// each runner then substituted a different number of its own (the embedded
    /// one an hour, the external one a second).
    #[test]
    fn a_job_timeout_outside_the_accepted_range_is_refused_by_name() {
        for accepted in [1, 60, 86_400] {
            validate_execution_semantics(&config_with_timeout(Some(accepted)))
                .unwrap_or_else(|error| panic!("{accepted}s must be accepted: {error:#}"));
        }
        validate_execution_semantics(&config_with_timeout(None))
            .expect("an absent timeout means 'use the server default'");

        for refused in [-1, 0, 86_401, i64::MIN] {
            let error = validate_execution_semantics(&config_with_timeout(Some(refused)))
                .expect_err(&format!("{refused}s must be refused"));
            let message = format!("{error:#}");
            assert!(
                message.contains("deploy") && message.contains("timeout_seconds"),
                "the rejection must name the job and the field: {message}"
            );
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a broken config is the client's to fix, so it must not become a 500: {message}"
            );
        }
    }

    /// The rejection has to survive the actual YAML front door: with the field
    /// typed `u64`, `-1` died inside `serde_yaml` before the validator ran, and
    /// the client was told `invalid value: integer -1, expected u64` with no job
    /// name anywhere in it.
    #[test]
    fn a_negative_timeout_in_the_committed_yaml_is_rejected_with_the_job_name() {
        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"stages:\n  - test\n\ndeploy:\n  stage: test\n  timeout_seconds: -1\n  script:\n    - echo ok\n",
        )]);

        let config =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect("a negative timeout must reach the validator, not die in the parser");
        let error = validate_execution_semantics(&config)
            .expect_err("a negative timeout must not produce a runnable pipeline");
        let message = format!("{error:#}");
        assert!(
            message.contains("deploy") && message.contains("-1"),
            "the rejection must name the job and the offending value: {message}"
        );
    }

    /// card_f495ef813343: every present environment declaration must either
    /// retain the protection lookup name or fail by the author's qualified key.
    #[test]
    fn invalid_gitea_environment_forms_fail_before_an_unprotected_job_exists() {
        for (expected_key, environment) in [
            (
                "environment.namee",
                "    environment:\n      namee: production\n",
            ),
            ("environment.name", "    environment: {}\n"),
            ("environment.name", "    environment:\n      name: 42\n"),
            ("environment", "    environment: true\n"),
            ("environment", "    environment: null\n"),
            (
                "environment.url",
                "    environment:\n      name: production\n      url: https://example.invalid\n",
            ),
        ] {
            let workflow = format!(
                "on: push\njobs:\n  deploy:\n{environment}    runs-on: ubuntu-latest\n    steps:\n      - run: echo deploy\n"
            );
            let (temp, sha) = commit_repo(&[
                (".gitea/workflows/environment.yml", workflow.as_bytes()),
                (
                    ".plombir-git-ci.yml",
                    b"fallback:\n  script: [echo must-not-run]\n" as &[u8],
                ),
            ]);

            let error =
                read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                    .expect_err("an invalid environment must not become an unprotected job");
            let message = format!("{error:#}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a committed environment mistake is a client error: {message}"
            );
            assert!(
                message.contains(".gitea/workflows/environment.yml")
                    && message.contains("deploy")
                    && message.contains(expected_key),
                "the refusal must name the workflow, job, and {expected_key:?}: {message}"
            );
        }
    }

    /// card_214f69ecca97: a present `runs-on` declaration must keep every
    /// runner constraint or fail before `trigger_pipeline` reaches its first
    /// database write.
    #[test]
    fn invalid_gitea_runs_on_forms_fail_before_a_weakened_job_exists() {
        for runs_on in ["42", "true", "{}", "null", "[]", "[self-hosted, 42]"] {
            let workflow = format!(
                "on: push\njobs:\n  deploy:\n    runs-on: {runs_on}\n    steps:\n      - run: echo deploy\n"
            );
            let (temp, sha) = commit_repo(&[
                (".gitea/workflows/runs-on.yml", workflow.as_bytes()),
                (
                    ".plombir-git-ci.yml",
                    b"fallback:\n  script: [echo must-not-run]\n" as &[u8],
                ),
            ]);

            let error =
                read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                    .expect_err("an invalid runs-on value must not create a weaker job");
            let message = format!("{error:#}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a committed runs-on mistake is a client error: {message}"
            );
            assert!(
                message.contains(".gitea/workflows/runs-on.yml")
                    && message.contains("deploy")
                    && message.contains("runs-on"),
                "the refusal must name the workflow, job, and runs-on: {message}"
            );
        }

        for (runs_on, expected) in [
            ("ubuntu-latest", vec!["ubuntu-latest".to_string()]),
            (
                "[self-hosted, linux]",
                vec!["self-hosted".to_string(), "linux".to_string()],
            ),
        ] {
            let workflow = format!(
                "on: push\njobs:\n  deploy:\n    runs-on: {runs_on}\n    steps:\n      - run: echo deploy\n"
            );
            let (temp, sha) = commit_repo(&[(".gitea/workflows/runs-on.yml", workflow.as_bytes())]);
            let config =
                read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                    .unwrap_or_else(|error| panic!("valid runs-on {runs_on:?}: {error:#}"));
            let job = config.jobs.values().next().expect("one workflow job");
            assert_eq!(job.tags.as_deref(), Some(expected.as_slice()));
        }
    }

    /// card_7e621a6a84ed: every declared matrix element must either become one
    /// concrete variant or fail before the persisted graph can have a smaller
    /// cardinality than the committed workflow.
    #[test]
    fn invalid_gitea_matrix_values_fail_before_a_shrunken_job_graph_exists() {
        for (expected_path, values) in [
            ("strategy.matrix.target[1]", "[linux, { family: mac }]"),
            ("strategy.matrix.target[0]", "[[linux, macos]]"),
            ("strategy.matrix.target[0]", "[null]"),
        ] {
            let workflow = format!(
                "on: push\njobs:\n  build:\n    strategy:\n      matrix:\n        target: {values}\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo build\n"
            );
            let (temp, sha) = commit_repo(&[(".gitea/workflows/matrix.yml", workflow.as_bytes())]);

            let error =
                read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                    .expect_err("an unsupported matrix value must not shrink the job graph");
            let message = format!("{error:#}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a committed matrix mistake is a client error: {message}"
            );
            assert!(
                message.contains(".gitea/workflows/matrix.yml")
                    && message.contains("build")
                    && message.contains(expected_path),
                "the refusal must name the workflow, job, and {expected_path:?}: {message}"
            );
        }

        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/matrix.yml",
            b"on: push\njobs:\n  build:\n    strategy:\n      matrix:\n        target: [linux, 42, true, false, 1.5]\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo build\n" as &[u8],
        )]);
        let config =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect("every supported scalar must survive the committed-workflow path");
        let (job_name, job) = config.jobs.iter().next().expect("one workflow job");
        let targets = expand_matrix(job_name, job)
            .expect("supported scalars expand")
            .into_iter()
            .map(|variant| variant.variables["MATRIX_TARGET"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            ["linux", "42", "true", "false", "1.5"],
            "one declared scalar must produce one concrete variant"
        );
    }

    /// card_4223cbf9a0a1: a declaration below `jobs.<name>` must either be
    /// translated or fail where the committed workflow is read. Serde normally
    /// discards every field a struct does not name, so these three workflows
    /// used to create runnable jobs after silently losing `services`, a
    /// misspelt `strategy` key, or `container.credentials`.
    #[test]
    fn unknown_gitea_job_keys_are_reported_with_their_file_and_key() {
        for (key, job_body) in [
            // `services:` itself runs now (card_bc6676ec0ae3); a key it does
            // not honour is still refused by name.
            (
                "volumes",
                "    services:\n      postgres:\n        image: postgres:17\n        volumes: ['/:/host']\n",
            ),
            (
                "fail-fsat",
                "    strategy:\n      fail-fsat: false\n      matrix:\n        os: [linux]\n",
            ),
            (
                "credentials",
                "    container:\n      image: registry.example/private:latest\n      credentials:\n        username: ci\n        password: secret\n",
            ),
        ] {
            let workflow = format!(
                "on: push\njobs:\n  build:\n{job_body}    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n"
            );
            let (temp, sha) = commit_repo(&[
                (".gitea/workflows/unknown.yml", workflow.as_bytes()),
                (
                    ".plombir-git-ci.yml",
                    b"fallback:\n  script: [echo must-not-run]\n" as &[u8],
                ),
            ]);

            let error = read_ci_config_for_test(
                temp.path(),
                &sha,
                "refs/heads/main",
                "push",
                None,
                None,
            )
            .expect_err("an unknown Gitea job key must not become a runnable job");
            let message = format!("{error:#}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a committed workflow mistake is a client error: {message}"
            );
            assert!(
                message.contains(".gitea/workflows/unknown.yml") && message.contains(key),
                "the refusal must name the workflow and {key:?}: {message}"
            );
        }
    }

    /// card_fc3db2c4b6f6: workflow-level declarations and their concurrency
    /// block are finite schemas too. Unknown keys must fail at the committed
    /// workflow boundary instead of disappearing before a pipeline is built.
    #[test]
    fn unknown_gitea_wrapper_keys_are_reported_with_their_file_and_supported_fields() {
        for (key, workflow, supported) in [
            (
                "run-name",
                "on: push\nrun-name: deploy\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
                &["name", "on", "jobs", "concurrency", "env", "defaults", "permissions"] as &[&str],
            ),
            (
                "cancel-inprogress",
                "on: push\nconcurrency:\n  group: deploy\n  cancel-inprogress: true\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
                &["group", "cancel-in-progress"],
            ),
        ] {
            let (temp, sha) = commit_repo(&[(
                ".gitea/workflows/unknown.yml",
                workflow.as_bytes(),
            )]);

            let error = read_ci_config_for_test(
                temp.path(),
                &sha,
                "refs/heads/main",
                "push",
                None,
                None,
            )
            .expect_err("an unknown Gitea wrapper key must not produce a pipeline");
            let message = format!("{error:#}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a committed workflow mistake is a client error: {message}"
            );
            assert!(
                message.contains(".gitea/workflows/unknown.yml") && message.contains(key),
                "the refusal must name the workflow and {key:?}: {message}"
            );
            for field in supported {
                assert!(
                    message.contains(field),
                    "the refusal for {key:?} must list supported field {field:?}: {message}"
                );
            }
        }
    }

    fn resolved_actions_expression_fields() -> (CiConfig, Vec<ResolvedActionJobFields>) {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/expressions.yml",
            b"on: push\njobs:\n  build:\n    strategy:\n      matrix:\n        target: [linux, macos]\n    runs-on: [self-hosted, '${{ matrix.target }}']\n    container:\n      image: 'registry.example/${{ github.repository_owner }}/${{ matrix.target }}:latest'\n    environment:\n      name: 'deploy-${{ matrix.target }}'\n    steps:\n      - run: echo ok\n" as &[u8],
        )]);
        let config =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect("supported job-field expressions must survive the committed-workflow path");
        let job_name = "expressions/build";
        let job = &config.jobs[job_name];
        let fields = expand_matrix(job_name, job)
            .unwrap()
            .iter()
            .map(|variant| {
                resolve_action_job_fields(
                    job_name,
                    job,
                    variant,
                    "refs/heads/main",
                    "push",
                    &sha,
                    RepositoryName {
                        owner: "owner",
                        name: "repo",
                    },
                )
                .unwrap()
            })
            .collect();
        (config, fields)
    }

    #[test]
    fn runs_on_expression_resolves_per_matrix_variant_before_pipeline_creation() {
        let (config, fields) = resolved_actions_expression_fields();
        assert!(
            !rg_core::yaml::to_string(&config).unwrap().contains("${{"),
            "Actions syntax must be compiled, not retained in CiConfig"
        );
        assert_eq!(
            fields
                .iter()
                .map(|field| field.tags.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(vec!["self-hosted".into(), "linux".into()]),
                Some(vec!["self-hosted".into(), "macos".into()]),
            ]
        );
    }

    #[test]
    fn container_image_expression_resolves_per_matrix_variant_before_pipeline_creation() {
        let (config, fields) = resolved_actions_expression_fields();
        assert!(!rg_core::yaml::to_string(&config).unwrap().contains("${{"));
        assert_eq!(
            fields
                .iter()
                .map(|field| field.image.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("registry.example/owner/linux:latest"),
                Some("registry.example/owner/macos:latest"),
            ]
        );
    }

    #[test]
    fn environment_name_expression_resolves_per_matrix_variant_before_pipeline_creation() {
        let (config, fields) = resolved_actions_expression_fields();
        assert!(!rg_core::yaml::to_string(&config).unwrap().contains("${{"));
        assert_eq!(
            fields
                .iter()
                .map(|field| field.environment.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("deploy-linux"), Some("deploy-macos")]
        );
    }

    #[tokio::test]
    async fn actions_job_field_expressions_are_persisted_per_matrix_variant() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/expressions.yml",
            b"on: push\njobs:\n  build:\n    strategy:\n      matrix:\n        target: [linux, macos]\n    runs-on: [self-hosted, '${{ matrix.target }}']\n    container:\n      image: 'registry.example/${{ github.repository_owner }}/${{ matrix.target }}:latest'\n    environment:\n      name: 'deploy-${{ matrix.target }}'\n    steps:\n      - run: echo ok\n  scalar:\n    runs-on: ubuntu-latest\n    environment: production\n    steps:\n      - run: echo deploy\n" as &[u8],
        )]);
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("expressions.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "expression-owner",
            "expression@example.com",
            "unused",
            "Expression Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("expressions".into()),
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
        .unwrap();
        let mut protected_environment_ids = HashMap::new();
        for name in ["deploy-linux", "deploy-macos", "production"] {
            let environment = rg_db::ops::ci_environment_ops::create_with_approvers(
                &db,
                rg_db::entities::ci_environment::ActiveModel {
                    id: NotSet,
                    repo_id: Set(repo.id),
                    name: Set(name.to_owned()),
                    protected: Set(true),
                    required_approvals: Set(1),
                    allowed_approver_ids: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                },
                Vec::new(),
            )
            .await
            .unwrap();
            protected_environment_ids.insert(name, environment.id);
        }
        let pipeline_id = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .unwrap();

        let mut jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        jobs.sort_by(|left, right| left.name.cmp(&right.name));
        assert_eq!(jobs.len(), 3);
        for target in ["linux", "macos"] {
            let job = jobs
                .iter()
                .find(|job| job.name.contains(&format!("target={target}")))
                .unwrap_or_else(|| panic!("missing {target} matrix job: {jobs:?}"));
            let expected_image = format!("registry.example/expression-owner/{target}:latest");
            let expected_environment = format!("deploy-{target}");
            assert_eq!(job.image.as_deref(), Some(expected_image.as_str()));
            assert_eq!(
                job.environment_name.as_deref(),
                Some(expected_environment.as_str())
            );
            assert_eq!(
                job.environment_id,
                Some(protected_environment_ids[expected_environment.as_str()])
            );
            assert_eq!(job.status, "waiting_approval");
            assert_eq!(
                serde_json::from_str::<Vec<String>>(job.tags.as_deref().unwrap()).unwrap(),
                vec!["self-hosted", target]
            );
        }
        let scalar = jobs
            .iter()
            .find(|job| job.name.ends_with("/scalar"))
            .unwrap_or_else(|| panic!("missing scalar environment job: {jobs:?}"));
        assert_eq!(scalar.environment_name.as_deref(), Some("production"));
        assert_eq!(
            scalar.environment_id,
            Some(protected_environment_ids["production"])
        );
        assert_eq!(scalar.status, "waiting_approval");
    }

    #[test]
    fn vars_context_is_not_aliased_to_workflow_job_or_step_env() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/vars-context.yml",
            br#"on: push
env:
  NAME: workflow-env
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      NAME: job-env
    steps:
      - env:
          NAME: step-env
        run: echo "${{ vars.NAME }}"
"# as &[u8],
        )]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("vars.* must not borrow a same-named env value");
        let message = format!("{error:#}");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "an unavailable Actions context is a committed-workflow client error: {message}"
        );
        assert!(
            message.contains(".gitea/workflows/vars-context.yml")
                && message.contains("build: step 1 run")
                && message.contains("vars.NAME"),
            "the refusal must name the workflow, expression site, and vars member: {message}"
        );
    }

    #[test]
    fn unavailable_job_field_expression_contexts_are_refused_by_site() {
        for (field, job_body) in [
            ("runs-on", "    runs-on: '${{ secrets.RUNNER }}'\n"),
            (
                "container.image",
                "    runs-on: ubuntu-latest\n    container:\n      image: '${{ secrets.IMAGE }}'\n",
            ),
            (
                "environment.name",
                "    runs-on: ubuntu-latest\n    environment:\n      name: '${{ secrets.ENVIRONMENT }}'\n",
            ),
        ] {
            let workflow =
                format!("on: push\njobs:\n  build:\n{job_body}    steps:\n      - run: echo ok\n");
            let (temp, sha) = commit_repo(&[(
                ".gitea/workflows/unsupported-expression.yml",
                workflow.as_bytes(),
            )]);
            let error = read_ci_config_for_test(
                temp.path(),
                &sha,
                "refs/heads/main",
                "push",
                None,
                None,
            )
            .expect_err("a context unavailable before runner startup must fail loudly");
            let message = format!("{error:#}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a committed workflow mistake is a client error: {message}"
            );
            assert!(
                message.contains(".gitea/workflows/unsupported-expression.yml")
                    && message.contains(field)
                    && message.contains("secrets."),
                "the refusal must name the workflow, field {field:?}, and expression: {message}"
            );
        }
    }

    /// A called workflow's `concurrency` applies only to that reusable
    /// workflow's jobs, while Plombir Git flattens those jobs into the caller's
    /// single pipeline. Silently keeping the caller's value loses the called
    /// declaration; copying the called value would also put the caller's own
    /// jobs into the called workflow's cancellation group. Refuse the
    /// unrepresentable declaration at the committed-workflow front door.
    #[test]
    fn called_reusable_workflow_concurrency_is_refused_by_file_name() {
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/caller.yml",
                b"on: push\njobs:\n  deploy:\n    uses: ./.gitea/workflows/called.yml\n" as &[u8],
            ),
            (
                ".gitea/workflows/called.yml",
                b"on: workflow_call\nconcurrency:\n  group: deploy-production\n  cancel-in-progress: true\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo deploy\n" as &[u8],
            ),
        ]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("called workflow concurrency must not disappear during expansion");
        let message = format!("{error:#}");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "an unrepresentable workflow declaration is a client error: {message}"
        );
        assert!(
            message.contains(".gitea/workflows/caller.yml")
                && message.contains("called.yml")
                && message.contains("`concurrency`")
                && message.contains("calling workflow"),
            "the refusal must name both files, the field, and the supported placement: {message}"
        );
    }

    /// The native format reaches a different parser after the top-level
    /// `#[serde(flatten)]` job map. The job value still has to reject GitLab-
    /// shaped or misspelled keys instead of accepting a misleading no-op.
    #[test]
    fn an_unknown_native_job_key_is_reported_with_its_file_and_key() {
        let (temp, sha) = commit_repo(&[(
            ".plombir-git-ci.yml",
            b"build:\n  retry: 2\n  script: [echo ok]\n",
        )]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .expect_err("an unknown native job key must not become a runnable job");
        let message = format!("{error:#}");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "a committed native config mistake is a client error: {message}"
        );
        assert!(
            message.contains(".plombir-git-ci.yml") && message.contains("retry"),
            "the refusal must name the native config and key: {message}"
        );
    }

    /// Native concurrency and cache blocks are nested below the top-level job
    /// map, so each one needs its own closed schema. Otherwise a typo changes
    /// cancellation or cache behaviour while the file still looks accepted.
    #[test]
    fn unknown_native_wrapper_keys_are_reported_with_their_file_and_supported_fields() {
        for (key, yaml, supported) in [
            (
                "cancel-inprogress",
                "concurrency:\n  group: deploy\n  cancel-inprogress: true\n\nbuild:\n  script: [echo ok]\n",
                &["group", "cancel_in_progress"] as &[&str],
            ),
            (
                "restore_keys",
                "build:\n  script: [echo ok]\n  cache:\n    key: cargo\n    paths: [target]\n    restore_keys: [cargo-]\n",
                &["key", "paths"],
            ),
        ] {
            let (temp, sha) = commit_repo(&[(".plombir-git-ci.yml", yaml.as_bytes())]);

            let error =
                read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                    .expect_err("an unknown native wrapper key must not produce a pipeline");
            let message = format!("{error:#}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a committed native config mistake is a client error: {message}"
            );
            assert!(
                message.contains(".plombir-git-ci.yml") && message.contains(key),
                "the refusal must name the native config and {key:?}: {message}"
            );
            for field in supported {
                assert!(
                    message.contains(field),
                    "the refusal for {key:?} must list supported field {field:?}: {message}"
                );
            }
        }
    }
}

/// card_e87a1b6f9633: the Run button and Retry named events no workflow can
/// declare, so a repository whose CI lives in `.gitea/workflows/` was told its
/// perfectly valid file triggers nothing.
#[cfg(test)]
mod manual_trigger_tests {
    use super::matrix_tests::commit_repo;
    use super::*;

    const DISPATCH_WORKFLOW: &[u8] = b"name: Manual\non:\n  workflow_dispatch:\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n";

    /// card_2a0f4ba3b59d: the row is built only from a document its column can
    /// keep. A producer that skipped the HTTP check still gets a request error
    /// here, before the transaction, rather than MySQL's `1406` as a 500.
    #[test]
    fn dispatch_inputs_the_row_cannot_keep_are_refused_where_the_row_is_built() {
        let fits = std::collections::HashMap::from([(
            "payload".to_string(),
            "x".repeat(rg_core::ci::MAX_DISPATCH_INPUTS_JSON_BYTES - 14),
        )]);
        let stored = persisted_dispatch_inputs(rg_core::ci::WORKFLOW_DISPATCH_EVENT, Some(&fits))
            .expect("a map at the ceiling is stored")
            .expect("a non-empty map is stored");
        assert_eq!(stored.len(), rg_core::ci::MAX_DISPATCH_INPUTS_JSON_BYTES);

        let mut over = fits;
        over.get_mut("payload").unwrap().replace_range(0..1, "\n");
        let error = persisted_dispatch_inputs(rg_core::ci::WORKFLOW_DISPATCH_EVENT, Some(&over))
            .expect_err("an escaped byte past the ceiling must be refused");
        assert!(error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .is_some());
        assert!(format!("{error:#}").contains("'payload'"));
    }

    #[test]
    fn a_bare_workflow_dispatch_trigger_is_read_as_present() {
        let (temp, sha) = commit_repo(&[(".gitea/workflows/manual.yml", DISPATCH_WORKFLOW)]);

        let config = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/heads/main",
            rg_core::ci::WORKFLOW_DISPATCH_EVENT,
            None,
            None,
        )
        .expect("the Run button must reach the workflow that asked for it");
        assert!(
            config.jobs.keys().any(|name| name.contains("build")),
            "the manual run produced no runnable job: {:?}",
            config.jobs.keys().collect::<Vec<_>>()
        );
    }

    /// `on: workflow_dispatch:` with nothing under it is the usual spelling, and
    /// a plain `Option<Value>` reads that empty value as *absent* — which is why
    /// the field existed and still matched nothing. The array form has to keep
    /// working too; it went through a different branch of the matcher.
    #[test]
    fn every_spelling_of_the_manual_trigger_matches_and_only_that_event() {
        for workflow in [
            DISPATCH_WORKFLOW,
            b"name: M\non: workflow_dispatch\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
            b"name: M\non: [push, workflow_dispatch]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
            b"name: M\non:\n  push:\n    branches: [main]\n  workflow_dispatch:\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
        ] {
            let (temp, sha) = commit_repo(&[(".gitea/workflows/manual.yml", workflow)]);
            assert!(
                workflow_matches_event_at(
                    temp.path(),
                    &sha,
                    rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                    "refs/heads/main",
                    None,
                )
                .expect("read the committed workflows"),
                "a manual run must reach this workflow: {}",
                String::from_utf8_lossy(workflow)
            );
        }

        // …and the trigger is not a wildcard: a workflow that only asks for
        // manual runs must stay out of the push pipeline.
        let (temp, sha) = commit_repo(&[(".gitea/workflows/manual.yml", DISPATCH_WORKFLOW)]);
        assert!(
            !workflow_matches_event_at(temp.path(), &sha, "push", "refs/heads/main", None)
                .expect("read the committed workflows"),
            "a manual-only workflow ran on a push"
        );
    }

    #[test]
    fn committed_dispatch_schema_is_the_form_the_web_can_render() {
        let workflow = br#"name: Deploy
on:
  workflow_dispatch:
    inputs:
      deploy:
        description: Whether to deploy
        required: true
        type: boolean
      target:
        type: choice
        options: [staging, production]
        default: staging
      attempts:
        type: number
        default: 2
      note:
        type: string
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
"#;
        let (temp, sha) = commit_repo(&[(".gitea/workflows/deploy.yml", workflow)]);

        let engine = CiEngine::new();
        let schema = rg_core::ci::CiTrigger::workflow_dispatch_schema(
            &engine,
            rg_core::ci::WorkflowDispatchSchemaQuery {
                repo_path: temp.path(),
                commit_sha: &sha,
            },
        )
        .expect("the production engine must expose the committed manual-run schema");

        assert_eq!(
            schema.inputs, schema.workflows[0].inputs,
            "the browser must render the exact aggregate the workflow declares"
        );
        assert_eq!(
            schema.workflows,
            vec![rg_core::ci::WorkflowDispatchWorkflow {
                path: ".gitea/workflows/deploy.yml".into(),
                name: "Deploy".into(),
                inputs: vec![
                    rg_core::ci::WorkflowDispatchInput {
                        name: "attempts".into(),
                        description: None,
                        required: false,
                        input_type: "number".into(),
                        default: Some("2".into()),
                        options: Vec::new(),
                    },
                    rg_core::ci::WorkflowDispatchInput {
                        name: "deploy".into(),
                        description: Some("Whether to deploy".into()),
                        required: true,
                        input_type: "boolean".into(),
                        default: None,
                        options: Vec::new(),
                    },
                    rg_core::ci::WorkflowDispatchInput {
                        name: "note".into(),
                        description: None,
                        required: false,
                        input_type: "string".into(),
                        default: None,
                        options: Vec::new(),
                    },
                    rg_core::ci::WorkflowDispatchInput {
                        name: "target".into(),
                        description: None,
                        required: false,
                        input_type: "choice".into(),
                        default: Some("staging".into()),
                        options: vec!["staging".into(), "production".into()],
                    },
                ],
            }]
        );
    }

    #[test]
    fn committed_dispatch_inputs_apply_types_defaults_and_required_values() {
        let workflow = br#"name: Manual inputs
on:
  workflow_dispatch:
    inputs:
      deploy:
        description: Whether to deploy
        required: true
        type: boolean
      target:
        type: choice
        options: [staging, production]
        default: staging
      attempts:
        type: number
        default: 2
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ inputs.deploy }} ${{ inputs.target }} ${{ inputs.attempts }}"
"#;
        let (temp, sha) = commit_repo(&[(".gitea/workflows/manual.yml", workflow)]);
        let inputs = std::collections::HashMap::from([("deploy".into(), "true".into())]);
        let config = read_ci_config_with_inputs(
            temp.path(),
            RepositoryName {
                owner: "owner",
                name: "repo",
            },
            &sha,
            "refs/heads/main",
            WorkflowInvocation {
                event: rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: Some(&inputs),
                schedule: None,
                actions: None,
                vars: None,
            },
        )
        .expect("typed dispatch inputs must reach the committed workflow");
        let job = &config.jobs["manual/build"];
        let variables = job.variables.as_ref().unwrap();
        assert_eq!(variables["INPUT_DEPLOY"], "true");
        assert_eq!(variables["INPUT_TARGET"], "staging");
        assert_eq!(variables["INPUT_ATTEMPTS"], "2");
        assert!(job
            .script
            .join("\n")
            .contains("${INPUT_DEPLOY} ${INPUT_TARGET} ${INPUT_ATTEMPTS}"));

        for (inputs, expected) in [
            (std::collections::HashMap::new(), "required input 'deploy'"),
            (
                std::collections::HashMap::from([("deploy".into(), "yes".into())]),
                "input 'deploy' must have type boolean",
            ),
            (
                std::collections::HashMap::from([
                    ("deploy".into(), "true".into()),
                    ("typo".into(), "value".into()),
                ]),
                "undeclared input(s): typo",
            ),
        ] {
            let error = read_ci_config_with_inputs(
                temp.path(),
                RepositoryName {
                    owner: "owner",
                    name: "repo",
                },
                &sha,
                "refs/heads/main",
                WorkflowInvocation {
                    event: rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                    base_branch: None,
                    head_branch: None,
                    previous_sha: None,
                    inputs: Some(&inputs),
                    schedule: None,
                    actions: None,
                    vars: None,
                },
            )
            .expect_err("an invalid dispatch input must refuse the committed workflow");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "repository-owned input errors must stay typed: {error:#}"
            );
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }

    /// card_d3036cf8db7f: Plombir Git merges every matching workflow into one
    /// pipeline, so its one request map is the union of their declarations —
    /// not a request that every workflow must declare every key from.
    #[test]
    fn distinct_dispatch_schemas_share_one_filtered_input_map() {
        let deploy = br#"name: Deploy
on:
  workflow_dispatch:
    inputs:
      deploy:
        required: true
        type: boolean
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ inputs.deploy }}"
"#;
        let target = br#"name: Target
on:
  workflow_dispatch:
    inputs:
      target:
        required: true
        type: string
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ inputs.target }}"
"#;
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/a.yml", deploy),
            (".gitea/workflows/b.yml", target),
        ]);

        let schema = workflow_dispatch_schema(temp.path(), &sha)
            .expect("the web form must read the aggregate dispatch contract");
        assert_eq!(
            schema
                .workflows
                .iter()
                .map(|workflow| (
                    workflow.path.as_str(),
                    workflow
                        .inputs
                        .iter()
                        .map(|input| input.name.as_str())
                        .collect::<Vec<_>>(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (".gitea/workflows/a.yml", vec!["deploy"]),
                (".gitea/workflows/b.yml", vec!["target"]),
            ]
        );
        assert_eq!(
            schema
                .inputs
                .iter()
                .map(|input| input.name.as_str())
                .collect::<Vec<_>>(),
            vec!["deploy", "target"],
            "the web form must consume the name-sorted aggregate"
        );

        let inputs = std::collections::HashMap::from([
            ("deploy".into(), "true".into()),
            ("target".into(), "production".into()),
        ]);
        let config = read_ci_config_with_inputs(
            temp.path(),
            RepositoryName {
                owner: "owner",
                name: "repo",
            },
            &sha,
            "refs/heads/main",
            WorkflowInvocation {
                event: rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: Some(&inputs),
                schedule: None,
                actions: None,
                vars: None,
            },
        )
        .expect("each workflow must receive its slice of the aggregate map");

        let deploy_variables = config.jobs["a/build"].variables.as_ref().unwrap();
        assert_eq!(deploy_variables["INPUT_DEPLOY"], "true");
        assert!(!deploy_variables.contains_key("INPUT_TARGET"));
        let target_variables = config.jobs["b/build"].variables.as_ref().unwrap();
        assert_eq!(target_variables["INPUT_TARGET"], "production");
        assert!(!target_variables.contains_key("INPUT_DEPLOY"));

        let mut unknown = inputs;
        unknown.insert("typo".into(), "value".into());
        let error = read_ci_config_with_inputs(
            temp.path(),
            RepositoryName {
                owner: "owner",
                name: "repo",
            },
            &sha,
            "refs/heads/main",
            WorkflowInvocation {
                event: rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: Some(&unknown),
                schedule: None,
                actions: None,
                vars: None,
            },
        )
        .expect_err("an input declared by no matching workflow must be refused");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "the aggregate refusal must remain a client error: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(message.contains("typo"), "{message}");
        assert!(message.contains(".gitea/workflows/a.yml"), "{message}");
        assert!(message.contains(".gitea/workflows/b.yml"), "{message}");
    }

    #[test]
    fn identical_dispatch_input_schemas_have_one_aggregate_field() {
        let workflow = br#"name: Deploy
on:
  workflow_dispatch:
    inputs:
      target:
        description: Where to deploy
        required: true
        type: choice
        options: [staging, production]
        default: staging
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ inputs.target }}"
"#;
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/a.yml", workflow),
            (".gitea/workflows/b.yml", workflow),
        ]);

        let schema = workflow_dispatch_schema(temp.path(), &sha)
            .expect("identical declarations must form one manual-run field");
        assert_eq!(schema.inputs.len(), 1, "the aggregate kept a duplicate");
        assert_eq!(schema.inputs[0].name, "target");
        assert_eq!(schema.inputs[0].input_type, "choice");
        assert_eq!(
            schema.inputs[0].options,
            vec!["staging".to_string(), "production".to_string()]
        );
        assert!(
            schema
                .workflows
                .iter()
                .all(|workflow| workflow.inputs == schema.inputs),
            "per-workflow provenance and the aggregate schema diverged"
        );

        let inputs = std::collections::HashMap::from([("target".into(), "production".into())]);
        let config = read_ci_config_with_inputs(
            temp.path(),
            RepositoryName {
                owner: "owner",
                name: "repo",
            },
            &sha,
            "refs/heads/main",
            WorkflowInvocation {
                event: rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: Some(&inputs),
                schedule: None,
                actions: None,
                vars: None,
            },
        )
        .expect("the compatible aggregate must run both workflows");
        assert_eq!(
            config.jobs["a/build"].variables.as_ref().unwrap()["INPUT_TARGET"],
            "production"
        );
        assert_eq!(
            config.jobs["b/build"].variables.as_ref().unwrap()["INPUT_TARGET"],
            "production"
        );
    }

    /// card_fe9e840132b0: one pipeline-wide input cannot obey two different
    /// schemas. Reject the repository declaration before the web renders a
    /// first-wins form or the trigger starts whichever workflow happens first.
    #[test]
    fn incompatible_dispatch_input_schemas_fail_before_form_or_trigger() {
        let boolean = br#"name: Boolean
on:
  workflow_dispatch:
    inputs:
      target:
        required: true
        type: boolean
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ inputs.target }}"
"#;
        let choice = br#"name: Choice
on:
  workflow_dispatch:
    inputs:
      target:
        required: true
        type: choice
        options: [production]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ inputs.target }}"
"#;
        let mut messages = Vec::new();

        for (a, b) in [(boolean.as_slice(), choice.as_slice()), (choice, boolean)] {
            let (temp, sha) =
                commit_repo(&[(".gitea/workflows/a.yml", a), (".gitea/workflows/b.yml", b)]);
            let schema_error = workflow_dispatch_schema(temp.path(), &sha)
                .expect_err("the schema endpoint must not expose a first-wins form");
            assert!(
                schema_error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "a repository-owned schema conflict is a client error: {schema_error:#}"
            );

            let inputs =
                std::collections::HashMap::from([("target".to_string(), "true".to_string())]);
            let trigger_error = read_ci_config_with_inputs(
                temp.path(),
                RepositoryName {
                    owner: "owner",
                    name: "repo",
                },
                &sha,
                "refs/heads/main",
                WorkflowInvocation {
                    event: rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                    base_branch: None,
                    head_branch: None,
                    previous_sha: None,
                    inputs: Some(&inputs),
                    schedule: None,
                    actions: None,
                    vars: None,
                },
            )
            .expect_err("the real trigger must reject the same aggregate contract");
            assert!(
                trigger_error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "the trigger lost the client-error type: {trigger_error:#}"
            );

            let message = format!("{schema_error:#}");
            assert_eq!(message, format!("{trigger_error:#}"));
            assert!(message.contains("target"), "{message}");
            assert!(message.contains(".gitea/workflows/a.yml"), "{message}");
            assert!(message.contains(".gitea/workflows/b.yml"), "{message}");
            messages.push(message);
        }

        assert_eq!(
            messages[0], messages[1],
            "which declaration was encountered first changed the refusal"
        );
    }

    /// card_24f475c09a17: the pipeline row is the only place the values a
    /// manual run was started with can survive, and a retry has nowhere else to
    /// read them from.
    ///
    /// The caller's own map is what gets written — not the resolved one. A
    /// `default:` frozen into the row would make the retry of a run that never
    /// named `target` indistinguishable from one that chose the default value
    /// on purpose, and would keep applying yesterday's default to a workflow
    /// whose declarations the retry re-reads anyway.
    #[tokio::test]
    async fn a_manual_run_records_the_inputs_it_was_started_with() {
        const WORKFLOW: &[u8] = br#"name: Manual
on:
  push:
  workflow_dispatch:
    inputs:
      deploy:
        required: true
        type: boolean
      target:
        type: choice
        options: [staging, production]
        default: staging
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ inputs.deploy }} ${{ inputs.target }}"
"#;
        let (temp, sha) = commit_repo(&[(".gitea/workflows/manual.yml", WORKFLOW)]);
        let (db, repo_id, user_id) = run_provenance_fixture(temp.path()).await;
        let notifications = CiNotifications::default();

        let inputs = std::collections::HashMap::from([("deploy".to_string(), "true".to_string())]);
        let manual = trigger_pipeline(
            run_provenance_params(
                &db,
                temp.path(),
                repo_id,
                &sha,
                user_id,
                rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                Some(&inputs),
            ),
            &notifications,
        )
        .await
        .expect("a manual run with a valid required input must publish a pipeline");

        let stored = rg_db::ops::pipeline_ops::get_pipeline(&db, manual)
            .await
            .unwrap()
            .expect("the manual pipeline row")
            .dispatch_inputs
            .expect("a manual run must record the inputs it was started with");
        assert_eq!(
            serde_json::from_str::<std::collections::HashMap<String, String>>(&stored).unwrap(),
            inputs,
            "the row must carry the caller's own map, unresolved: {stored}"
        );

        // The same workflow reached by an event that has no inputs records
        // none — `dispatch_inputs` is provenance of a manual run, not a slot
        // every producer fills with something.
        let pushed = trigger_pipeline(
            run_provenance_params(&db, temp.path(), repo_id, &sha, user_id, "push", None),
            &notifications,
        )
        .await
        .expect("the push side of the same workflow must still publish a pipeline");
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, pushed)
                .await
                .unwrap()
                .expect("the push pipeline row")
                .dispatch_inputs,
            None,
            "a push recorded workflow_dispatch inputs it never had"
        );
    }

    /// A migrated database, a user and a repository row — `trigger_pipeline`
    /// resolves the repository identity before it reads the workflow.
    ///
    /// Shared with `replay_context_tests`, which asks the same question about
    /// the other three values a producer parameterises a run with.
    pub(super) async fn run_provenance_fixture(
        repo_path: &std::path::Path,
    ) -> (rg_db::DatabaseConnection, i64, i64) {
        use sea_orm::ActiveValue::{NotSet, Set};

        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                repo_path.join("run-provenance.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "run-provenance-owner",
            "run-provenance@example.com",
            "unused",
            "Dispatch Provenance Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("run-provenance".into()),
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
        .unwrap();
        (db, repo.id, user.id)
    }

    /// The trigger a provenance test starts from: the `main` branch of the
    /// fixture repository, with every value a producer decides left at its
    /// "nothing to hand over" default. A caller that is about a particular one
    /// of them sets that field on the returned struct, so this signature does
    /// not grow one argument per column.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_provenance_params<'a>(
        db: &'a rg_db::DatabaseConnection,
        repo_path: &'a std::path::Path,
        repo_id: i64,
        sha: &'a str,
        user_id: i64,
        trigger_type: &'a str,
        inputs: Option<&'a std::collections::HashMap<String, String>>,
    ) -> TriggerPipelineParams<'a> {
        TriggerPipelineParams {
            db,
            repo_root: repo_path,
            repo_path,
            repo_id,
            commit_sha: sha,
            ref_name: "refs/heads/main",
            trigger_type,
            base_branch: None,
            head_branch: None,
            previous_sha: None,
            inputs,
            triggered_by: Some(user_id),
            docker_enabled: false,
            external_runners: true,
            allow_host_runner: false,
            jwt_secret: Some("secret"),
            encryption_key: Some("secret"),
            external_url: None,
            schedule: None,
        }
    }

    #[test]
    fn committed_trigger_input_unknown_keys_are_named_with_their_workflow() {
        for (trigger, body, expected) in [
            (
                "workflow_dispatch",
                "inputs:\n      target:\n        type: string\n        typo: true",
                "workflow_dispatch.inputs.target.typo",
            ),
            (
                "workflow_call",
                "inputs:\n      target:\n        type: string\n        typo: true",
                "workflow_call.inputs.target.typo",
            ),
        ] {
            let workflow = format!(
                "on:\n  {trigger}:\n    {body}\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n"
            );
            let (temp, sha) = commit_repo(&[(".gitea/workflows/schema.yml", workflow.as_bytes())]);
            let error =
                read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                    .expect_err("an unknown nested schema key must be refused before matching");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "the committed workflow refusal must remain typed: {error:#}"
            );
            let message = format!("{error:#}");
            assert!(message.contains(".gitea/workflows/schema.yml"), "{message}");
            assert!(message.contains(expected), "{message}");
        }
    }
}

/// card_74d58ec3ac1e: the rest of what a retry has to be filtered by.
///
/// `base_branch` and `previous_sha` are the last two of the five values a
/// producer parameterises a trigger with, and the pipeline row is the only
/// place either can survive the event that supplied it. Neither fails loudly
/// when it goes missing: the branch falls back to the repository's default and
/// the revision to the commit's first parent, so a retry that cannot read them
/// back answers a *different* question and still reports `201`.
#[cfg(test)]
mod replay_context_tests {
    use super::manual_trigger_tests::{run_provenance_fixture, run_provenance_params};
    use super::matrix_tests::commit_repo;
    use super::trigger_filter_tests::commit_again;
    use super::*;

    /// One file, both events: the pull-request half is filtered on a branch the
    /// fixture repository does not have as its default, and the push half needs
    /// no target branch at all.
    const BOTH_EVENTS: &[u8] = b"name: Check
on:
  push:
  pull_request:
    branches: [develop]
jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - run: echo reviewed
";

    /// The branch a run's `on:` filters were matched against is written down —
    /// and a run that had no target branch records none rather than the one it
    /// would have fallen back to.
    #[tokio::test]
    async fn a_run_records_the_branch_its_filters_were_matched_against() {
        let (temp, sha) = commit_repo(&[(".gitea/workflows/check.yml", BOTH_EVENTS)]);
        let (db, repo_id, user_id) = run_provenance_fixture(temp.path()).await;
        let notifications = CiNotifications::default();

        let mut params = run_provenance_params(
            &db,
            temp.path(),
            repo_id,
            &sha,
            user_id,
            "pull_request",
            None,
        );
        params.ref_name = "refs/pull/7/head";
        params.base_branch = Some("develop");
        let reviewed = trigger_pipeline(params, &notifications)
            .await
            .expect("a PR into develop must publish the workflow filtered on develop");
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, reviewed)
                .await
                .unwrap()
                .expect("the pull_request pipeline row")
                .base_branch
                .as_deref(),
            Some("develop"),
            "the row must carry the branch the run was judged against — a retry has nowhere \
             else to read it from, and the fallback is whatever this repository defaults to \
             today"
        );

        let pushed = trigger_pipeline(
            run_provenance_params(&db, temp.path(), repo_id, &sha, user_id, "push", None),
            &notifications,
        )
        .await
        .expect("the push half of the same workflow must still publish a pipeline");
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, pushed)
                .await
                .unwrap()
                .expect("the push pipeline row")
                .base_branch,
            None,
            "a push targets no branch but the ref it moves; recording one would replay it \
             against a filter it never faced"
        );
    }

    /// The revision a run's `paths:` filters were diffed from is written down,
    /// and only the producer that actually had one records it.
    #[tokio::test]
    async fn a_run_records_the_revision_its_diff_was_taken_from() {
        let (temp, _) = commit_repo(&[(".gitea/workflows/check.yml", BOTH_EVENTS)]);
        // Two commits, so "where the ref stood before this push" is a different
        // answer from "the head commit's first parent" — which is precisely the
        // difference a lost `previous_sha` erases.
        let (before, _) = commit_again(&temp, &[("backend/main.rs", b"fn main() {}\n")]);
        let (_, after) = commit_again(&temp, &[("README.md", b"docs\n")]);
        let (db, repo_id, user_id) = run_provenance_fixture(temp.path()).await;
        let notifications = CiNotifications::default();

        let mut params =
            run_provenance_params(&db, temp.path(), repo_id, &after, user_id, "push", None);
        params.previous_sha = Some(&before);
        let pushed = trigger_pipeline(params, &notifications)
            .await
            .expect("a push must publish its pipeline");
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, pushed)
                .await
                .unwrap()
                .expect("the push pipeline row")
                .previous_sha
                .as_deref(),
            Some(before.as_str()),
            "the row must carry the revision the push moved the ref from: a retry that falls \
             back to the head commit's first parent computes a narrower diff than the push \
             had, and skips a job the push ran"
        );

        let mut params = run_provenance_params(
            &db,
            temp.path(),
            repo_id,
            &after,
            user_id,
            "pull_request",
            None,
        );
        params.ref_name = "refs/pull/7/head";
        params.base_branch = Some("develop");
        let reviewed = trigger_pipeline(params, &notifications)
            .await
            .expect("the pull-request half of the same workflow must publish too");
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, reviewed)
                .await
                .unwrap()
                .expect("the pull_request pipeline row")
                .previous_sha,
            None,
            "a pull-request run has no previous revision of its own; inventing one would \
             replay it against a diff nobody asked for"
        );
    }

    /// Why the column is not merely a nicety: in a repository that also keeps a
    /// native config, a pull-request run matched without its base branch does
    /// not refuse — it publishes a **different graph** and calls it the same
    /// pipeline.
    #[test]
    fn a_pull_request_matched_without_its_base_branch_falls_through_to_the_native_config() {
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/pr.yml", BOTH_EVENTS),
            (
                ".plombir-git-ci.yml",
                b"native:\n  script:\n    - echo native\n",
            ),
        ]);

        let replayed = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/7/head",
            "pull_request",
            Some("develop"),
            None,
        )
        .expect("the recorded base branch selects the workflow the run was built from");
        assert!(
            replayed.jobs.contains_key("pr/verify"),
            "the replay must rebuild the workflow's own job: {:?}",
            replayed.jobs.keys().collect::<Vec<_>>()
        );

        let rebuilt = read_ci_config_for_test(
            temp.path(),
            &sha,
            "refs/pull/7/head",
            "pull_request",
            None,
            None,
        )
        .expect("this is the silent outcome, not an error");
        assert!(
            rebuilt.jobs.contains_key("native"),
            "without the base branch the workflow matches nothing and the run falls through \
             to .plombir-git-ci.yml — the graph a retry used to publish under the id of a run \
             that never contained it: {:?}",
            rebuilt.jobs.keys().collect::<Vec<_>>()
        );
    }

    /// A column that means "the producer handed this over" must not be filled
    /// with a value that says nothing: an empty `base_branch` reads back as a
    /// branch named `""` and matches no filter at all, while `None` correctly
    /// falls back to the repository's default.
    #[test]
    fn a_replay_value_that_says_nothing_is_recorded_as_nothing() {
        const ZERO: &str = "0000000000000000000000000000000000000000";

        assert_eq!(persisted_replay_value(Some("develop")), Some("develop"));
        assert_eq!(persisted_replay_value(None), None);
        assert_eq!(persisted_replay_value(Some("")), None);
        assert_eq!(persisted_replay_value(Some("   ")), None);
        // The zero sha is a real answer about the push — "this ref did not
        // exist" — and every reader of the column already treats it as one.
        assert_eq!(persisted_replay_value(Some(ZERO)), Some(ZERO));
    }
}

/// card_e1e76c3ede65: `paths`, `paths-ignore` and `tags-ignore` were parsed and
/// then read by nobody.
///
/// The three are driven through `read_ci_config` rather than the matcher alone,
/// because the half that was missing is not the comparison — it is that the
/// commit's diff never reached it.
#[cfg(test)]
mod trigger_filter_tests {
    use super::matrix_tests::commit_repo;
    use super::*;

    fn workflow(on: &str) -> Vec<u8> {
        format!("name: W\non:\n{on}jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n")
            .into_bytes()
    }

    /// Commit `files` on top of the fixture repo and return (repo, before, after).
    pub(super) fn commit_again(
        temp: &tempfile::TempDir,
        files: &[(&str, &[u8])],
    ) -> (String, String) {
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let before = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        for (path, contents) in files {
            let target = temp.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        assert!(git
            .run(&["add", "-A"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "change"], Some(temp.path()))
            .unwrap()
            .success());
        let after = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        (before, after)
    }

    /// Build the event shape path filters care about: the target branch stays
    /// at the fixture commit while a two-commit head first touches `backend/`
    /// and then changes only the README.
    fn multi_commit_pull_request(on: &str) -> (tempfile::TempDir, String, String) {
        let definition = workflow(on);
        let (temp, _) = commit_repo(&[
            (".gitea/workflows/backend.yml", definition.as_slice()),
            ("README.md", b"docs\n"),
        ]);
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let base_branch = git
            .run(&["symbolic-ref", "--short", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        assert!(git
            .run(&["checkout", "-b", "feature"], Some(temp.path()))
            .unwrap()
            .success());
        commit_again(&temp, &[("backend/main.rs", b"fn main() {}\n")]);
        let (_, head_sha) = commit_again(&temp, &[("README.md", b"docs v2\n")]);
        (temp, base_branch, head_sha)
    }

    /// `paths:` is a *narrowing* filter, so ignoring it did not skip work the
    /// author asked for — it ran work the author asked to skip. On a monorepo
    /// with a heavy `paths: [backend/**]` workflow, every README commit paid for
    /// a full run.
    #[test]
    fn a_paths_filter_selects_only_the_commits_that_touch_those_paths() {
        let (temp, _) = commit_repo(&[
            (
                ".gitea/workflows/backend.yml",
                &workflow("  push:\n    paths:\n      - backend/**\n"),
            ),
            ("backend/main.rs", b"fn main() {}\n"),
            ("README.md", b"docs\n"),
        ]);

        let (before, after) = commit_again(&temp, &[("README.md", b"docs, edited\n")]);
        let error = read_ci_config_for_test(
            temp.path(),
            &after,
            "refs/heads/main",
            "push",
            None,
            Some(&before),
        )
        .expect_err("a commit outside `paths:` must not select this workflow");
        assert!(
            format!("{error:#}").contains("triggered"),
            "the refusal must say the workflow sat this event out: {error:#}"
        );

        let (before, after) = commit_again(&temp, &[("backend/main.rs", b"fn main() { }\n")]);
        let config = read_ci_config_for_test(
            temp.path(),
            &after,
            "refs/heads/main",
            "push",
            None,
            Some(&before),
        )
        .expect("a commit inside `paths:` must select the workflow");
        assert!(config.jobs.keys().any(|name| name.contains("build")));
    }

    #[test]
    fn a_paths_ignore_filter_skips_only_commits_that_change_nothing_else() {
        let (temp, _) = commit_repo(&[
            (
                ".gitea/workflows/code.yml",
                &workflow("  push:\n    paths-ignore:\n      - docs/**\n      - '*.md'\n"),
            ),
            ("docs/guide.md", b"guide\n"),
            ("src/main.rs", b"fn main() {}\n"),
        ]);

        let (before, after) = commit_again(&temp, &[("docs/guide.md", b"guide v2\n")]);
        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/heads/main",
                "push",
                None,
                Some(&before)
            )
            .is_err(),
            "a docs-only commit must not run a `paths-ignore: [docs/**]` workflow"
        );

        // One file outside the ignore list is enough — that is GitHub's rule,
        // not "no file inside it".
        let (before, after) = commit_again(
            &temp,
            &[
                ("docs/guide.md", b"guide v3\n"),
                ("src/main.rs", b"fn main(){}\n"),
            ],
        );
        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/heads/main",
                "push",
                None,
                Some(&before)
            )
            .is_ok(),
            "a commit touching code as well as docs must still run"
        );
    }

    /// card_105181820c3b: Actions path filters are not evaluated for tag
    /// pushes. A tag that points at a docs-only commit still selects a release
    /// workflow with `paths: [src/**]`; the same commit on a branch does not.
    #[test]
    fn a_tag_push_satisfies_paths_without_weakening_the_branch_filter() {
        const ZERO: &str = "0000000000000000000000000000000000000000";

        let (temp, _) = commit_repo(&[(
            ".gitea/workflows/release.yml",
            &workflow("  push:\n    paths:\n      - src/**\n"),
        )]);
        let (_, after) = commit_again(&temp, &[("CHANGELOG.md", b"release notes\n")]);

        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/tags/v1.0.0",
                "push",
                None,
                Some(ZERO)
            )
            .is_ok(),
            "a tag push satisfies `paths:` without reading the commit diff"
        );
        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/heads/main",
                "push",
                None,
                Some(ZERO)
            )
            .is_err(),
            "the same docs-only commit on a branch is still outside `paths: [src/**]`"
        );
    }

    /// The upstream tag arm treats `paths-ignore` exactly like `paths`: the
    /// ref filter decides, and no changed path can suppress a tag-triggered run.
    #[test]
    fn a_tag_push_satisfies_paths_ignore_without_weakening_the_branch_filter() {
        const ZERO: &str = "0000000000000000000000000000000000000000";

        let (temp, _) = commit_repo(&[(
            ".gitea/workflows/release.yml",
            &workflow("  push:\n    paths-ignore:\n      - '*.md'\n"),
        )]);
        let (_, after) = commit_again(&temp, &[("CHANGELOG.md", b"release notes\n")]);

        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/tags/v1.0.0",
                "push",
                None,
                Some(ZERO)
            )
            .is_ok(),
            "a tag push satisfies `paths-ignore:` without reading the commit diff"
        );
        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/heads/main",
                "push",
                None,
                Some(ZERO)
            )
            .is_err(),
            "the same Markdown-only commit on a branch is still ignored"
        );
    }

    /// The filter's whole point is that the push range is wider than the last
    /// commit: a fast-forward of two commits, only the first of which touches
    /// the watched path, still has to run.
    #[test]
    fn the_filter_reads_the_whole_push_not_just_its_head_commit() {
        let (temp, _) = commit_repo(&[
            (
                ".gitea/workflows/backend.yml",
                &workflow("  push:\n    paths:\n      - backend/**\n"),
            ),
            ("backend/main.rs", b"fn main() {}\n"),
            ("README.md", b"docs\n"),
        ]);

        let (before, _) = commit_again(&temp, &[("backend/main.rs", b"fn main() { /* 1 */ }\n")]);
        let (_, after) = commit_again(&temp, &[("README.md", b"docs again\n")]);

        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/heads/main",
                "push",
                None,
                Some(&before)
            )
            .is_ok(),
            "the push touched backend/ in its first commit; only its head did not"
        );
        // …and with no previous revision to compare against, the fallback is the
        // head commit's own diff, which here says "README only".
        assert!(
            read_ci_config_for_test(temp.path(), &after, "refs/heads/main", "push", None, None)
                .is_err(),
            "the documented fallback is the commit's own diff"
        );
    }

    /// card_bafc06ac4f57: a pull-request path filter asks what the whole PR
    /// contributes relative to its target, not what only its last commit did.
    #[test]
    fn a_pull_request_path_filter_reads_from_merge_base_to_head() {
        let (temp, base_branch, head_sha) =
            multi_commit_pull_request("  pull_request:\n    paths:\n      - backend/**\n");

        assert!(
            workflow_matches_event_at(
                temp.path(),
                &head_sha,
                "pull_request",
                "refs/pull/7/head",
                Some(&base_branch),
            )
            .expect("the event gate must read the pull request range"),
            "the first PR commit touched backend/, although its head commit only touched README"
        );
        let config = read_ci_config_for_test(
            temp.path(),
            &head_sha,
            "refs/pull/7/head",
            "pull_request",
            Some(&base_branch),
            None,
        )
        .expect("the trigger must read the same pull request range as its event gate");
        assert!(config.jobs.keys().any(|name| name.contains("build")));
    }

    /// Path filters fail open when the repository cannot answer their diff
    /// question. A deleted target branch is such a case: silently treating the
    /// last commit as the range would bring the original under-counting back.
    #[test]
    fn a_pull_request_with_an_unreadable_base_does_not_skip_ci() {
        let (temp, _, head_sha) =
            multi_commit_pull_request("  pull_request:\n    paths:\n      - backend/**\n");

        assert!(
            workflow_matches_event_at(
                temp.path(),
                &head_sha,
                "pull_request",
                "refs/pull/7/head",
                Some("deleted-base"),
            )
            .expect("an unreadable path-filter range must fail open"),
            "an unreadable PR range must run CI instead of guessing from the head commit"
        );
        let config = read_ci_config_for_test(
            temp.path(),
            &head_sha,
            "refs/pull/7/head",
            "pull_request",
            Some("deleted-base"),
            None,
        )
        .expect("the trigger must share the event gate's fail-open range policy");
        assert!(config.jobs.keys().any(|name| name.contains("build")));
    }

    /// `merge_group` already points at a speculative merge commit. Its first
    /// parent is the base tip, so changing it to the PR merge-base rule would
    /// turn the candidate's own path set into an empty diff once the ref moves.
    #[test]
    fn a_merge_group_path_filter_keeps_the_speculative_commits_first_parent() {
        let (temp, base_branch, _) =
            multi_commit_pull_request("  merge_group:\n    paths:\n      - backend/**\n");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git
            .run(&["checkout", &base_branch], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["merge", "--no-ff", "feature", "-m", "merge group"],
                Some(temp.path()),
            )
            .unwrap()
            .success());
        let group_sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();

        assert!(
            workflow_matches_event_at(
                temp.path(),
                &group_sha,
                "merge_group",
                "refs/heads/gh-readonly-queue/main/pr-7",
                Some(&base_branch),
            )
            .expect("the merge-group event gate must read its speculative commit"),
            "the speculative merge changed backend/ relative to its first parent"
        );
        let config = read_ci_config_for_test(
            temp.path(),
            &group_sha,
            "refs/heads/gh-readonly-queue/main/pr-7",
            "merge_group",
            Some(&base_branch),
            None,
        )
        .expect("the merge-group trigger must preserve its first-parent range");
        assert!(config.jobs.keys().any(|name| name.contains("build")));
    }

    #[test]
    fn a_tags_ignore_filter_keeps_the_workflow_off_those_tags() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/release.yml",
            &workflow("  push:\n    tags:\n      - 'v*'\n    tags-ignore:\n      - '*-rc*'\n"),
        )]);

        assert!(
            workflow_matches_event_at(temp.path(), &sha, "push", "refs/tags/v1.0.0", None)
                .expect("read workflows"),
            "a release tag must still run"
        );
        assert!(
            !workflow_matches_event_at(temp.path(), &sha, "push", "refs/tags/v1.0.0-rc1", None)
                .expect("read workflows"),
            "`tags-ignore` was parsed and read by nobody"
        );
        // A branch push is not "a tag that was not ignored": the exclusion only
        // has a say over refs the tag half of the filter is about.
        assert!(
            !workflow_matches_event_at(temp.path(), &sha, "push", "refs/heads/main", None)
                .expect("read workflows"),
            "a `tags:` filter still excludes branch pushes"
        );
    }

    /// card_13c2d6a55c3c: the branch half of a ref filter used to be matched
    /// against the *whole* ref of a tag push, so a workflow restricted to
    /// branches ran on tags — the wrong direction for a deploy job.
    #[test]
    fn a_branches_filter_does_not_select_a_tag_push() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/deploy.yml",
            &workflow("  push:\n    branches:\n      - '**'\n"),
        )]);

        assert!(
            workflow_matches_event_at(temp.path(), &sha, "push", "refs/heads/main", None)
                .expect("read workflows"),
            "`branches: ['**']` is every branch"
        );
        // `refs/tags/v1` matched `**` as a whole ref, and the `tags:` half the
        // author never wrote was asked nothing.
        assert!(
            !workflow_matches_event_at(temp.path(), &sha, "push", "refs/tags/v1", None)
                .expect("read workflows"),
            "a tag push must not select a workflow that only asked for branches"
        );
    }

    /// The exclusion half has the same scope as the half it mirrors: declaring
    /// `branches-ignore:` makes the workflow a branch one, and its patterns are
    /// never tried against a tag name.
    #[test]
    fn a_branches_ignore_filter_has_no_say_over_tags() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/wip.yml",
            &workflow("  push:\n    branches-ignore:\n      - 'wip/*'\n    tags:\n      - '**'\n"),
        )]);

        for (ref_name, expected, why) in [
            ("refs/heads/main", true, "a branch outside the ignore list"),
            ("refs/heads/wip/1", false, "`branches-ignore` excludes it"),
            (
                "refs/tags/wip/1",
                true,
                "a tag named like an ignored branch is still a tag",
            ),
        ] {
            assert_eq!(
                workflow_matches_event_at(temp.path(), &sha, "push", ref_name, None)
                    .expect("read workflows"),
                expected,
                "{ref_name}: {why}"
            );
        }

        // …and on its own it is a branch-only filter, so a tag push has no half
        // of this workflow to satisfy.
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/wip.yml",
            &workflow("  push:\n    branches-ignore:\n      - 'wip/*'\n"),
        )]);
        assert!(
            !workflow_matches_event_at(temp.path(), &sha, "push", "refs/tags/v1", None)
                .expect("read workflows"),
            "declaring only the branch half means the workflow is about branches"
        );
    }

    /// Both halves declared: one of them matching is enough. They used to be
    /// `&&`-ed, so such a workflow ran on neither kind of ref — a branch push
    /// failed the tag half and a tag push the branch half.
    #[test]
    fn a_workflow_declaring_both_halves_runs_on_either_kind_of_ref() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/release.yml",
            &workflow("  push:\n    branches:\n      - main\n    tags:\n      - 'v*'\n"),
        )]);

        for (ref_name, expected) in [
            ("refs/heads/main", true),
            ("refs/tags/v1.0.0", true),
            ("refs/heads/feature", false),
            ("refs/tags/nightly", false),
        ] {
            assert_eq!(
                workflow_matches_event_at(temp.path(), &sha, "push", ref_name, None)
                    .expect("read workflows"),
                expected,
                "{ref_name} against `branches: [main]` + `tags: ['v*']`"
            );
        }
    }

    /// The one ref shape that is neither `refs/heads/…` nor `refs/tags/…`: the
    /// bare short name a pipeline row written before the trigger endpoints
    /// canonicalised their input still carries into a retry. It is read as the
    /// branch it used to mean, which is a decision rather than a discovery —
    /// hence this test.
    #[test]
    fn a_ref_outside_both_namespaces_is_read_as_a_branch() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/main.yml",
            &workflow("  push:\n    branches:\n      - main\n"),
        )]);

        assert!(
            workflow_matches_event_at(temp.path(), &sha, "push", "main", None)
                .expect("read workflows"),
            "a bare short name is the branch name it names"
        );
        assert!(
            !workflow_matches_event_at(temp.path(), &sha, "push", "other", None)
                .expect("read workflows"),
            "…and is still matched against the pattern"
        );
    }

    /// The ref patterns the old ladder of `starts_with` / `ends_with` special
    /// cases silently answered `false` to. Both shapes are ordinary: `**` under
    /// a directory, and a star at each end — which is how every `tags-ignore`
    /// for pre-releases is written.
    #[test]
    fn ref_patterns_with_a_star_in_the_middle_match() {
        use crate::gitea_actions::match_glob_for_test as m;
        assert!(m("releases/1.0", "releases/**"));
        assert!(m("releases/1.0/hotfix", "releases/**"));
        assert!(!m("releases", "releases/**"));
        assert!(m("v1.0.0-rc1", "*-rc*"));
        assert!(!m("v1.0.0", "*-rc*"));
        assert!(m("v1.0.0", "v*"));
        assert!(m("main", "*"));
        // `*` stops at the separator, `**` does not — GitHub's rule for refs as
        // well as for paths.
        assert!(!m("feature/x", "*"));
        assert!(m("feature/x", "**"));
        assert!(m("main", "main"));
        assert!(!m("maintenance", "main"));
    }

    /// GitHub reads a filter list in order, and `['**', '!docs/**']` is the
    /// canonical spelling of "everything except the docs". Under the `any()`
    /// this list used to get, the leading `**` answered first and the exclusion
    /// never got a say: a commit touching one README paid for the full run.
    #[test]
    fn a_negated_path_pattern_excludes_what_an_earlier_pattern_selected() {
        let (temp, _) = commit_repo(&[
            (
                ".gitea/workflows/code.yml",
                &workflow(
                    "  push:\n    paths:\n      - '**'\n      - '!docs/**'\n      - docs/deploy.md\n",
                ),
            ),
            ("docs/guide.md", b"guide\n"),
            ("src/main.rs", b"fn main() {}\n"),
        ]);

        let (before, after) = commit_again(&temp, &[("docs/guide.md", b"guide v2\n")]);
        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/heads/main",
                "push",
                None,
                Some(&before)
            )
            .is_err(),
            "`!docs/**` must subtract from the `**` above it, not match a path named `!docs/…`"
        );

        let (before, after) = commit_again(&temp, &[("src/main.rs", b"fn main() { }\n")]);
        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/heads/main",
                "push",
                None,
                Some(&before)
            )
            .is_ok(),
            "the exclusion must not swallow what it does not name"
        );

        // Order is the whole mechanism: a later plain pattern selects back what
        // the negation excluded.
        let (before, after) = commit_again(&temp, &[("docs/deploy.md", b"deploy\n")]);
        assert!(
            read_ci_config_for_test(
                temp.path(),
                &after,
                "refs/heads/main",
                "push",
                None,
                Some(&before)
            )
            .is_ok(),
            "a plain pattern after the negation must select the path again"
        );
    }

    #[test]
    fn a_negated_branch_pattern_excludes_a_branch_an_earlier_pattern_selected() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/release.yml",
            &workflow("  push:\n    branches:\n      - 'release/**'\n      - '!release/wip'\n"),
        )]);

        assert!(
            workflow_matches_event_at(temp.path(), &sha, "push", "refs/heads/release/1.0", None)
                .expect("read workflows"),
            "the release branches the author selected must still run"
        );
        assert!(
            !workflow_matches_event_at(temp.path(), &sha, "push", "refs/heads/release/wip", None)
                .expect("read workflows"),
            "`!release/wip` was a literal, so the branch it names ran anyway"
        );
    }

    /// `branches:` took a short-cut around the glob matcher for any pattern
    /// without a `*`, so the `\` escape was an ordinary byte on a ref while the
    /// very same spelling worked on a path.
    ///
    /// The vehicle used to be `v?`, until `?` was refused by name
    /// (card_eeffc067afdd) — so the escape carries the check on its own now,
    /// and it carries it better: `v\.1` is a pattern the short-cut answered
    /// `false` to for *every* ref, because no ref carries a backslash.
    #[test]
    fn ref_patterns_use_the_same_dialect_as_path_patterns() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/w.yml",
            &workflow("  push:\n    branches:\n      - 'v\\.1'\n"),
        )]);

        assert!(
            workflow_matches_event_at(temp.path(), &sha, "push", "refs/heads/v.1", None)
                .expect("read workflows"),
            "`\\.` is the literal `.` of the ref, as it is of a path"
        );
        assert!(
            !workflow_matches_event_at(temp.path(), &sha, "push", "refs/heads/v\\.1", None)
                .expect("read workflows"),
            "the backslash escapes the next character instead of standing for itself"
        );
    }

    /// The other half of that dialect: `?` is refused where the file is read,
    /// so a ref pattern using it never reaches the matcher at all. A workflow
    /// whose filters cannot be honoured is not a workflow that selects this
    /// event (card_eeffc067afdd).
    #[test]
    fn a_ref_pattern_using_a_question_mark_is_not_run_as_a_wildcard() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/w.yml",
            &workflow("  push:\n    branches:\n      - 'release?/**'\n"),
        )]);

        let error =
            workflow_matches_event_at(temp.path(), &sha, "push", "refs/heads/releaseX/api", None)
                .expect_err("a filter this engine cannot honour is refused, not silently widened")
                .to_string();
        assert!(error.contains("release?/**"), "{error}");
        assert!(error.contains("`?`"), "{error}");
    }

    #[test]
    fn path_patterns_follow_the_separator_rules() {
        use crate::gitea_actions::match_path_pattern_for_test as m;
        assert!(m("backend/main.rs", "backend/**"));
        assert!(m("backend/api/v1/mod.rs", "backend/**"));
        assert!(m("backend/main.rs", "backend/"));
        assert!(!m("backendish/main.rs", "backend/**"));
        // A single `*` stops at the separator; `**` does not.
        assert!(m("README.md", "*.md"));
        assert!(!m("docs/README.md", "*.md"));
        assert!(m("docs/README.md", "**/*.md"));
        assert!(m("README.md", "**/*.md"));
        assert!(m("src/a/b/c.rs", "src/**/*.rs"));
        assert!(m("exact/path.rs", "exact/path.rs"));
        // `?` is not a wildcard here and not GitHub's quantifier either — it is
        // refused where the file is read, and whatever reaches the matcher
        // without passing that gate is the literal character (card_eeffc067afdd).
        assert!(m("?.rs", "?.rs"));
        assert!(!m("a.rs", "?.rs"));
        // A backslash makes the next character a literal — GitHub's own escape,
        // and the only way to name a file that carries a metacharacter.
        assert!(m("a*b.txt", "a\\*b.txt"));
        assert!(!m("axb.txt", "a\\*b.txt"));
        assert!(m("!keep.txt", "\\!keep.txt"));
        assert!(!m("keep.txt", "\\!keep.txt"));
    }
}

/// A `.gitea/workflows` job that declares an artifact must leave a file behind
/// that the artifact routes can serve (card_ba2e171eb366).
///
/// The route that publishes an artifact, the list a repository shows, the
/// download and the retention policy were all mounted and tested — over a
/// producer that did not exist. Nothing in either runner ever uploaded one, and
/// neither workflow format could even declare one, so every instance's artifact
/// list was empty and always would be. These tests follow the whole chain the
/// user's own file starts: workflow → job row → the runner that executes it →
/// the artifact row and the bytes it names.
#[cfg(test)]
mod artifact_publication_tests {
    use super::*;
    use sea_orm::{NotSet, Set};

    const WORKFLOW: &[u8] = b"name: build\non: [push]\njobs:\n  build:\n    steps:\n      - run: mkdir -p out && printf 'artifact-bytes' > out/report.txt\n      - uses: actions/upload-artifact@v4\n        with:\n          name: build-report\n          path: out/report.txt\n";

    /// A repository on disk at `<root>/<owner>/<name>`, the shape the runner
    /// derives its storage root from — so `_ci_workspaces`, `_artifacts` and
    /// the blob store all land inside the temporary directory rather than
    /// beside the developer's own repositories.
    async fn workflow_fixture(
        workflow: &[u8],
    ) -> (
        tempfile::TempDir,
        String,
        rg_db::DatabaseConnection,
        rg_db::entities::user::Model,
        rg_db::entities::repository::Model,
        std::path::PathBuf,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let repo_path = temp.path().join("artifact-owner/reports");
        std::fs::create_dir_all(repo_path.join(".gitea/workflows")).unwrap();
        std::fs::write(repo_path.join(".gitea/workflows/build.yml"), workflow).unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.name", "CI"],
            vec!["config", "user.email", "ci@example.com"],
            vec!["add", "-A"],
            vec!["commit", "-m", "workflow"],
        ] {
            assert!(git.run(&args, Some(&repo_path)).unwrap().success());
        }
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(&repo_path))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();

        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("artifacts.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "artifact-owner",
            "artifact-owner@example.com",
            "unused",
            "Artifact Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("reports".into()),
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
        .unwrap();
        (temp, sha, db, user, repo, repo_path)
    }

    /// Build the pipeline graph from the committed workflow without letting
    /// anything run yet, so the run below is a runner this test drives and
    /// awaits rather than a detached task it has to guess the timing of.
    async fn trigger_graph(
        db: &rg_db::DatabaseConnection,
        repo_root: &std::path::Path,
        repo_path: &std::path::Path,
        repo_id: i64,
        sha: &str,
        user_id: i64,
    ) -> i64 {
        trigger_pipeline(
            TriggerPipelineParams {
                db,
                repo_root,
                repo_path,
                repo_id,
                commit_sha: sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                triggered_by: Some(user_id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
                schedule: None,
            },
            &CiNotifications::default(),
        )
        .await
        .expect("a workflow declaring an artifact must still be a workflow this engine runs")
    }

    /// The only job of the pipeline's only stage.
    async fn only_job(
        db: &rg_db::DatabaseConnection,
        pipeline_id: i64,
    ) -> rg_db::entities::pipeline_job::Model {
        let stages = rg_db::ops::pipeline_ops::list_stages_by_pipeline(db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(stages.len(), 1, "the fixture workflow declares one job");
        rg_db::ops::pipeline_ops::list_jobs_by_stage(db, stages[0].id)
            .await
            .unwrap()
            .pop()
            .expect("the workflow's job must reach the pipeline")
    }

    /// card_a6f59b3d45f5: a trigger reads `vars.*` from the database — the
    /// repository's variable over its organization's of the same name — and
    /// the job's steps carry the values.
    #[tokio::test]
    async fn a_trigger_fills_in_the_repositorys_variables_over_its_organizations() {
        use rg_db::ops::ci_variable_ops::{upsert, Owner};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let (temp, sha, db, user, repo, repo_path) = workflow_fixture(
            b"on: push\njobs:\n  build:\n    steps:\n      - run: echo \"${{ vars.REGION }}/${{ vars.TIER }}\"\n",
        )
        .await;
        let org = rg_db::ops::org_ops::create_org(&db, "vars-org", None, None, user.id, "public")
            .await
            .unwrap();
        rg_db::entities::repository::Entity::update_many()
            .col_expr(
                rg_db::entities::repository::Column::OrgId,
                sea_orm::sea_query::Expr::value(org.id),
            )
            .filter(rg_db::entities::repository::Column::Id.eq(repo.id))
            .exec(&db)
            .await
            .unwrap();
        for (owner, name, value) in [
            (Owner::Organization(org.id), "REGION", "eu"),
            (Owner::Organization(org.id), "TIER", "org-tier"),
            (Owner::Repository(repo.id), "TIER", "repo-tier"),
        ] {
            upsert(&db, owner, name, value, user.id).await.unwrap();
        }
        let pipeline_id = trigger_graph(&db, temp.path(), &repo_path, repo.id, &sha, user.id).await;
        let job = only_job(&db, pipeline_id).await;
        let steps = job.steps.unwrap_or_default();
        assert!(steps.contains("echo \\\"eu/repo-tier\\\""), "{steps}");
    }

    /// `actions/upload-artifact` reaches the job row as a declaration the
    /// runner can act on.
    ///
    /// Asserted on the column rather than on the parse alone: the runner reads
    /// this text and nothing else, and a declaration that stops here is exactly
    /// the shape of the defect — the workflow says it publishes, the pipeline
    /// row says nothing, and the run goes green having produced no artifact.
    #[tokio::test]
    async fn a_workflow_upload_step_reaches_the_job_row_as_a_declaration() {
        let (temp, sha, db, user, repo, repo_path) = workflow_fixture(WORKFLOW).await;
        let pipeline_id = trigger_graph(&db, temp.path(), &repo_path, repo.id, &sha, user.id).await;

        let job = only_job(&db, pipeline_id).await;
        let declared: serde_json::Value = serde_json::from_str(
            job.artifacts
                .as_deref()
                .expect("the upload step must be persisted with the job that produces it"),
        )
        .unwrap();
        assert_eq!(declared["name"], "build-report");
        assert_eq!(declared["paths"], serde_json::json!(["out/report.txt"]));
    }

    /// The runner publishes under the storage root it is handed — the root the
    /// HTTP side reads artifacts from — and not under a directory guessed from
    /// where the repository sits (card_a3fbb5635ad1). Here the root is not two
    /// levels above the repository, which is the one layout the guess got right.
    #[tokio::test]
    async fn an_artifact_lands_under_the_storage_root_the_runner_is_handed() {
        let (temp, sha, db, user, repo, repo_path) = workflow_fixture(WORKFLOW).await;
        let storage_root = temp.path().join("storage");
        std::fs::create_dir_all(&storage_root).unwrap();
        let pipeline_id =
            trigger_graph(&db, &storage_root, &repo_path, repo.id, &sha, user.id).await;

        let mut runner =
            PipelineRunner::new_local_only(db.clone(), &storage_root, &repo_path, pipeline_id);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();

        let artifacts = rg_db::ops::artifact_ops::list_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(artifacts.len(), 1);
        let key = rg_core::blob_storage::BlobKey::new(artifacts[0].file_path.clone()).unwrap();
        // Read the way the download route reads: `LocalBlobStorage` over the
        // instance's root.
        let served = rg_core::blob_storage::LocalBlobStorage::new(&storage_root);
        assert!(
            rg_core::blob_storage::BlobStorage::get(&served, &key)
                .await
                .is_ok(),
            "the artifact is not where the download route looks"
        );
        let guessed = rg_core::blob_storage::LocalBlobStorage::new(temp.path());
        assert!(
            rg_core::blob_storage::BlobStorage::get(&guessed, &key)
                .await
                .is_err(),
            "the artifact was written under the directory two levels above the repository"
        );
    }

    /// The whole chain: the committed workflow, the job the engine built from
    /// it, the runner that executed it, and the artifact row plus the bytes a
    /// download would serve.
    #[tokio::test]
    async fn a_workflow_declared_artifact_is_published_by_the_run_that_produced_it() {
        let (temp, sha, db, user, repo, repo_path) = workflow_fixture(WORKFLOW).await;
        let pipeline_id = trigger_graph(&db, temp.path(), &repo_path, repo.id, &sha, user.id).await;
        let job = only_job(&db, pipeline_id).await;

        let mut runner =
            PipelineRunner::new_local_only(db.clone(), temp.path(), &repo_path, pipeline_id);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();

        let settled = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            settled.status,
            "success",
            "the fixture job must succeed, or the artifact is missing for the wrong reason: {}",
            settled.log.unwrap_or_default()
        );

        let artifacts = rg_db::ops::artifact_ops::list_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(
            artifacts.len(),
            1,
            "the pipeline's artifact list is what the repository page shows, and it is the list \
             that was structurally always empty"
        );
        let artifact = &artifacts[0];
        assert_eq!(artifact.name, "build-report");
        assert_eq!(artifact.job_id, job.id);
        assert!(
            artifact.expires_at.is_some(),
            "an artifact published without an expiry outlives the repository's retention policy"
        );

        // The row is only half of it: an artifact whose bytes are not there is
        // the same empty list with an extra step. Read them back the way the
        // download route does — through the key the row names — and unpack.
        let storage = rg_core::blob_storage::LocalBlobStorage::new(temp.path());
        let key = rg_core::blob_storage::BlobKey::new(artifact.file_path.clone()).unwrap();
        let bytes = rg_core::blob_storage::BlobStorage::get(&storage, &key)
            .await
            .expect("the artifact row must name bytes that exist");
        assert_eq!(
            bytes.len() as i64,
            artifact.size,
            "the recorded size must be the size of the bytes stored"
        );
        assert_eq!(
            hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes)),
            artifact.sha256.clone().unwrap(),
            "the recorded digest is what the download route verifies before serving"
        );

        let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
        let mut found = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().display().to_string();
            let mut content = String::new();
            std::io::Read::read_to_string(&mut entry, &mut content).unwrap();
            found.push((path, content));
        }
        assert_eq!(
            found,
            vec![("out/report.txt".to_string(), "artifact-bytes".to_string())],
            "the archive has to carry the file the job wrote, under the path it declared"
        );

        // Staging is a second full copy of every artifact, and nothing walks it:
        // no row names it and retention reads rows. It has to be gone.
        let staged = temp
            .path()
            .join("_artifacts")
            .join("jobs")
            .join(job.id.to_string());
        let leftovers: Vec<_> = std::fs::read_dir(&staged)
            .map(|entries| entries.filter_map(|entry| entry.ok()).collect())
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "the packed archive was left in {} — every published artifact would be stored twice",
            staged.display()
        );
        assert!(
            rg_core::deletion_recovery::journalled_deletion_ids(&storage)
                .await
                .unwrap()
                .is_empty(),
            "a committed embedded-runner publication left recovery intent behind"
        );
    }

    /// The embedded runner must refuse before its final blob write when the
    /// durable intent cannot be recorded. If this producer bypasses the shared
    /// publication boundary, it creates an artifact and this test fails.
    #[tokio::test]
    async fn embedded_artifact_publication_refuses_an_unwritable_journal() {
        let (temp, sha, db, user, repo, repo_path) = workflow_fixture(WORKFLOW).await;
        let pipeline_id = trigger_graph(&db, temp.path(), &repo_path, repo.id, &sha, user.id).await;
        let job = only_job(&db, pipeline_id).await;

        std::fs::create_dir_all(temp.path().join("_deleted")).unwrap();
        std::fs::write(temp.path().join("_deleted/journal"), b"not a directory").unwrap();

        let mut runner =
            PipelineRunner::new_local_only(db.clone(), temp.path(), &repo_path, pipeline_id);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();

        let settled = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            settled.status, "success",
            "artifact publication failure must not rewrite the script result"
        );
        assert!(
            settled
                .log
                .unwrap_or_default()
                .contains("was not published"),
            "the job log did not expose the refused artifact publication"
        );
        assert!(
            rg_db::ops::artifact_ops::list_by_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .is_empty(),
            "the embedded runner published metadata without durable recovery intent"
        );
        let artifact_dir = temp
            .path()
            .join("artifacts")
            .join("jobs")
            .join(job.id.to_string());
        assert!(
            std::fs::read_dir(&artifact_dir).is_err(),
            "the embedded runner wrote final artifact bytes without durable recovery intent"
        );
    }

    /// A job that fails publishes nothing, and a declaration whose paths the
    /// build never produced says so in the log instead of publishing an empty
    /// archive.
    #[tokio::test]
    async fn a_declaration_that_matched_nothing_publishes_nothing_and_says_so() {
        const MISSING: &[u8] = b"name: build\non: [push]\njobs:\n  build:\n    steps:\n      - run: echo built nothing\n      - uses: actions/upload-artifact@v4\n        with:\n          name: build-report\n          path: out/report.txt\n";
        let (temp, sha, db, user, repo, repo_path) = workflow_fixture(MISSING).await;
        let pipeline_id = trigger_graph(&db, temp.path(), &repo_path, repo.id, &sha, user.id).await;
        let job = only_job(&db, pipeline_id).await;

        let mut runner =
            PipelineRunner::new_local_only(db.clone(), temp.path(), &repo_path, pipeline_id);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();

        let settled = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            settled.status, "success",
            "a missing artifact is not a failed build; the script's exit code is the answer"
        );
        let log = settled.log.unwrap_or_default();
        assert!(
            log.contains("was not published"),
            "the person who has to act on a missing artifact reads the job log: {log}"
        );
        assert!(
            rg_db::ops::artifact_ops::list_by_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .is_empty(),
            "an empty archive published under the name a real run uses is worse than none"
        );
    }
}

/// card_372a240bf59d: `on: schedule` driven end to end — the real engine reading
/// real workflow files, the real tick ledger, a held clock.
#[cfg(test)]
mod schedule_pass_tests {
    use super::*;

    const OWNER: &str = "schedule-owner";
    const REPO: &str = "nightly";

    const NIGHTLY: &[u8] = b"name: Nightly
on:
  schedule:
    - cron: '0 3 * * *'
jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - run: echo audit
";

    struct Fixture {
        root: tempfile::TempDir,
        db: sea_orm::DatabaseConnection,
        repo_id: i64,
    }

    impl Fixture {
        fn repo_path(&self) -> std::path::PathBuf {
            self.root.path().join(format!("{OWNER}/{REPO}.git"))
        }

        /// The working copy commits are made in; the repository the server
        /// reads is the bare one under the storage root, pushed to.
        fn work_path(&self) -> std::path::PathBuf {
            self.root.path().join("work")
        }

        fn git(&self, args: &[&str]) -> String {
            let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
            let output = git.run(args, Some(&self.work_path())).unwrap();
            assert!(output.success(), "git {args:?}: {}", output.stdout_str());
            output.stdout_str().trim().to_owned()
        }

        /// Write `files` (an empty body removes the file), commit on `main` and
        /// push it to the repository the server reads.
        fn commit(&self, files: &[(&str, &[u8])]) -> String {
            for (path, contents) in files {
                let target = self.work_path().join(path);
                if contents.is_empty() {
                    std::fs::remove_file(target).unwrap();
                } else {
                    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                    std::fs::write(target, contents).unwrap();
                }
            }
            self.git(&["add", "-A"]);
            self.git(&["commit", "-m", "change"]);
            let bare = self.repo_path();
            self.git(&["push", "--quiet", bare.to_str().unwrap(), "main"]);
            self.git(&["rev-parse", "HEAD"])
        }

        fn context(&self) -> rg_core::push_hooks::PostPushContext {
            rg_core::push_hooks::PostPushContext {
                repo_root: self.root.path().to_path_buf(),
                docker_enabled: false,
                // Pipelines are published and left for a runner, so the test
                // reads the graph the tick produced rather than racing one.
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret".into()),
                encryption_key: Some("secret".into()),
                smtp_config: None,
                ci_engine: std::sync::Arc::new(CiEngine::new()),
                external_url: None,
                notifier: None,
                delivery_tracker: rg_core::task_tracker::delivery_tracker().clone(),
                write_pool: None,
                storage_limits: rg_core::storage_quota::StorageLimits::default(),
            }
        }

        async fn pass(
            &self,
            cache: &mut rg_core::ci::schedule::ScheduleCache,
            config: rg_core::ci::schedule::ScheduleConfig,
            now: &str,
        ) -> Vec<rg_core::ci::schedule::ScheduledPipeline> {
            let now = chrono::DateTime::parse_from_rfc3339(now)
                .unwrap()
                .with_timezone(&chrono::Utc);
            rg_core::ci::schedule::run_pass(&self.db, &self.context(), config, cache, now)
                .await
                .expect("the schedule pass runs")
        }

        async fn pipelines(&self) -> Vec<rg_db::entities::pipeline::Model> {
            use sea_orm::{EntityTrait, QueryOrder};
            rg_db::entities::pipeline::Entity::find()
                .order_by_asc(rg_db::entities::pipeline::Column::Id)
                .all(&self.db)
                .await
                .unwrap()
        }

        async fn job_names(&self, pipeline_id: i64) -> Vec<String> {
            let mut names = Vec::new();
            for stage in rg_db::ops::pipeline_ops::list_stages_by_pipeline(&self.db, pipeline_id)
                .await
                .unwrap()
            {
                for job in rg_db::ops::pipeline_ops::list_jobs_by_stage(&self.db, stage.id)
                    .await
                    .unwrap()
                {
                    names.push(job.name);
                }
            }
            names.sort();
            names
        }
    }

    async fn fixture(files: &[(&str, &[u8])]) -> Fixture {
        use sea_orm::ActiveValue::{NotSet, Set};

        let root = tempfile::tempdir().unwrap();
        let repo_path = root.path().join(format!("{OWNER}/{REPO}.git"));
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::create_dir_all(root.path().join("work")).unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git
            .run(
                &["init", "--bare", "--quiet", "-b", "main"],
                Some(&repo_path)
            )
            .unwrap()
            .success());
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                root.path().join("schedule.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            OWNER,
            "schedule-owner@example.com",
            "unused",
            "Schedule Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set(REPO.into()),
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
        .unwrap();
        let fixture = Fixture {
            root,
            db,
            repo_id: repo.id,
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "CI"]);
        fixture.git(&["config", "user.email", "ci@example.com"]);
        fixture.commit(files);
        fixture
    }

    fn default_config() -> rg_core::ci::schedule::ScheduleConfig {
        rg_core::ci::schedule::ScheduleConfig::default()
    }

    /// The acceptance of the card: a tick runs exactly one pipeline, and the
    /// same tick asked again — by the same process, by a restarted one with an
    /// empty cache, by a second replica — runs nothing.
    #[tokio::test]
    async fn a_tick_runs_one_pipeline_and_its_repeat_runs_none() {
        let fixture = fixture(&[(".gitea/workflows/nightly.yml", NIGHTLY)]).await;
        let mut cache = rg_core::ci::schedule::ScheduleCache::default();

        // First sight: the entry is recorded, the tick that passed before it was
        // seen is not run.
        assert!(fixture
            .pass(&mut cache, default_config(), "2026-10-20T02:00:00Z")
            .await
            .is_empty());
        assert!(fixture.pipelines().await.is_empty());

        let ran = fixture
            .pass(&mut cache, default_config(), "2026-10-20T03:00:30Z")
            .await;
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert_eq!(ran[0].workflow, "nightly.yml");
        assert_eq!(ran[0].cron, "0 3 * * *");
        assert_eq!(ran[0].tick.to_rfc3339(), "2026-10-20T03:00:00+00:00");

        // The same minute again, and the same tick from a process that never
        // saw it (a restart, or a second replica).
        assert!(fixture
            .pass(&mut cache, default_config(), "2026-10-20T03:00:50Z")
            .await
            .is_empty());
        let mut replica = rg_core::ci::schedule::ScheduleCache::default();
        assert!(fixture
            .pass(&mut replica, default_config(), "2026-10-20T03:59:00Z")
            .await
            .is_empty());

        let pipelines = fixture.pipelines().await;
        assert_eq!(pipelines.len(), 1, "one tick, one pipeline: {pipelines:?}");
        let pipeline = &pipelines[0];
        assert_eq!(Some(pipeline.id), ran[0].pipeline_id);
        assert_eq!(pipeline.trigger_type, "schedule");
        assert_eq!(pipeline.ref_name, "refs/heads/main");
        assert_eq!(pipeline.schedule_workflow.as_deref(), Some("nightly.yml"));
        assert_eq!(pipeline.schedule_cron.as_deref(), Some("0 3 * * *"));
        assert_eq!(fixture.job_names(pipeline.id).await, ["nightly/audit"]);
    }

    /// Six ticks missed while the server was down are one run when it comes
    /// back, not six.
    #[tokio::test]
    async fn ticks_missed_while_down_are_not_caught_up() {
        let fixture = fixture(&[(".gitea/workflows/nightly.yml", NIGHTLY)]).await;
        let mut cache = rg_core::ci::schedule::ScheduleCache::default();
        fixture
            .pass(&mut cache, default_config(), "2026-10-14T12:00:00Z")
            .await;
        let ran = fixture
            .pass(&mut cache, default_config(), "2026-10-20T12:00:00Z")
            .await;
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert_eq!(ran[0].tick.to_rfc3339(), "2026-10-20T03:00:00+00:00");
        assert_eq!(fixture.pipelines().await.len(), 1);
    }

    /// `* * * * *` on a default instance runs every fifth minute.
    #[tokio::test]
    async fn no_entry_runs_more_often_than_the_floor() {
        let fixture = fixture(&[(
            ".gitea/workflows/often.yml",
            b"on:\n  schedule:\n    - cron: '* * * * *'\njobs:\n  ping:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ping\n",
        )])
        .await;
        let mut cache = rg_core::ci::schedule::ScheduleCache::default();
        let mut ran_at = Vec::new();
        for minute in 0..=11 {
            let now = format!("2026-10-20T10:{minute:02}:05Z");
            if !fixture
                .pass(&mut cache, default_config(), &now)
                .await
                .is_empty()
            {
                ran_at.push(minute);
            }
        }
        assert_eq!(
            ran_at,
            [5, 10],
            "first sight at :00, then every fifth minute"
        );

        let one_minute = rg_core::ci::schedule::ScheduleConfig {
            min_interval_minutes: 1,
        };
        assert_eq!(
            fixture
                .pass(&mut cache, one_minute, "2026-10-20T10:12:05Z")
                .await
                .len(),
            1,
            "a one-minute floor lets the 10:12 tick run two minutes after 10:10"
        );
    }

    /// A tick belongs to one entry of one file: a second workflow on the same
    /// cron is its own run, and neither run carries the other's jobs — nor the
    /// jobs of a workflow that is not scheduled at all.
    #[tokio::test]
    async fn each_scheduled_workflow_is_its_own_run() {
        let fixture = fixture(&[
            (".gitea/workflows/nightly.yml", NIGHTLY),
            (
                ".gitea/workflows/backup.yml",
                b"on:\n  schedule:\n    - cron: '0 3 * * *'\njobs:\n  dump:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo dump\n",
            ),
            (
                ".gitea/workflows/ci.yml",
                b"on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo test\n",
            ),
        ])
        .await;
        let mut cache = rg_core::ci::schedule::ScheduleCache::default();
        fixture
            .pass(&mut cache, default_config(), "2026-10-20T02:00:00Z")
            .await;
        let ran = fixture
            .pass(&mut cache, default_config(), "2026-10-20T03:00:00Z")
            .await;
        assert_eq!(ran.len(), 2, "{ran:?}");
        let mut jobs = Vec::new();
        for pipeline in fixture.pipelines().await {
            jobs.push(fixture.job_names(pipeline.id).await);
        }
        jobs.sort();
        assert_eq!(jobs, [vec!["backup/dump"], vec!["nightly/audit"]]);
    }

    /// The default branch is the schedule. A push that removes an entry stops
    /// it at once, and putting it back starts over — it does not run straight
    /// away for a tick that passed while it was gone.
    #[tokio::test]
    async fn a_push_to_the_default_branch_reschedules() {
        let fixture = fixture(&[(".gitea/workflows/nightly.yml", NIGHTLY)]).await;
        let mut cache = rg_core::ci::schedule::ScheduleCache::default();
        fixture
            .pass(&mut cache, default_config(), "2026-10-20T02:00:00Z")
            .await;

        fixture.commit(&[(".gitea/workflows/nightly.yml", b"")]);
        assert!(fixture
            .pass(&mut cache, default_config(), "2026-10-20T03:00:00Z")
            .await
            .is_empty());
        assert!(
            rg_db::ops::ci_schedule_ops::list_for_repo(&fixture.db, fixture.repo_id)
                .await
                .unwrap()
                .is_empty(),
            "an entry the branch no longer declares leaves the ledger"
        );

        fixture.commit(&[(".gitea/workflows/nightly.yml", NIGHTLY)]);
        assert!(fixture
            .pass(&mut cache, default_config(), "2026-10-21T02:00:00Z")
            .await
            .is_empty());
        assert_eq!(
            fixture
                .pass(&mut cache, default_config(), "2026-10-21T03:00:00Z")
                .await
                .len(),
            1
        );
    }

    /// A pull mirror's workflows are its upstream's: the mirror pass runs no
    /// `push` pipeline for them, and a schedule must not run them either.
    #[tokio::test]
    async fn a_pull_mirror_is_not_scheduled() {
        use sea_orm::ActiveValue::{NotSet, Set};

        let fixture = fixture(&[(".gitea/workflows/nightly.yml", NIGHTLY)]).await;
        let now = chrono::Utc::now();
        rg_db::ops::mirror_ops::create(
            &fixture.db,
            rg_db::entities::mirror::ActiveModel {
                id: NotSet,
                repo_id: Set(fixture.repo_id),
                url: Set("https://example.invalid/upstream.git".into()),
                username: Set(None),
                password_encrypted: Set(None),
                sync_interval_seconds: Set(3600),
                next_sync_at: Set(None),
                last_sync_at: Set(None),
                last_sync_error: Set(None),
                status: Set("active".into()),
                created_at: Set(now),
                updated_at: Set(now),
            },
        )
        .await
        .unwrap();
        let mut cache = rg_core::ci::schedule::ScheduleCache::default();
        for now in ["2026-10-20T02:00:00Z", "2026-10-20T03:00:00Z"] {
            assert!(fixture
                .pass(&mut cache, default_config(), now)
                .await
                .is_empty());
        }
        assert!(fixture.pipelines().await.is_empty());
        assert!(
            rg_db::ops::ci_schedule_ops::list_for_repo(&fixture.db, fixture.repo_id)
                .await
                .unwrap()
                .is_empty(),
            "a mirror is not even read for schedules"
        );
    }

    /// A tick whose run cannot be built leaves a failed `schedule` pipeline
    /// that says why — and that a retry can replay, because the row names the
    /// entry. Anything less is a nightly that silently stopped.
    #[tokio::test]
    async fn a_refused_run_is_recorded_where_ci_is_read() {
        let fixture = fixture(&[(
            ".gitea/workflows/nightly.yml",
            b"on:\n  schedule:\n    - cron: '0 3 * * *'\njobs:\n  audit:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: someone/else@v1\n",
        )])
        .await;
        let mut cache = rg_core::ci::schedule::ScheduleCache::default();
        fixture
            .pass(&mut cache, default_config(), "2026-10-20T02:00:00Z")
            .await;
        let ran = fixture
            .pass(&mut cache, default_config(), "2026-10-20T03:00:00Z")
            .await;
        assert_eq!(ran.len(), 1, "{ran:?}");
        let pipelines = fixture.pipelines().await;
        assert_eq!(pipelines.len(), 1);
        assert_eq!(pipelines[0].status, "failed");
        assert_eq!(pipelines[0].trigger_type, "schedule");
        assert_eq!(
            pipelines[0].schedule_workflow.as_deref(),
            Some("nightly.yml")
        );
        assert_eq!(pipelines[0].schedule_cron.as_deref(), Some("0 3 * * *"));
    }

    /// The trigger half of a retry: the recorded entry selects its file again,
    /// and a scheduled run that names no entry — or one the commit does not
    /// declare — is refused rather than falling through to the native config,
    /// which nobody scheduled.
    #[test]
    fn a_scheduled_run_selects_its_entry_and_nothing_else() {
        let (temp, sha) = super::matrix_tests::commit_repo(&[
            (".gitea/workflows/nightly.yml", NIGHTLY),
            (
                ".plombir-git-ci.yml",
                b"native:\n  script:\n    - echo native\n",
            ),
        ]);
        let read = |schedule: Option<rg_core::ci::ScheduledRun<'_>>| {
            read_ci_config_with_inputs(
                temp.path(),
                RepositoryName {
                    owner: "owner",
                    name: "repo",
                },
                &sha,
                "refs/heads/main",
                WorkflowInvocation {
                    event: "schedule",
                    base_branch: None,
                    head_branch: None,
                    previous_sha: None,
                    inputs: None,
                    schedule,
                    actions: None,
                    vars: None,
                },
            )
        };
        let config = read(Some(rg_core::ci::ScheduledRun {
            workflow: "nightly.yml",
            cron: "0 3 * * *",
        }))
        .expect("the recorded entry selects its workflow");
        assert_eq!(config.jobs.keys().collect::<Vec<_>>(), ["nightly/audit"]);
        for schedule in [
            None,
            Some(rg_core::ci::ScheduledRun {
                workflow: "nightly.yml",
                cron: "0 4 * * *",
            }),
            Some(rg_core::ci::ScheduledRun {
                workflow: "other.yml",
                cron: "0 3 * * *",
            }),
        ] {
            let error = read(schedule).expect_err("no entry, no run — and no native fallback");
            assert!(
                format!("{error:#}").contains("does not declare"),
                "{schedule:?}: {error:#}"
            );
        }
    }
}

/// card_c8b3fa9f0ebc: a job translated from an Actions workflow runs its steps
/// one by one in the embedded runner, and each one is recorded on its row.
#[cfg(test)]
mod step_model_tests {
    use super::*;
    use sea_orm::{NotSet, Set};

    async fn run_workflow(workflow: &str) -> (rg_db::DatabaseConnection, i64, tempfile::TempDir) {
        let (db, pipeline_id, temp) = run_pipeline(workflow).await;
        let job = rg_db::ops::pipeline_ops::list_stages_by_pipeline(&db, pipeline_id)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let job = rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, job.id)
            .await
            .unwrap()
            .remove(0);
        (db, job.id, temp)
    }

    /// Every job of the pipeline the workflow publishes, by name, once the
    /// embedded runner ran it.
    async fn run_pipeline_jobs(
        workflow: &str,
    ) -> (
        rg_db::DatabaseConnection,
        std::collections::HashMap<String, rg_db::entities::pipeline_job::Model>,
        tempfile::TempDir,
    ) {
        let (db, pipeline_id, temp) = run_pipeline(workflow).await;
        let mut jobs = std::collections::HashMap::new();
        for stage in rg_db::ops::pipeline_ops::list_stages_by_pipeline(&db, pipeline_id)
            .await
            .unwrap()
        {
            for job in rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, stage.id)
                .await
                .unwrap()
            {
                jobs.insert(job.name.clone(), job);
            }
        }
        (db, jobs, temp)
    }

    async fn run_pipeline(workflow: &str) -> (rg_db::DatabaseConnection, i64, tempfile::TempDir) {
        let (db, pipeline_id, repo_id, temp) = publish_pipeline(workflow).await;
        let repo_path = temp.path().join("step-owner/steps");
        let mut runner =
            PipelineRunner::new_local_only(db.clone(), temp.path(), &repo_path, pipeline_id);
        runner.set_repo_id(repo_id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();
        (db, pipeline_id, temp)
    }

    /// The pipeline the workflow publishes, not yet run: (db, pipeline, repo).
    async fn publish_pipeline(
        workflow: &str,
    ) -> (rg_db::DatabaseConnection, i64, i64, tempfile::TempDir) {
        publish_pipeline_with_environments(workflow, &[]).await
    }

    /// [`publish_pipeline`] in a repository that has these environments —
    /// `(name, protected)` — before the workflow is read.
    async fn publish_pipeline_with_environments(
        workflow: &str,
        environments: &[(&str, bool)],
    ) -> (rg_db::DatabaseConnection, i64, i64, tempfile::TempDir) {
        publish_pipeline_on(workflow, environments, "push", None)
            .await
            .expect("the workflow publishes a pipeline")
    }

    /// [`publish_pipeline_with_environments`] for `event`, with the
    /// `workflow_dispatch` inputs a manual run was given; the refusal when the
    /// workflow is not one this engine runs.
    async fn publish_pipeline_on(
        workflow: &str,
        environments: &[(&str, bool)],
        event: &str,
        inputs: Option<&[(&str, &str)]>,
    ) -> Result<(rg_db::DatabaseConnection, i64, i64, tempfile::TempDir)> {
        publish_pipeline_for(workflow, environments, TestEvent::on(event, inputs)).await
    }

    /// The event a test pipeline is published for: `event` on `refs/heads/main`
    /// ([`TestEvent::on`]), or a pull request ([`TestEvent::pull_request`]).
    #[derive(Clone, Copy, Debug)]
    struct TestEvent<'a> {
        event: &'a str,
        ref_name: &'a str,
        base_branch: Option<&'a str>,
        head_branch: Option<&'a str>,
        inputs: Option<&'a [(&'a str, &'a str)]>,
    }

    impl<'a> TestEvent<'a> {
        fn on(event: &'a str, inputs: Option<&'a [(&'a str, &'a str)]>) -> Self {
            Self {
                event,
                ref_name: "refs/heads/main",
                base_branch: None,
                head_branch: None,
                inputs,
            }
        }

        /// Pull request #1, from `head` into `base`, as
        /// `rg_core::pull_request::ci` triggers it.
        fn pull_request(head: &'a str, base: &'a str) -> Self {
            Self {
                event: rg_core::pull_request::PULL_REQUEST_EVENT,
                ref_name: "refs/pull/1/head",
                base_branch: Some(base),
                head_branch: Some(head),
                inputs: None,
            }
        }
    }

    async fn publish_pipeline_for(
        workflow: &str,
        environments: &[(&str, bool)],
        event: TestEvent<'_>,
    ) -> Result<(rg_db::DatabaseConnection, i64, i64, tempfile::TempDir)> {
        publish_workflows_for(&[("build.yml", workflow)], environments, event).await
    }

    /// [`publish_pipeline_for`] over several files of `.gitea/workflows`.
    async fn publish_workflows_for(
        workflows: &[(&str, &str)],
        environments: &[(&str, bool)],
        event: TestEvent<'_>,
    ) -> Result<(rg_db::DatabaseConnection, i64, i64, tempfile::TempDir)> {
        let inputs = event.inputs.map(|inputs| {
            inputs
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect::<std::collections::HashMap<_, _>>()
        });
        let temp = tempfile::tempdir().unwrap();
        let repo_path = temp.path().join("step-owner/steps");
        std::fs::create_dir_all(repo_path.join(".gitea/workflows")).unwrap();
        for (file, workflow) in workflows {
            std::fs::write(repo_path.join(".gitea/workflows").join(file), workflow).unwrap();
        }
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.name", "CI"],
            vec!["config", "user.email", "ci@example.com"],
            vec!["add", "-A"],
            vec!["commit", "-m", "workflow"],
        ] {
            assert!(git.run(&args, Some(&repo_path)).unwrap().success());
        }
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(&repo_path))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("steps.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "step-owner",
            "step-owner@example.com",
            "unused",
            "Step Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("steps".into()),
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
        .unwrap();
        for (name, protected) in environments {
            rg_db::ops::ci_environment_ops::create_with_approvers(
                &db,
                rg_db::entities::ci_environment::ActiveModel {
                    id: NotSet,
                    repo_id: Set(repo.id),
                    name: Set((*name).to_string()),
                    protected: Set(*protected),
                    required_approvals: Set(1),
                    allowed_approver_ids: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                },
                Vec::new(),
            )
            .await
            .unwrap();
        }
        let pipeline_id = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_root: temp.path(),
                repo_path: &repo_path,
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: event.ref_name,
                trigger_type: event.event,
                base_branch: event.base_branch,
                head_branch: event.head_branch,
                previous_sha: None,
                inputs: inputs.as_ref(),
                schedule: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                // Published only: the runner below is driven and awaited.
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
            },
            &CiNotifications::default(),
        )
        .await?;
        Ok((db, pipeline_id, repo.id, temp))
    }

    /// Every job of the pipeline `workflow` publishes for `event`, by name,
    /// once the embedded runner ran it — and the pipeline's own status.
    async fn run_pipeline_on(
        workflow: &str,
        event: &str,
        inputs: Option<&[(&str, &str)]>,
    ) -> (
        std::collections::HashMap<String, rg_db::entities::pipeline_job::Model>,
        String,
        rg_db::DatabaseConnection,
        tempfile::TempDir,
    ) {
        run_pipeline_for(workflow, TestEvent::on(event, inputs)).await
    }

    async fn run_pipeline_for(
        workflow: &str,
        event: TestEvent<'_>,
    ) -> (
        std::collections::HashMap<String, rg_db::entities::pipeline_job::Model>,
        String,
        rg_db::DatabaseConnection,
        tempfile::TempDir,
    ) {
        let (jobs, pipeline, db, temp) = run_workflows_for(&[("build.yml", workflow)], event).await;
        (jobs, pipeline.status, db, temp)
    }

    /// [`run_pipeline_for`] over several files of `.gitea/workflows`, answering
    /// with the whole pipeline row.
    async fn run_workflows_for(
        workflows: &[(&str, &str)],
        event: TestEvent<'_>,
    ) -> (
        std::collections::HashMap<String, rg_db::entities::pipeline_job::Model>,
        rg_db::entities::pipeline::Model,
        rg_db::DatabaseConnection,
        tempfile::TempDir,
    ) {
        let (db, pipeline_id, repo_id, temp) = publish_workflows_for(workflows, &[], event)
            .await
            .expect("the workflow publishes a pipeline");
        let repo_path = temp.path().join("step-owner/steps");
        let mut runner =
            PipelineRunner::new_local_only(db.clone(), temp.path(), &repo_path, pipeline_id);
        runner.set_repo_id(repo_id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap()
            .into_iter()
            .map(|job| (job.name.clone(), job))
            .collect();
        let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
            .await
            .unwrap()
            .unwrap();
        (jobs, pipeline, db, temp)
    }

    async fn step_rows(
        db: &rg_db::DatabaseConnection,
        job_id: i64,
    ) -> Vec<(String, String, Option<String>, Option<i32>)> {
        let mut rows = Vec::new();
        for number in 1..=10 {
            let Some(step) = rg_db::ops::pipeline_step_ops::find_job_step(db, job_id, number)
                .await
                .unwrap()
            else {
                break;
            };
            rows.push((step.name, step.status, step.conclusion, step.exit_code));
        }
        rows
    }

    const STEPS: &str = "on: push\njobs:\n  build:\n    steps:\n      - name: One\n        run: echo one\n      - name: Two\n        run: echo two; exit 3\nCONTINUE      - name: Three\n        run: echo three\n";

    /// The card's acceptance, through the embedded runner: step 2 fails under
    /// `continue-on-error: true`, step 3 runs and the job is green; without
    /// the flag step 3 is skipped and the job is red.
    #[tokio::test]
    async fn continue_on_error_decides_the_job_in_the_embedded_runner() {
        let (db, job_id, _temp) =
            run_workflow(&STEPS.replace("CONTINUE", "        continue-on-error: true\n")).await;
        let job = rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.status, "success", "{:?}", job.log);
        assert_eq!(
            step_rows(&db, job_id).await,
            [
                (
                    "One".into(),
                    "success".into(),
                    Some("success".into()),
                    Some(0)
                ),
                (
                    "Two".into(),
                    "failure".into(),
                    Some("success".into()),
                    Some(3)
                ),
                (
                    "Three".into(),
                    "success".into(),
                    Some("success".into()),
                    Some(0)
                ),
            ]
        );
        let three = rg_db::ops::pipeline_step_ops::find_job_step(&db, job_id, 3)
            .await
            .unwrap()
            .unwrap();
        assert!(three.log.unwrap_or_default().contains("three"));
        assert!(three.started_at.is_some() && three.finished_at.is_some());

        let (db, job_id, _temp) = run_workflow(&STEPS.replace("CONTINUE", "")).await;
        let job = rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.status, "failed");
        assert_eq!(job.exit_code, Some(3));
        assert_eq!(
            step_rows(&db, job_id).await,
            [
                (
                    "One".into(),
                    "success".into(),
                    Some("success".into()),
                    Some(0)
                ),
                (
                    "Two".into(),
                    "failure".into(),
                    Some("failure".into()),
                    Some(3)
                ),
                (
                    "Three".into(),
                    "skipped".into(),
                    Some("skipped".into()),
                    None
                ),
            ]
        );
        let log = job.log.unwrap_or_default();
        assert!(log.contains("▶ step 3/3: Three — skipped"), "{log}");
    }

    /// card_5013aa8fca9e, through the embedded runner: job A writes
    /// `version=1.2.3` to `$GITHUB_OUTPUT`, and job B, which needs A, reads it
    /// as `${{ needs.A.outputs.version }}` — in a step, in a job variable and
    /// in a step condition.
    #[tokio::test]
    async fn a_needed_jobs_outputs_reach_the_job_that_needs_it() {
        let (db, jobs, _temp) = run_pipeline_jobs(
            "on: push\njobs:\n  A:\n    outputs:\n      version: ${{ steps.v.outputs.version }}\n    steps:\n      - id: v\n        run: echo version=1.2.3 >> \"$GITHUB_OUTPUT\"\n  B:\n    needs: A\n    env:\n      FROM_JOB: ${{ needs.A.outputs.version }}\n    steps:\n      - run: echo \"step=${{ needs.A.outputs.version }} job=$FROM_JOB result=${{ needs.A.result }}\"\n      - run: echo gated-in\n        if: needs.A.outputs.version == '1.2.3'\n      - run: echo never\n        if: needs.A.outputs.version == '9.9.9'\n",
        )
        .await;
        // A workflow's jobs are named after its file.
        let a = &jobs["build/A"];
        assert_eq!(a.status, "success", "{:?}", a.log);
        assert_eq!(a.outputs.as_deref(), Some(r#"{"version":"1.2.3"}"#));
        let b = &jobs["build/B"];
        assert_eq!(b.status, "success", "{:?}", b.log);
        let first = rg_db::ops::pipeline_step_ops::find_job_step(&db, b.id, 1)
            .await
            .unwrap()
            .unwrap();
        assert!(
            first
                .log
                .unwrap_or_default()
                .contains("step=1.2.3 job=1.2.3 result=success"),
            "{:?}",
            b.log
        );
        let rows = step_rows(&db, b.id).await;
        assert_eq!(rows[1].1, "success", "{rows:?}");
        assert_eq!(rows[2].1, "skipped", "{rows:?}");
    }

    /// card_8a48a54066a7's acceptance, through the embedded runner: the
    /// paths-filter pattern. A job's `if:` reads what the job it needs left —
    /// an output, or its result — once that job has finished; a false one
    /// skips the job, and the job that needs the skipped one is skipped too,
    /// as on GitHub. Nothing of that fails the pipeline.
    #[tokio::test]
    async fn a_job_condition_reads_what_a_needed_job_left() {
        let (db, jobs, _temp) = run_pipeline_jobs(
            r#"on: push
jobs:
  changes:
    outputs:
      backend: ${{ steps.filter.outputs.backend }}
      frontend: ${{ steps.filter.outputs.frontend }}
    steps:
      - id: filter
        run: |
          echo backend=true >> "$GITHUB_OUTPUT"
          echo frontend=false >> "$GITHUB_OUTPUT"
  backend:
    needs: changes
    if: needs.changes.outputs.backend == 'true'
    steps:
      - run: echo backend
  frontend:
    needs: changes
    if: ${{ needs.changes.outputs.frontend == 'true' }}
    steps:
      - run: echo frontend
  frontend-e2e:
    needs: frontend
    steps:
      - run: echo e2e
  report:
    needs: [changes, backend]
    if: needs.changes.result == 'success' && needs.backend.result == 'success'
    steps:
      - run: echo report
"#,
        )
        .await;
        for (name, status) in [
            ("build/changes", "success"),
            ("build/backend", "success"),
            ("build/frontend", "skipped"),
            ("build/frontend-e2e", "skipped"),
            ("build/report", "success"),
        ] {
            assert_eq!(jobs[name].status, status, "{name}: {:?}", jobs[name].log);
            assert!(
                jobs[name].deferred_condition.is_none(),
                "{name}: a decided job holds nothing"
            );
        }
        assert!(
            jobs["build/frontend"].log.as_deref().is_some_and(|log| log
                .contains("needs.changes.outputs.frontend == 'true'")
                && log.contains("is false")),
            "{:?}",
            jobs["build/frontend"].log
        );
        assert!(
            jobs["build/frontend-e2e"]
                .log
                .as_deref()
                .is_some_and(|log| log.contains("`frontend`, a job this one needs, was skipped")),
            "{:?}",
            jobs["build/frontend-e2e"].log
        );
        let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&db, jobs["build/changes"].stage_id)
            .await
            .unwrap()
            .unwrap();
        let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&db, stage.pipeline_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pipeline.status, "success");
    }

    /// A job skipped by an `if:` answered when the graph was written skips
    /// the job that needs it. It used to run it: a skipped job does not fail
    /// its stage, and the next stage was reached as if it had succeeded.
    #[tokio::test]
    async fn a_job_that_needs_a_skipped_job_is_skipped() {
        let (_db, jobs, _temp) = run_pipeline_jobs(
            "on: push\njobs:\n  release:\n    if: github.ref == 'refs/heads/release'\n    steps:\n      - run: echo release\n  announce:\n    needs: release\n    steps:\n      - run: echo announce\n  lint:\n    steps:\n      - run: echo lint\n  after-lint:\n    needs: lint\n    steps:\n      - run: echo after\n",
        )
        .await;
        assert_eq!(jobs["build/release"].status, "skipped");
        assert_eq!(
            jobs["build/announce"].status, "skipped",
            "{:?}",
            jobs["build/announce"].log
        );
        assert_eq!(jobs["build/after-lint"].status, "success");
    }

    /// card_28ac32f9c069's workflow: a deploy gated on a `workflow_dispatch`
    /// checkbox, read as `inputs.*` and as `github.event.inputs.*`, by a job
    /// and by a step.
    const DISPATCH: &str = r#"on:
  push:
  workflow_dispatch:
    inputs:
      deploy:
        type: boolean
      target:
        type: choice
        options: [staging, "it's prod"]
        default: staging
jobs:
  deploy:
    if: inputs.deploy
    steps:
      - run: echo deploy
  by-event:
    if: github.event.inputs.deploy == 'true'
    steps:
      - run: echo by-event
  gated:
    steps:
      - name: Gated
        if: inputs.deploy == true && inputs.target == 'staging'
        run: echo gated
      - name: Quoted
        if: ${{ inputs.TARGET != 'it''s prod' }}
        run: echo quoted
"#;

    /// card_28ac32f9c069's acceptance, through the embedded runner: a ticked
    /// box runs the job and the step, an unticked one skips them, and a run
    /// that was not dispatched reads every input as empty. Each case asserts
    /// both answers, so an input missing from the condition — read as `''` —
    /// cannot pass.
    #[tokio::test]
    async fn dispatch_inputs_decide_job_and_step_conditions() {
        for (event, inputs, job_status, step_statuses) in [
            (
                rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                Some(&[("deploy", "true")][..]),
                "success",
                ["success", "success"],
            ),
            (
                rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                Some(&[("deploy", "false"), ("target", "it's prod")][..]),
                "skipped",
                ["skipped", "skipped"],
            ),
            ("push", None, "skipped", ["skipped", "success"]),
        ] {
            let (jobs, status, db, _temp) = run_pipeline_on(DISPATCH, event, inputs).await;
            assert_eq!(status, "success", "{event} {inputs:?}");
            for name in ["build/deploy", "build/by-event"] {
                assert_eq!(
                    jobs[name].status, job_status,
                    "{name} on {event} {inputs:?}: {:?}",
                    jobs[name].log
                );
            }
            let rows = step_rows(&db, jobs["build/gated"].id).await;
            assert_eq!(
                [rows[0].1.as_str(), rows[1].1.as_str()],
                step_statuses,
                "{event} {inputs:?}: {rows:?}"
            );
        }
    }

    /// An input the workflow does not declare is refused by name — in a job's
    /// `if:`, a step's, `github.event.inputs.*`, and a `run:` — rather than
    /// read as empty, which would quietly never deploy.
    #[tokio::test]
    async fn an_undeclared_input_is_refused_by_name() {
        for (job, expected) in [
            ("    if: inputs.nope\n    steps:\n      - run: echo x\n", "inputs.nope"),
            (
                "    steps:\n      - if: ${{ inputs.Deploy && inputs.nope == 'x' }}\n        run: echo x\n",
                "inputs.nope",
            ),
            (
                "    if: github.event.inputs.nope == 'true'\n    steps:\n      - run: echo x\n",
                "github.event.inputs.nope",
            ),
            (
                "    steps:\n      - run: echo ${{ inputs.nope }}\n",
                "inputs.nope",
            ),
        ] {
            let workflow = format!(
                "on:\n  workflow_dispatch:\n    inputs:\n      deploy:\n        type: boolean\njobs:\n  ship:\n{job}"
            );
            let error = publish_pipeline_on(
                &workflow,
                &[],
                rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                Some(&[("deploy", "true")]),
            )
            .await
            .map(|_| ())
            .expect_err("an undeclared input must refuse the workflow");
            let message = format!("{error:#}");
            assert!(
                message.contains(expected) && message.contains("does not declare"),
                "{message}"
            );
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "the workflow's mistake, not the server's: {message}"
            );
        }
    }

    /// The `concurrency.group` of the pipeline a workflow declaring `group`
    /// publishes for `event` — or the refusal.
    async fn concurrency_group_of(
        group: &str,
        event: &str,
        inputs: Option<&[(&str, &str)]>,
    ) -> Result<Option<String>> {
        let workflow = format!(
            "on:\n  push:\n  workflow_dispatch:\n    inputs:\n      environment:\n        \
             type: choice\n        options: [staging, production]\n      fallback:\n        \
             type: string\n      dry:\n        type: boolean\nconcurrency:\n  group: {group}\n\
             jobs:\n  ship:\n    steps:\n      - run: echo ship\n"
        );
        let (db, pipeline_id, _, _temp) =
            publish_pipeline_on(&workflow, &[], event, inputs).await?;
        Ok(rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
            .await
            .unwrap()
            .expect("the pipeline was published")
            .concurrency_group)
    }

    /// card_b06309f3cefb: one deploy per environment. Two dispatches with
    /// different inputs get different groups and the same input the same one;
    /// a push reads the input as `''`, as on GitHub; `github.event.inputs.*`, a
    /// boolean and a group that computes its value read the same inputs.
    #[tokio::test]
    async fn dispatch_inputs_name_the_concurrency_group() {
        let dispatch = rg_core::ci::WORKFLOW_DISPATCH_EVENT;
        for (group, event, inputs, expected) in [
            (
                "deploy-${{ inputs.environment }}",
                dispatch,
                Some(&[("environment", "staging")][..]),
                "deploy-staging",
            ),
            (
                "deploy-${{ inputs.environment }}",
                dispatch,
                Some(&[("environment", "production")][..]),
                "deploy-production",
            ),
            ("deploy-${{ inputs.environment }}", "push", None, "deploy-"),
            (
                "deploy-${{inputs.ENVIRONMENT}}",
                dispatch,
                Some(&[("environment", "production")][..]),
                "deploy-production",
            ),
            (
                "deploy-${{ github.event.inputs.environment }}",
                dispatch,
                Some(&[("environment", "production")][..]),
                "deploy-production",
            ),
            (
                "deploy-${{ inputs.fallback || 'staging' }}",
                dispatch,
                Some(&[("environment", "production")][..]),
                "deploy-staging",
            ),
            (
                "deploy-${{ inputs.fallback || 'staging' }}",
                dispatch,
                Some(&[("environment", "production"), ("fallback", "it's qa")][..]),
                "deploy-it's qa",
            ),
            (
                "${{ inputs.dry }}-${{ inputs.environment }}",
                dispatch,
                Some(&[("environment", "staging"), ("dry", "true")][..]),
                "true-staging",
            ),
        ] {
            let resolved = concurrency_group_of(group, event, inputs)
                .await
                .unwrap_or_else(|error| panic!("{group} on {event} {inputs:?}: {error:#}"));
            assert_eq!(
                resolved.as_deref(),
                Some(expected),
                "{group} on {event} {inputs:?}"
            );
        }
    }

    /// An input the workflow does not declare is refused by name in a group as
    /// it is in `if:` — on GitHub it reads as `''`, and every deploy would
    /// share one group — and so is an input that would turn into an expression.
    #[tokio::test]
    async fn a_concurrency_group_refuses_an_undeclared_or_expression_input() {
        for (group, inputs, expected) in [
            (
                "deploy-${{ inputs.enviroment }}",
                &[("environment", "staging")][..],
                "concurrency.group: reads inputs.enviroment, which this workflow does not declare",
            ),
            (
                "deploy-${{ github.event.inputs.nope }}",
                &[("environment", "staging")][..],
                "concurrency.group: reads github.event.inputs.nope, which workflow_dispatch does \
                 not declare",
            ),
            // Refused for the whole workflow, before any field reads it.
            (
                "deploy-${{ inputs.fallback }}",
                &[("fallback", "${{ github.sha }}")][..],
                "workflow_dispatch input 'fallback' holds `${{`",
            ),
        ] {
            let error =
                concurrency_group_of(group, rg_core::ci::WORKFLOW_DISPATCH_EVENT, Some(inputs))
                    .await
                    .expect_err("the group must be refused");
            let message = format!("{error:#}");
            assert!(message.contains(expected), "{group}: {message}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "the workflow's mistake, not the server's: {message}"
            );
        }
    }

    /// card_af3833480552: the workflow GitHub's documentation gives for
    /// cancelling superseded PR runs, plus a job that runs only for a pull
    /// request into `main` and a step that runs only for one from `feature`.
    const PULL_REQUEST_REFS: &str = "on: [push, pull_request]\nconcurrency:\n  group: ${{ github.workflow }}-${{ github.head_ref || github.ref }}\n  cancel-in-progress: true\njobs:\n  print:\n    steps:\n      - run: echo \"head=[${{ github.head_ref }}] base=[${{ github.base_ref }}]\"\n  into-main:\n    if: github.base_ref == 'main'\n    steps:\n      - run: echo into-main\n  from-feature:\n    steps:\n      - if: ${{ github.head_ref == 'feature' }}\n        run: echo from-feature\n";

    /// card_af3833480552's acceptance, through the embedded runner: a pull
    /// request is grouped by its source branch and a push by its ref; a job's
    /// `if:` on `github.base_ref` tells a PR into `main` from one into
    /// `develop`; a step's `if:` and a `run:` read the same branches, and a
    /// push reads both as `''`, as on GitHub. Each case asserts every answer,
    /// so a name missing from any one context — read as `''` — cannot pass.
    #[tokio::test]
    async fn pull_request_branches_reach_groups_conditions_and_steps() {
        for (event, group, printed, into_main, from_feature) in [
            (
                TestEvent::pull_request("feature", "main"),
                "build-feature",
                "head=[feature] base=[main]",
                "success",
                "success",
            ),
            (
                TestEvent::pull_request("hotfix", "develop"),
                "build-hotfix",
                "head=[hotfix] base=[develop]",
                "skipped",
                "skipped",
            ),
            (
                TestEvent::on("push", None),
                "build-refs/heads/main",
                "head=[] base=[]",
                "skipped",
                "skipped",
            ),
        ] {
            let (jobs, pipeline, db, _temp) =
                run_workflows_for(&[("build.yml", PULL_REQUEST_REFS)], event).await;
            assert_eq!(pipeline.status, "success", "{event:?}");
            assert_eq!(
                pipeline.concurrency_group.as_deref(),
                Some(group),
                "{event:?}"
            );
            assert_eq!(
                pipeline.head_branch.as_deref(),
                event.head_branch,
                "{event:?}"
            );
            let log = jobs["build/print"].log.clone().unwrap_or_default();
            assert!(log.contains(printed), "{event:?}: {log}");
            assert_eq!(jobs["build/into-main"].status, into_main, "{event:?}");
            let rows = step_rows(&db, jobs["build/from-feature"].id).await;
            assert_eq!(rows[0].1, from_feature, "{event:?}: {rows:?}");
        }
    }

    /// card_a93f0cb6c158: `github.ref_name` in a `run:` and in a step's `if:`
    /// — a branch and a tag by their short name, a pull request's ref as it
    /// is, the same answer in both.
    const REF_NAME: &str = "on: [push, pull_request]\njobs:\n  print:\n    steps:\n      - run: echo \"ref-name=[${{ github.ref_name }}]\"\n      - if: github.ref_name == 'v1.2'\n        run: echo tagged\n";

    #[tokio::test]
    async fn github_ref_name_is_the_short_ref_in_run_and_if() {
        for (event, printed, tagged) in [
            (TestEvent::on("push", None), "ref-name=[main]", "skipped"),
            (
                TestEvent {
                    ref_name: "refs/tags/v1.2",
                    ..TestEvent::on("push", None)
                },
                "ref-name=[v1.2]",
                "success",
            ),
            (
                TestEvent::pull_request("feature", "main"),
                "ref-name=[refs/pull/1/head]",
                "skipped",
            ),
        ] {
            let (jobs, pipeline, db, _temp) =
                run_workflows_for(&[("build.yml", REF_NAME)], event).await;
            assert_eq!(pipeline.status, "success", "{event:?}: {jobs:?}");
            let log = jobs["build/print"].log.clone().unwrap_or_default();
            assert!(log.contains(printed), "{event:?}: {log}");
            let rows = step_rows(&db, jobs["build/print"].id).await;
            assert_eq!(rows[1].1, tagged, "{event:?}: {rows:?}");
        }
    }

    /// The job fields resolved before a runner starts read it too.
    #[tokio::test]
    async fn github_ref_name_names_a_job_field() {
        let workflow = "on: push\njobs:\n  ship:\n    runs-on: pool-${{ github.ref_name }}\n    steps:\n      - run: echo ship\n";
        let (db, pipeline_id, _, _temp) = publish_pipeline_on(workflow, &[], "push", None)
            .await
            .expect("the workflow publishes a pipeline");
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(
            jobs[0].tags.as_deref(),
            Some(r#"["pool-main"]"#),
            "{jobs:?}"
        );
    }

    /// The `actions/upload-artifact` name each job of the pipeline `workflow`
    /// publishes on a push stores, by job — or the refusal.
    async fn artifact_names_of(
        workflow: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let (db, pipeline_id, _, _temp) = publish_pipeline_on(workflow, &[], "push", None).await?;
        Ok(
            rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .into_iter()
                .map(|job| {
                    let artifacts: serde_json::Value =
                        serde_json::from_str(job.artifacts.as_deref().expect("an artifact"))
                            .unwrap();
                    (job.name, artifacts["name"].as_str().unwrap().to_owned())
                })
                .collect(),
        )
    }

    const UPLOAD: &str = "on: push\nenv:\n  ART: bundle\njobs:\n  build:\nSTRATEGY    steps:\n      - run: echo\n      - uses: actions/upload-artifact@v4\n        with:\n          name: NAME\n          path: dist\n";

    /// card_5bb39c78906d: the name of a job's artifact is rendered per matrix
    /// variant, as `runs-on` is, so a matrix publishes one artifact per
    /// variant; `github.*` and `env.*` read as they did; a literal name is
    /// unchanged.
    #[tokio::test]
    async fn an_artifact_name_reads_the_matrix_variant_and_github_context() {
        let matrix = "    strategy:\n      matrix:\n        os: [linux, mac]\n";
        for (strategy, name, expected) in [
            (
                matrix,
                "dist-${{ matrix.os }}",
                &[
                    ("build/build [os=linux]", "dist-linux"),
                    ("build/build [os=mac]", "dist-mac"),
                ][..],
            ),
            (
                "",
                "${{ env.ART }}-${{ github.ref_name }}",
                &[("build/build", "bundle-main")][..],
            ),
            ("", "plain", &[("build/build", "plain")][..]),
            // No `name:` at all is GitHub's `artifact`, not the job's
            // `build/build`, which no artifact may be called.
            ("", "", &[("build/build", "artifact")][..]),
        ] {
            let workflow = UPLOAD.replace("STRATEGY", strategy);
            let workflow = if name.is_empty() {
                workflow.replace("          name: NAME\n", "")
            } else {
                workflow.replace("NAME", name)
            };
            let names = artifact_names_of(&workflow)
                .await
                .unwrap_or_else(|error| panic!("{name}: {error:#}"));
            let expected = expected
                .iter()
                .map(|(job, artifact)| ((*job).to_owned(), (*artifact).to_owned()))
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(names, expected, "{name}");
        }
    }

    /// A name this engine has no value for is refused by field and
    /// expression, not by the characters of `${{` it would otherwise have been
    /// stored under; a rendered name the store cannot keep names the job.
    #[tokio::test]
    async fn an_artifact_name_refuses_an_unknown_expression_or_an_unusable_value() {
        for (name, expected) in [
            (
                "art-${{ github.actor }}",
                "build: step 2 with.name expression `github.actor`",
            ),
            (
                "art-${{ secrets.TOKEN }}",
                "build: step 2 with.name expression `secrets.TOKEN`",
            ),
            (
                "art-${{ github.ref }}",
                "job 'build/build' has an unusable artifacts block: artifact name \
                 'art-refs/heads/main'",
            ),
        ] {
            let workflow = UPLOAD.replace("STRATEGY", "").replace("NAME", name);
            let error = artifact_names_of(&workflow)
                .await
                .expect_err("the artifact name must be refused");
            let message = format!("{error:#}");
            assert!(message.contains(expected), "{name}: {message}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "the workflow's mistake, not the server's: {message}"
            );
        }
    }

    /// The rows a push of `workflows` publishes, by job name — or the refusal.
    async fn job_rows_of(
        workflows: &[(&str, &str)],
    ) -> Result<std::collections::BTreeMap<String, rg_db::entities::pipeline_job::Model>> {
        let (db, pipeline_id, _, _temp) =
            publish_workflows_for(workflows, &[], TestEvent::on("push", None)).await?;
        Ok(
            rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .into_iter()
                .map(|job| (job.name.clone(), job))
                .collect(),
        )
    }

    const PATHS: &str = "on: push\njobs:\n  build:\n    strategy:\n      matrix:\nMATRIX    env:\n      OUT: out-${{ matrix.os }}\n    steps:\n      - uses: actions/cache@v4\n        with:\n          key: k-${{ matrix.os }}\n          path: |\n            CACHE\n            .cargo\n      - run: echo\n      - uses: actions/upload-artifact@v4\n        with:\n          name: dist-${{ matrix.os }}\n          path: PATH\n";

    /// card_ffb33f8af357: an artifact's and a cache's `path:` are rendered per
    /// matrix variant, as the artifact's name is — written directly or through
    /// an `env.*` that reads the matrix — instead of being stored as the text
    /// `${{ matrix.os }}`, which no path is called.
    #[tokio::test]
    async fn artifact_and_cache_paths_read_the_matrix_variant() {
        let matrix = "        os: [linux, mac]\n";
        for path in ["out-${{ matrix.os }}", "${{ env.OUT }}"] {
            let workflow = PATHS
                .replace("MATRIX", matrix)
                .replace("CACHE", "target-${{ matrix.os }}")
                .replace("PATH", path);
            let rows = job_rows_of(&[("build.yml", &workflow)])
                .await
                .unwrap_or_else(|error| panic!("{path}: {error:#}"));
            assert_eq!(rows.len(), 2, "{path}: {rows:?}");
            for os in ["linux", "mac"] {
                let job = &rows[&format!("build/build [os={os}]")];
                let artifacts: serde_json::Value =
                    serde_json::from_str(job.artifacts.as_deref().unwrap()).unwrap();
                assert_eq!(
                    artifacts,
                    serde_json::json!({ "name": format!("dist-{os}"), "paths": [format!("out-{os}")] }),
                    "{path}"
                );
                let cache_paths: Vec<String> =
                    serde_json::from_str(job.cache_paths.as_deref().unwrap()).unwrap();
                assert_eq!(cache_paths, [format!("target-{os}"), ".cargo".to_owned()]);
            }
        }
    }

    /// A path expression this engine has no value for is refused by field
    /// and expression when the workflow is read; a path whose own text leaves
    /// the workspace is refused as the author wrote it, and one a matrix value
    /// takes out of it is refused for that variant.
    #[tokio::test]
    async fn a_path_refuses_an_unknown_expression_or_a_way_out_of_the_workspace() {
        let os = "        os: [linux]\n";
        for (matrix, cache, path, expected) in [
            (
                os,
                "target",
                "${{ secrets.TOKEN }}",
                "build: step 3 with.path expression `secrets.TOKEN`",
            ),
            (
                os,
                "${{ steps.prep.outputs.dir }}",
                "dist",
                "build: step 1 with.path expression `steps.prep.outputs.dir`",
            ),
            (
                os,
                "target",
                "/${{ matrix.os }}",
                "artifact path must stay within the workspace: /${{ matrix.os }}",
            ),
            (
                "        os: [linux]\n        dir: [../up]\n",
                "target",
                "${{ matrix.dir }}",
                "job 'build/build [dir=../up, os=linux]' has an unusable artifacts block: \
                 artifact path must stay within the workspace: ../up",
            ),
        ] {
            let workflow = PATHS
                .replace("MATRIX", matrix)
                .replace("CACHE", cache)
                .replace("PATH", path);
            let error = job_rows_of(&[("build.yml", &workflow)])
                .await
                .expect_err("the path must be refused");
            let message = format!("{error:#}");
            assert!(message.contains(expected), "{path}: {message}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "the workflow's mistake, not the server's: {message}"
            );
        }
    }

    const NAMED: &str = "on: push\njobs:\n  b:\nSTRATEGYNAME    steps:\n      - run: echo hi\n";

    /// The name each job of `workflows` is shown under, by job.
    async fn display_names_of(
        workflows: &[(&str, &str)],
    ) -> Result<std::collections::BTreeMap<String, Option<String>>> {
        Ok(job_rows_of(workflows)
            .await?
            .into_iter()
            .map(|(name, job)| (name, job.display_name))
            .collect())
    }

    /// card_1535b3af81ac: `jobs.<id>.name` is accepted, rendered per matrix
    /// variant as `runs-on` is, and stored beside the job's id, which stays
    /// what it was.
    #[tokio::test]
    async fn a_job_name_is_shown_per_matrix_variant_beside_the_job_id() {
        let matrix = "    strategy:\n      matrix:\n        os: [linux, mac]\n";
        for (strategy, name, expected) in [
            (
                matrix,
                "Build on ${{ matrix.os }}",
                &[
                    ("build/b [os=linux]", Some("Build on linux")),
                    ("build/b [os=mac]", Some("Build on mac")),
                ][..],
            ),
            (
                "",
                "Build ${{ github.ref_name }}",
                &[("build/b", Some("Build main"))][..],
            ),
            ("", "Plain", &[("build/b", Some("Plain"))][..]),
            ("", "", &[("build/b", None)][..]),
        ] {
            let name_line = if name.is_empty() {
                String::new()
            } else {
                format!("    name: {name}\n")
            };
            let workflow = NAMED
                .replace("STRATEGY", strategy)
                .replace("NAME", &name_line);
            let names = display_names_of(&[("build.yml", &workflow)])
                .await
                .unwrap_or_else(|error| panic!("{name}: {error:#}"));
            let expected = expected
                .iter()
                .map(|(job, shown)| ((*job).to_owned(), shown.map(str::to_owned)))
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(names, expected, "{name}");
        }
    }

    /// A name expression this engine has no value for before a runner starts
    /// is refused by field and expression.
    #[tokio::test]
    async fn a_job_name_refuses_an_expression_it_has_no_value_for() {
        for expression in ["secrets.TOKEN", "needs.a.outputs.x", "env.X"] {
            let workflow = NAMED.replace("STRATEGY", "").replace(
                "NAME",
                &format!("    name: Build ${{{{ {expression} }}}}\n"),
            );
            let error = display_names_of(&[("build.yml", &workflow)])
                .await
                .expect_err("the name must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains(&format!("b: name expression `{expression}`")),
                "{expression}: {message}"
            );
        }
    }

    /// card_ffb33f8af357 (the low half): a step's label reads the matrix
    /// variant, `github.*` and `env.*` — written or, for an unnamed step, the
    /// first line it runs — in the step rows and in the steps a runner gets;
    /// an expression with no value before a runner starts stays as written
    /// instead of refusing the workflow over a label.
    #[tokio::test]
    async fn a_step_label_reads_the_matrix_variant() {
        let workflow = "on: push\nenv:\n  TOOL: cargo\njobs:\n  b:\n    strategy:\n      matrix:\n        os: [linux, mac]\n    steps:\n      - name: Test on ${{ matrix.os }} with ${{ env.TOOL }} at ${{ github.ref_name }}\n        run: echo test\n      - run: echo ${{ matrix.os }}\n      - name: Cache on ${{ runner.os }}\n        run: echo cache\n";
        let (db, pipeline_id, _, _temp) = publish_pipeline_on(workflow, &[], "push", None)
            .await
            .expect("the workflow publishes a pipeline");
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 2, "{jobs:?}");
        for job in jobs {
            let os = if job.name.contains("linux") {
                "linux"
            } else {
                "mac"
            };
            let expected = [
                format!("Test on {os} with cargo at main"),
                format!("Run echo {os}"),
                "Cache on ${{ runner.os }}".to_owned(),
            ];
            let rows = step_rows(&db, job.id)
                .await
                .into_iter()
                .map(|row| row.0)
                .collect::<Vec<_>>();
            assert_eq!(rows, expected, "{}", job.name);
            let specs: Vec<rg_process::ci_steps::StepSpec> =
                serde_json::from_str(job.steps.as_deref().unwrap()).unwrap();
            let names = specs.into_iter().map(|step| step.name).collect::<Vec<_>>();
            assert_eq!(names, expected, "{}", job.name);
        }
    }

    const CALLED: &str = "on:\n  workflow_call:\n    inputs:\n      target:\n        type: string\njobs:\n  deploy:\n    name: Deploy ${{ inputs.target }}\n    steps:\n      - run: echo\n  check:\n    steps:\n      - run: echo\n";

    /// The name of a job that calls a reusable workflow heads each job it
    /// brings, as GitHub shows `<caller> / <job>`; the called job's own name
    /// reads the called workflow's inputs. Only a literal heads them.
    #[tokio::test]
    async fn a_calling_jobs_name_heads_the_jobs_it_brings() {
        let caller = "on: push\njobs:\n  shared:\n    name: NAME\n    uses: ./.gitea/workflows/called.yml\n    with:\n      target: prod\n";
        let names = display_names_of(&[
            ("build.yml", &caller.replace("NAME", "Shared")),
            ("called.yml", CALLED),
        ])
        .await
        .unwrap();
        let shown = names.values().flatten().cloned().collect::<Vec<_>>();
        assert_eq!(
            shown,
            ["Shared / check", "Shared / Deploy prod"],
            "{names:?}"
        );

        let error = display_names_of(&[
            (
                "build.yml",
                &caller.replace("NAME", "Shared ${{ github.ref_name }}"),
            ),
            ("called.yml", CALLED),
        ])
        .await
        .expect_err("an expression in a calling job's name is refused");
        assert!(
            format!("{error:#}").contains(
                "jobs.shared.name: a job that calls a reusable workflow takes a literal name"
            ),
            "{error:#}"
        );
    }

    /// card_71a3e7424faa: the pre-2022 spelling of a dispatch input, in a
    /// step's `run:` and `env:` and a job's `env:`, and in a job a reusable
    /// workflow brings, whose own `inputs.target` is a different value —
    /// `github.event` there is still the dispatch.
    const EVENT_INPUTS: &str = "on:\n  push:\n  workflow_dispatch:\n    inputs:\n      target:\n        type: string\n      dry-run:\n        type: boolean\njobs:\n  ship:\n    env:\n      JOB_TARGET: ${{ github.event.inputs.target }}\n    steps:\n      - run: echo \"run=[${{ github.event.inputs.target }}] dry=[${{ github.event.inputs.DRY-RUN }}] job=[$JOB_TARGET] step=[$STEP_TARGET]\"\n        env:\n          STEP_TARGET: ${{ github.event.inputs.target }}\n  shared:\n    uses: ./.gitea/workflows/called.yml\n    with:\n      target: from-caller\n";

    const EVENT_INPUTS_CALLED: &str = "on:\n  workflow_call:\n    inputs:\n      target:\n        type: string\njobs:\n  inner:\n    steps:\n      - run: echo \"called=[${{ inputs.target }}] event=[${{ github.event.inputs.target }}]\"\n";

    /// card_71a3e7424faa's acceptance, through the embedded runner: a dispatch
    /// prints its input in `run:`, in `env:` and in the job of a called
    /// workflow, as text — a ticked box is `true`, as the event payload carries
    /// it; a push prints `''`. The called job prints its own `inputs.target`
    /// beside it, so reading one for the other cannot pass.
    #[tokio::test]
    async fn dispatch_inputs_read_as_github_event_inputs_in_text_fields() {
        for (event, inputs, ship, inner) in [
            (
                rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                Some(&[("target", "staging"), ("dry-run", "true")][..]),
                "run=[staging] dry=[true] job=[staging] step=[staging]",
                "called=[from-caller] event=[staging]",
            ),
            (
                "push",
                None,
                "run=[] dry=[] job=[] step=[]",
                "called=[from-caller] event=[]",
            ),
        ] {
            let (jobs, pipeline, _db, _temp) = run_workflows_for(
                &[
                    ("build.yml", EVENT_INPUTS),
                    ("called.yml", EVENT_INPUTS_CALLED),
                ],
                TestEvent::on(event, inputs),
            )
            .await;
            assert_eq!(pipeline.status, "success", "{event}: {jobs:?}");
            for (job, expected) in [("build/ship", ship), ("build/shared/inner", inner)] {
                let log = jobs[job].log.clone().unwrap_or_default();
                assert!(log.contains(expected), "{job} on {event}: {log}");
            }
        }
    }

    /// The job fields resolved before a runner starts read the dispatch too:
    /// `runs-on: ${{ github.event.inputs.pool }}` routes the job to the pool
    /// the form named.
    #[tokio::test]
    async fn github_event_inputs_name_a_job_field() {
        let workflow = "on:\n  workflow_dispatch:\n    inputs:\n      pool:\n        type: string\njobs:\n  ship:\n    runs-on: ${{ github.event.inputs.pool }}\n    steps:\n      - run: echo ship\n";
        let (db, pipeline_id, _, _temp) = publish_pipeline_on(
            workflow,
            &[],
            rg_core::ci::WORKFLOW_DISPATCH_EVENT,
            Some(&[("pool", "gpu-pool")]),
        )
        .await
        .expect("the workflow publishes a pipeline");
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(jobs[0].tags.as_deref(), Some(r#"["gpu-pool"]"#), "{jobs:?}");
    }

    /// A name `workflow_dispatch` does not declare is refused by name in a text
    /// field as it is in `if:` — on GitHub it reads as `''`, and a typo in a
    /// deploy command would quietly deploy nothing — and so is a value that
    /// would turn into an expression of its own: `${{ secrets.X }}` typed into
    /// the form must not hand the job that secret.
    #[tokio::test]
    async fn github_event_inputs_in_a_text_field_refuse_an_undeclared_or_expression_input() {
        for (field, inputs, expected) in [
            (
                "      - run: echo ${{ github.event.inputs.tagret }}\n",
                &[("target", "staging")][..],
                "github.event.inputs.tagret, which workflow_dispatch does not declare",
            ),
            (
                "      - run: echo x\n        env:\n          T: ${{ github.event.inputs.nope }}\n",
                &[("target", "staging")][..],
                "github.event.inputs.nope, which workflow_dispatch does not declare",
            ),
            (
                "      - uses: actions/cache@v4\n        with:\n          path: x\n          key: ${{ github.event.inputs.nope }}\n",
                &[("target", "staging")][..],
                "github.event.inputs.nope, which workflow_dispatch does not declare",
            ),
            (
                "      - run: echo ${{ github.event.inputs.target }}\n",
                &[("target", "${{ secrets.DEPLOY_KEY }}")][..],
                "workflow_dispatch input 'target' holds `${{`",
            ),
            // `inputs.*` read the same text before this was refused, and the
            // step was handed `DEPLOY_KEY`.
            (
                "      - run: echo ${{ inputs.target }}\n",
                &[("target", "${{ secrets.DEPLOY_KEY }}")][..],
                "workflow_dispatch input 'target' holds `${{`",
            ),
        ] {
            let workflow = format!(
                "on:\n  workflow_dispatch:\n    inputs:\n      target:\n        type: string\njobs:\n  ship:\n    steps:\n{field}"
            );
            let error = publish_pipeline_on(
                &workflow,
                &[],
                rg_core::ci::WORKFLOW_DISPATCH_EVENT,
                Some(inputs),
            )
            .await
            .map(|_| ())
            .expect_err("the workflow must be refused");
            let message = format!("{error:#}");
            assert!(message.contains(expected), "{field}: {message}");
            assert!(
                error
                    .downcast_ref::<rg_core::error::InvalidRequest>()
                    .is_some(),
                "the workflow's mistake, not the server's: {message}"
            );
        }
    }

    /// card_29fa305fba93's workflow: `notify` and `cleanup` hang off `test`,
    /// `report` has no status function, `after` names `!cancelled()`.
    const AFTER_FAILURE: &str = "on: push\njobs:\n  build:\n    steps:\n      - run: exit BUILD\n  test:\n    needs: build\n    steps:\n      - run: exit TEST\n  notify:\n    needs: test\n    if: failure()\n    steps:\n      - run: echo notify\n  cleanup:\n    needs: test\n    if: always()\n    steps:\n      - run: echo cleanup\n  report:\n    needs: test\n    steps:\n      - run: echo report\n  after:\n    needs: notify\n    if: ${{ !cancelled() }}\n    steps:\n      - run: echo after\n";

    /// card_29fa305fba93's acceptance, through the embedded runner: `failure()`
    /// runs after a needed job failed — or after a job *that* one needs failed
    /// and skipped it — and is skipped otherwise; `always()` runs either way; a
    /// job with no status function is skipped after a failure; the pipeline
    /// stays `failed` however green the cleanup is.
    #[tokio::test]
    async fn status_functions_run_a_job_after_the_jobs_it_needs_failed() {
        for (build, test, expected, pipeline) in [
            (
                "0",
                "1",
                [
                    ("build/build", "success"),
                    ("build/test", "failed"),
                    ("build/notify", "success"),
                    ("build/cleanup", "success"),
                    ("build/report", "skipped"),
                    ("build/after", "success"),
                ],
                "failed",
            ),
            (
                "0",
                "0",
                [
                    ("build/build", "success"),
                    ("build/test", "success"),
                    ("build/notify", "skipped"),
                    ("build/cleanup", "success"),
                    ("build/report", "success"),
                    ("build/after", "success"),
                ],
                "success",
            ),
            (
                "1",
                "0",
                [
                    ("build/build", "failed"),
                    ("build/test", "skipped"),
                    ("build/notify", "success"),
                    ("build/cleanup", "success"),
                    ("build/report", "skipped"),
                    ("build/after", "success"),
                ],
                "failed",
            ),
        ] {
            let workflow = AFTER_FAILURE.replace("BUILD", build).replace("TEST", test);
            let (jobs, status, _db, _temp) = run_pipeline_on(&workflow, "push", None).await;
            for (name, job_status) in expected {
                assert_eq!(
                    jobs[name].status, job_status,
                    "build={build} test={test} {name}: {:?}",
                    jobs[name].log
                );
            }
            assert_eq!(status, pipeline, "build={build} test={test}");
        }
    }

    /// A job that failed under `continue-on-error: true` failed nothing: the
    /// jobs that need it read `success`, as on GitHub, and a `failure()` after
    /// it does not announce a failure on a green pipeline.
    #[tokio::test]
    async fn a_job_that_continued_on_error_is_no_failure_to_the_jobs_after_it() {
        let (jobs, status, _db, _temp) = run_pipeline_on(
            "on: push\njobs:\n  flaky:\n    continue-on-error: true\n    steps:\n      - run: exit 1\n  notify:\n    needs: flaky\n    if: failure()\n    steps:\n      - run: echo notify\n  report:\n    needs: flaky\n    if: needs.flaky.result == 'success'\n    steps:\n      - run: echo report\n",
            "push",
            None,
        )
        .await;
        assert_eq!(jobs["build/flaky"].status, "failed");
        assert_eq!(
            jobs["build/notify"].status, "skipped",
            "{:?}",
            jobs["build/notify"].log
        );
        assert_eq!(
            jobs["build/report"].status, "success",
            "{:?}",
            jobs["build/report"].log
        );
        assert_eq!(status, "success");
    }

    /// The external-runner path of the same: after `test` failed, the job
    /// with no status function is skipped with the stage, the held
    /// `failure()` job is decided once the stages before it finished, and
    /// only then is it handed out.
    #[tokio::test]
    async fn a_polling_runner_gets_the_job_that_runs_after_a_failure() {
        let workflow = AFTER_FAILURE.replace("BUILD", "0").replace("TEST", "1");
        let (db, pipeline_id, repo_id, _temp) = publish_pipeline_on(&workflow, &[], "push", None)
            .await
            .unwrap();
        let job = |name: &'static str| {
            let db = db.clone();
            async move {
                rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|job| job.name == name)
                    .unwrap()
            }
        };
        let pick = || {
            let db = db.clone();
            async move {
                match rg_db::ops::pipeline_ops::find_pending_job_for_runner(&db, repo_id, &[], true)
                    .await
                    .unwrap()
                {
                    rg_db::ops::pipeline_ops::PendingPick::Job(job) => Some(job.name),
                    _ => None,
                }
            }
        };
        for (name, outcome) in [("build/build", "success"), ("build/test", "failed")] {
            // `test` needs `build`, so it is held too: decided first, as a
            // poll decides every held job whose stage is reached.
            for held in rg_db::ops::pipeline_condition_ops::ready_deferred_conditions(&db, repo_id)
                .await
                .unwrap()
            {
                rg_core::ci::CiTrigger::decide_deferred_condition(&CiEngine::new(), &db, &held)
                    .await
                    .unwrap();
            }
            assert_eq!(pick().await.as_deref(), Some(name));
            let row = job(name).await;
            assert!(
                rg_db::ops::pipeline_ops::start_job_if_active(&db, row.id, None)
                    .await
                    .unwrap()
            );
            rg_db::ops::pipeline_ops::finish_embedded_job(
                &db,
                pipeline_id,
                row.stage_id,
                row.id,
                outcome,
                Some(i32::from(outcome != "success")),
                None,
                Some(chrono::Utc::now().naive_utc()),
            )
            .await
            .unwrap();
        }
        assert_eq!(job("build/report").await.status, "skipped");
        assert_eq!(pick().await, None, "nothing runs before it is decided");
        let mut ready = rg_db::ops::pipeline_condition_ops::ready_deferred_conditions(&db, repo_id)
            .await
            .unwrap()
            .into_iter()
            .map(|job| job.name)
            .collect::<Vec<_>>();
        ready.sort();
        assert_eq!(ready, ["build/cleanup", "build/notify"]);
        for held in rg_db::ops::pipeline_condition_ops::ready_deferred_conditions(&db, repo_id)
            .await
            .unwrap()
        {
            rg_core::ci::CiTrigger::decide_deferred_condition(&CiEngine::new(), &db, &held)
                .await
                .unwrap();
        }
        let picked = pick().await;
        assert!(
            matches!(picked.as_deref(), Some("build/notify" | "build/cleanup")),
            "{picked:?}"
        );
        let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(pipeline.status.as_str(), "pending" | "running"),
            "notify and cleanup are still to run: {}",
            pipeline.status
        );
    }

    /// A matrix read from the outputs of a job that was skipped is not
    /// expanded from outputs nobody wrote, and does not fail: the matrix job
    /// is skipped whole.
    #[tokio::test]
    async fn a_matrix_whose_needed_job_was_skipped_is_skipped() {
        let (_db, jobs, _temp) = run_pipeline_jobs(
            "on: push\njobs:\n  setup:\n    if: github.ref == 'refs/heads/release'\n    outputs:\n      os: ${{ steps.list.outputs.os }}\n    steps:\n      - id: list\n        run: echo 'os=[\"linux\"]' >> \"$GITHUB_OUTPUT\"\n  test:\n    needs: setup\n    strategy:\n      matrix:\n        os: ${{ fromJSON(needs.setup.outputs.os) }}\n    steps:\n      - run: echo test\n",
        )
        .await;
        let test = &jobs["build/test"];
        assert_eq!(test.status, "skipped", "{:?}", test.log);
        assert!(test.deferred_matrix.is_none());
        assert!(test
            .log
            .as_deref()
            .is_some_and(|log| log.contains("`setup`, a job this one needs, was skipped")));
    }

    /// The external-runner path: a job that needs another is never handed out
    /// before it is decided, and its environment — which may stop it for an
    /// approval — is attached only when it is to run, so a job its `if:`
    /// skips never asks anyone to approve it.
    #[tokio::test]
    async fn the_engine_decides_a_held_job_for_a_polling_runner() {
        let (db, pipeline_id, repo_id, _temp) = publish_pipeline_with_environments(
            "on: push\njobs:\n  changes:\n    outputs:\n      deploy: ${{ steps.f.outputs.deploy }}\n    steps:\n      - id: f\n        run: echo deploy=yes >> \"$GITHUB_OUTPUT\"\n  deploy:\n    needs: changes\n    if: needs.changes.outputs.deploy == 'yes'\n    environment: production\n    steps:\n      - run: echo deploy\n  preview:\n    needs: changes\n    if: needs.changes.outputs.deploy == 'no'\n    environment: staging\n    steps:\n      - run: echo preview\n",
            &[("production", true), ("staging", false)],
        )
        .await;
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        let named = |name: &str| jobs.iter().find(|job| job.name == name).unwrap().clone();
        let (changes, deploy, preview) = (
            named("build/changes"),
            named("build/deploy"),
            named("build/preview"),
        );
        for held in [&deploy, &preview] {
            assert_eq!(held.status, "pending");
            assert!(held.deferred_condition.is_some(), "{held:?}");
            assert_eq!(held.environment_name, None, "not attached before it runs");
        }
        let picked = rg_db::ops::pipeline_ops::find_pending_job_for_runner(&db, repo_id, &[], true)
            .await
            .unwrap();
        let rg_db::ops::pipeline_ops::PendingPick::Job(picked) = picked else {
            panic!("changes is there to hand out: {picked:?}");
        };
        assert_eq!(picked.id, changes.id);
        assert!(
            rg_db::ops::pipeline_condition_ops::ready_deferred_conditions(&db, repo_id)
                .await
                .unwrap()
                .is_empty(),
            "changes has not run"
        );
        assert!(
            !rg_db::ops::pipeline_ops::assign_job(&db, deploy.id, 1)
                .await
                .unwrap(),
            "a held job cannot be claimed, even by id"
        );

        assert!(
            rg_db::ops::pipeline_ops::start_job_if_active(&db, changes.id, None)
                .await
                .unwrap()
        );
        assert!(rg_db::ops::pipeline_step_ops::record_job_outputs(
            &db,
            changes.id,
            r#"{"deploy":"yes"}"#
        )
        .await
        .unwrap());
        rg_db::ops::pipeline_ops::finish_embedded_job(
            &db,
            pipeline_id,
            changes.stage_id,
            changes.id,
            "success",
            Some(0),
            None,
            Some(chrono::Utc::now().naive_utc()),
        )
        .await
        .unwrap();

        // The stage is reached, and still nothing is handed out before the
        // held jobs are decided.
        assert!(matches!(
            rg_db::ops::pipeline_ops::find_pending_job_for_runner(&db, repo_id, &[], true)
                .await
                .unwrap(),
            rg_db::ops::pipeline_ops::PendingPick::Nothing
        ));
        let ready = rg_db::ops::pipeline_condition_ops::ready_deferred_conditions(&db, repo_id)
            .await
            .unwrap();
        let mut ready_ids = ready.iter().map(|job| job.id).collect::<Vec<_>>();
        ready_ids.sort_unstable();
        let mut held_ids = vec![deploy.id, preview.id];
        held_ids.sort_unstable();
        assert_eq!(ready_ids, held_ids);
        for held in &ready {
            assert!(
                rg_core::ci::CiTrigger::decide_deferred_condition(&CiEngine::new(), &db, held)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let reread = |id: i64| {
            let db = db.clone();
            async move {
                rg_db::ops::pipeline_ops::get_job(&db, id)
                    .await
                    .unwrap()
                    .unwrap()
            }
        };
        let deploy = reread(deploy.id).await;
        assert_eq!(deploy.status, "waiting_approval", "{deploy:?}");
        assert_eq!(deploy.environment_name.as_deref(), Some("production"));
        assert!(deploy.deferred_condition.is_none());
        let preview = reread(preview.id).await;
        assert_eq!(preview.status, "skipped", "{preview:?}");
        assert_eq!(
            preview.environment_name, None,
            "a skipped job asks for no environment"
        );

        // Deciding again changes nothing.
        for held in &ready {
            rg_core::ci::CiTrigger::decide_deferred_condition(&CiEngine::new(), &db, held)
                .await
                .unwrap();
        }
        assert_eq!(reread(deploy.id).await.status, "waiting_approval");
        assert_eq!(reread(preview.id).await.status, "skipped");
    }

    async fn step_log(db: &rg_db::DatabaseConnection, job_id: i64, number: i32) -> String {
        rg_db::ops::pipeline_step_ops::find_job_step(db, job_id, number)
            .await
            .unwrap()
            .and_then(|step| step.log)
            .unwrap_or_default()
    }

    /// card_5e09bc40b020's acceptance, through the embedded runner: a step's
    /// `working-directory: ${{ matrix.dir }}` — and a job's
    /// `defaults.run.working-directory` reading it — runs each variant in its
    /// own directory. It used to become `${MATRIX_DIR}`, which no shell reads
    /// there, and every variant failed with "No such file or directory".
    #[tokio::test]
    async fn a_working_directory_reads_the_matrix_variant() {
        let (jobs, status, db, _temp) = run_pipeline_on(
            "on: push\njobs:\n  step:\n    strategy:\n      matrix:\n        dir: [a, b]\n    steps:\n      - run: mkdir -p a b\n      - run: echo \"in=[$(basename \"$PWD\")]\"\n        working-directory: ${{ matrix.dir }}\n  defaulted:\n    strategy:\n      matrix:\n        dir: [a, b]\n    defaults:\n      run:\n        working-directory: ${{ matrix.dir }}\n    steps:\n      - run: mkdir -p a b\n        working-directory: .\n      - run: echo \"in=[$(basename \"$PWD\")]\"\n",
            "push",
            None,
        )
        .await;
        assert_eq!(status, "success", "{jobs:?}");
        for job in ["step", "defaulted"] {
            for dir in ["a", "b"] {
                let name = format!("build/{job} [dir={dir}]");
                let log = step_log(&db, jobs[&name].id, 2).await;
                assert!(log.contains(&format!("in=[{dir}]")), "{name}: {log}");
            }
        }
    }

    /// A dispatch form names the directory too, so the value it became is what
    /// has to stay inside the workspace: `../..` is refused by name.
    #[tokio::test]
    async fn a_working_directory_from_a_dispatch_input_stays_inside_the_workspace() {
        let (jobs, status, db, _temp) = run_pipeline_on(
            "on:\n  workflow_dispatch:\n    inputs:\n      dir:\n        type: string\njobs:\n  ship:\n    steps:\n      - run: pwd\n        working-directory: ${{ inputs.dir }}\n",
            rg_core::ci::WORKFLOW_DISPATCH_EVENT,
            Some(&[("dir", "../..")]),
        )
        .await;
        assert_eq!(status, "failed", "{jobs:?}");
        let log = step_log(&db, jobs["build/ship"].id, 1).await;
        assert!(
            log.contains("working-directory `../..` must stay inside the workspace"),
            "{log}"
        );
    }

    /// card_d97de416d0b0's acceptance, through the embedded runner: the
    /// matrix of `test` is what `setup` wrote to its outputs, `include` and
    /// all; the placeholder `test` is gone and its variants ran.
    #[tokio::test]
    async fn a_matrix_built_from_a_needed_jobs_outputs_runs_its_variants() {
        let (db, jobs, _temp) = run_pipeline_jobs(
            "on: push\njobs:\n  setup:\n    outputs:\n      matrix: ${{ steps.list.outputs.matrix }}\n    steps:\n      - id: list\n        run: echo 'matrix={\"package\":[\"api\",\"web\"],\"include\":[{\"package\":\"web\",\"node-version\":20}]}' >> \"$GITHUB_OUTPUT\"\n  test:\n    needs: setup\n    strategy:\n      matrix: ${{ fromJSON(needs.setup.outputs.matrix) }}\n    steps:\n      - run: echo \"package=${{ matrix.package }} node=${{ matrix.node-version }}\"\n",
        )
        .await;
        let mut names = jobs.keys().cloned().collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            [
                "build/setup",
                "build/test [package=api]",
                "build/test [package=web, node-version=20]",
            ],
            "the placeholder is replaced by the variants"
        );
        for (name, expected) in [
            ("build/test [package=api]", "package=api node=\n"),
            (
                "build/test [package=web, node-version=20]",
                "package=web node=20",
            ),
        ] {
            let job = &jobs[name];
            assert_eq!(job.status, "success", "{name}: {:?}", job.log);
            assert!(
                step_log(&db, job.id, 1).await.contains(expected),
                "{name}: {:?}",
                job.log
            );
            assert_eq!(job.matrix_group.as_deref(), Some("build/test"));
            assert_eq!(job.matrix_fail_fast, Some(true), "GitHub's default");
        }
    }

    /// The external-runner path through the real engine: the placeholder of
    /// a matrix decided at run time survives being stored — steps, context,
    /// strategy and all — and once its needed job has succeeded, the engine
    /// writes the variants a polling runner is then handed, never the
    /// placeholder itself.
    #[tokio::test]
    async fn the_engine_expands_a_stored_matrix_for_a_polling_runner() {
        let (db, pipeline_id, repo_id, _temp) = publish_pipeline(
            "on: push\njobs:\n  setup:\n    outputs:\n      os: ${{ steps.list.outputs.os }}\n    steps:\n      - id: list\n        run: echo 'os=[\"linux\",\"mac\"]' >> \"$GITHUB_OUTPUT\"\n  test:\n    needs: setup\n    strategy:\n      max-parallel: 1\n      matrix:\n        os: ${{ fromJSON(needs.setup.outputs.os) }}\n    steps:\n      - run: echo \"on ${{ matrix.os }} after ${{ needs.setup.result }}\"\n",
        )
        .await;
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        let setup = jobs.iter().find(|job| job.name == "build/setup").unwrap();
        let placeholder = jobs.iter().find(|job| job.name == "build/test").unwrap();
        assert!(placeholder.deferred_matrix.is_some());
        assert!(
            rg_db::ops::pipeline_matrix_ops::ready_deferred_jobs(&db, repo_id)
                .await
                .unwrap()
                .is_empty(),
            "setup has not run"
        );

        assert!(
            rg_db::ops::pipeline_ops::start_job_if_active(&db, setup.id, None)
                .await
                .unwrap()
        );
        assert!(rg_db::ops::pipeline_step_ops::record_job_outputs(
            &db,
            setup.id,
            r#"{"os":"[\"linux\",\"mac\"]"}"#
        )
        .await
        .unwrap());
        rg_db::ops::pipeline_ops::finish_embedded_job(
            &db,
            pipeline_id,
            setup.stage_id,
            setup.id,
            "success",
            Some(0),
            None,
            Some(chrono::Utc::now().naive_utc()),
        )
        .await
        .unwrap();

        let ready = rg_db::ops::pipeline_matrix_ops::ready_deferred_jobs(&db, repo_id)
            .await
            .unwrap();
        assert_eq!(
            ready.iter().map(|job| job.id).collect::<Vec<_>>(),
            [placeholder.id]
        );
        let finished =
            rg_core::ci::CiTrigger::expand_deferred_matrix(&CiEngine::new(), &db, &ready[0])
                .await
                .unwrap();
        assert!(finished.is_none());

        let variants = rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, placeholder.stage_id)
            .await
            .unwrap();
        assert_eq!(
            variants
                .iter()
                .map(|job| job.name.as_str())
                .collect::<Vec<_>>(),
            ["build/test [os=linux]", "build/test [os=mac]"]
        );
        for variant in &variants {
            assert_eq!(variant.matrix_group.as_deref(), Some("build/test"));
            assert_eq!(variant.matrix_max_parallel, Some(1));
            assert!(variant
                .steps
                .as_deref()
                .unwrap_or_default()
                .contains("${MATRIX_OS}"));
            assert!(variant
                .job_context
                .as_deref()
                .unwrap_or_default()
                .contains("build/setup"));
        }
        // A variant needs `setup` like its job did, so it is held until the
        // poll decides it, which the poll does right after expanding.
        assert!(
            variants
                .iter()
                .all(|variant| variant.deferred_condition.is_some()),
            "{variants:?}"
        );
        assert!(matches!(
            rg_db::ops::pipeline_ops::find_pending_job_for_runner(&db, repo_id, &[], true)
                .await
                .unwrap(),
            rg_db::ops::pipeline_ops::PendingPick::Nothing
        ));
        for held in rg_db::ops::pipeline_condition_ops::ready_deferred_conditions(&db, repo_id)
            .await
            .unwrap()
        {
            assert!(rg_core::ci::CiTrigger::decide_deferred_condition(
                &CiEngine::new(),
                &db,
                &held
            )
            .await
            .unwrap()
            .is_none());
        }
        let picked = rg_db::ops::pipeline_ops::find_pending_job_for_runner(&db, repo_id, &[], true)
            .await
            .unwrap();
        let rg_db::ops::pipeline_ops::PendingPick::Job(picked) = picked else {
            panic!("a variant is there to hand out: {picked:?}");
        };
        assert_eq!(picked.name, "build/test [os=linux]");
        assert!(
            rg_core::ci::CiTrigger::expand_deferred_matrix(&CiEngine::new(), &db, &ready[0])
                .await
                .unwrap()
                .is_none()
                && rg_db::ops::pipeline_ops::list_jobs_by_stage(&db, placeholder.stage_id)
                    .await
                    .unwrap()
                    .len()
                    == 2,
            "a second expansion of the same placeholder writes nothing"
        );
    }

    /// What the needed job left has to be a matrix; when it is not, the job
    /// fails saying why, and so does the pipeline.
    #[tokio::test]
    async fn a_matrix_the_needed_job_did_not_leave_fails_its_job_by_reason() {
        let many = format!(
            "{{\\\"n\\\":[{}]}}",
            (0..257)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        for (written, reason) in [
            (
                "not-json".to_string(),
                "`needs.setup.outputs.matrix` is not JSON",
            ),
            (
                "{\\\"n\\\":[]}".to_string(),
                "matrix dimension `n` has no values",
            ),
            (many, "matrix expands to 257 variants; maximum is 256"),
        ] {
            let (_db, jobs, _temp) = run_pipeline_jobs(&format!(
                "on: push\njobs:\n  setup:\n    outputs:\n      matrix: ${{{{ steps.list.outputs.matrix }}}}\n    steps:\n      - id: list\n        run: echo \"matrix={written}\" >> \"$GITHUB_OUTPUT\"\n  test:\n    needs: setup\n    strategy:\n      matrix: ${{{{ fromJSON(needs.setup.outputs.matrix) }}}}\n    steps:\n      - run: echo never\n",
            ))
            .await;
            let test = &jobs["build/test"];
            assert_eq!(test.status, "failed", "{written}: {jobs:?}");
            let log = test.log.clone().unwrap_or_default();
            assert!(log.contains(reason), "{written}: {log}");
            assert_eq!(test.deferred_matrix, None);
            assert_eq!(jobs.len(), 2, "no variant was written: {jobs:?}");
        }
    }

    /// `strategy.fail-fast`: the built-in runner runs the variants in order,
    /// so the first one failing cancels the two after it — and with
    /// `fail-fast: false` they run.
    ///
    /// The runner re-reads each job before it starts it: a stale `pending`
    /// would send it to start a variant fail-fast canceled, find the job no
    /// longer its own and walk away from the pipeline before announcing how
    /// it ended — the pipeline-finished hook is what shows that.
    #[tokio::test]
    async fn fail_fast_cancels_the_unfinished_variants_unless_switched_off() {
        static FINISHED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        fn observe(status: &str) {
            FINISHED.lock().unwrap().push(status.to_string());
        }
        rg_core::metrics_hook::set_ci_pipeline_finished_observer(observe);
        let workflow = "on: push\njobs:\n  test:\n    strategy:\nFAIL_FAST      matrix:\n        n: [a, b, c]\n    steps:\n      - run: test \"${{ matrix.n }}\" != a\n";
        let (_db, jobs, _temp) = run_pipeline_jobs(&workflow.replace("FAIL_FAST", "")).await;
        let status =
            |jobs: &std::collections::HashMap<String, rg_db::entities::pipeline_job::Model>,
             n: &str| { jobs[&format!("build/test [n={n}]")].status.clone() };
        assert_eq!(
            [status(&jobs, "a"), status(&jobs, "b"), status(&jobs, "c")],
            ["failed", "canceled", "canceled"]
        );
        let canceled = jobs["build/test [n=b]"].log.clone().unwrap_or_default();
        assert!(
            canceled.contains(
                "`build/test [n=a]` failed, and the matrix job's strategy.fail-fast is on"
            ),
            "{canceled}"
        );
        assert_eq!(
            *FINISHED.lock().unwrap(),
            ["failed"],
            "the runner saw the pipeline through to its end"
        );

        let (_db, jobs, _temp) =
            run_pipeline_jobs(&workflow.replace("FAIL_FAST", "      fail-fast: false\n")).await;
        assert_eq!(
            [status(&jobs, "a"), status(&jobs, "b"), status(&jobs, "c")],
            ["failed", "success", "success"]
        );
    }

    /// `matrix.<key>` reads the variant's coordinate even when the key is no
    /// variable name: `node-version` has only `MATRIX_NODE_VERSION`, and the
    /// job's `if:` used to read nothing for it and skip every variant.
    #[tokio::test]
    async fn a_job_condition_reads_a_matrix_key_that_is_no_variable_name() {
        let (_db, jobs, _temp) = run_pipeline_jobs(
            "on: push\njobs:\n  test:\n    if: matrix.node-version == '20'\n    strategy:\n      matrix:\n        node-version: [18, 20]\n    steps:\n      - run: echo ok\n",
        )
        .await;
        assert_eq!(jobs["build/test [node-version=18]"].status, "skipped");
        assert_eq!(jobs["build/test [node-version=20]"].status, "success");
    }

    /// Steps are separate processes: a variable exported by one step does not
    /// reach the next, and `$GITHUB_ENV` does.
    #[tokio::test]
    async fn one_step_reaches_the_next_only_through_the_file_commands() {
        let (db, job_id, _temp) = run_workflow(
            "on: push\njobs:\n  build:\n    steps:\n      - run: |\n          export LEAKED=yes\n          echo SHARED=carried >> \"$GITHUB_ENV\"\n      - run: echo \"leaked=${LEAKED:-no} shared=$SHARED\"\n",
        )
        .await;
        let second = rg_db::ops::pipeline_step_ops::find_job_step(&db, job_id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.status, "success");
        assert!(
            second
                .log
                .unwrap_or_default()
                .contains("leaked=no shared=carried"),
            "only the file command crosses"
        );
    }
}

#[cfg(test)]
mod composite_action_tests {
    //! card_4cf988c73fd1: `uses: owner/repo@ref` loads a composite action —
    //! from a repository of this instance or from the configured upstream —
    //! under the operator's policy, and the job runs its steps with the
    //! caller's inputs and hands back its outputs.

    use super::*;
    use sea_orm::{NotSet, Set};

    fn git(args: &[&str], cwd: &std::path::Path) -> String {
        let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let output = gateway.run(args, Some(cwd)).unwrap();
        assert!(output.success(), "git {args:?}: {}", output.stderr_str());
        output.stdout_str().trim().to_string()
    }

    /// A bare repository at `bare` holding `files` on `main`, tagged `v1`;
    /// answers the commit.
    fn bare_with(bare: &std::path::Path, files: &[(&str, &str)]) -> String {
        let work = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let path = work.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "CI"],
            vec!["config", "user.email", "ci@example.com"],
            vec!["add", "-A"],
            vec!["commit", "-q", "-m", "action"],
            vec!["tag", "v1"],
        ] {
            git(&args, work.path());
        }
        std::fs::create_dir_all(bare).unwrap();
        git(&["init", "-q", "--bare"], bare);
        let bare_arg = bare.to_string_lossy();
        git(&["push", "-q", &bare_arg, "main", "v1"], work.path());
        git(&["rev-parse", "HEAD"], work.path())
    }

    const GREET: &str = "name: Greet\ninputs:\n  who:\n    required: true\n  greeting:\n    default: Hello\noutputs:\n  said:\n    value: ${{ steps.say.outputs.text }}\nruns:\n  using: composite\n  steps:\n    - id: say\n      shell: sh\n      run: |\n        echo \"composite says ${{ inputs.greeting }}, ${{ inputs.who }}\"\n        echo \"text=${{ inputs.greeting }} ${{ inputs.who }}\" >> \"$GITHUB_OUTPUT\"\n";

    const CALLER: &str = "on: push\njobs:\n  build:\n    steps:\n      - id: hi\n        uses: USES\n        with:\n          who: world\n      - name: Read\n        run: echo \"caller read <${{ steps.hi.outputs.said }}>\"\n";

    struct Fixture {
        temp: tempfile::TempDir,
        db: rg_db::DatabaseConnection,
        repo_id: i64,
        user_id: i64,
        repo_path: std::path::PathBuf,
        sha: String,
    }

    async fn repository(
        db: &rg_db::DatabaseConnection,
        owner_id: i64,
        name: &str,
        is_private: bool,
    ) -> i64 {
        let now = chrono::Utc::now();
        rg_db::ops::repo_ops::create(
            db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner_id),
                name: Set(name.into()),
                description: Set(None),
                is_private: Set(is_private),
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
        .unwrap()
        .id
    }

    /// The workflow repository `step-owner/steps` running `workflow`, and two
    /// repositories of this instance holding the greet action: `tools/greet`
    /// (public) and `tools/secret` (private).
    async fn fixture(workflow: &str) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let repo_path = temp.path().join("step-owner/steps.git");
        let sha = bare_with(&repo_path, &[(".gitea/workflows/build.yml", workflow)]);
        bare_with(
            &temp.path().join("tools/greet.git"),
            &[("action.yml", GREET)],
        );
        bare_with(
            &temp.path().join("tools/secret.git"),
            &[("action.yml", GREET)],
        );
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("actions.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "step-owner",
            "step-owner@example.com",
            "unused",
            "Step Owner",
        )
        .await
        .unwrap();
        let tools =
            rg_db::ops::user_ops::create_user(&db, "tools", "tools@example.com", "unused", "Tools")
                .await
                .unwrap();
        let repo_id = repository(&db, user.id, "steps", true).await;
        repository(&db, tools.id, "greet", false).await;
        repository(&db, tools.id, "secret", true).await;
        Fixture {
            temp,
            db,
            repo_id,
            user_id: user.id,
            repo_path,
            sha,
        }
    }

    async fn trigger(fixture: &Fixture, policy: actions::ActionsPolicy) -> Result<i64> {
        let engine = CiEngine::new().with_actions_policy(policy);
        trigger_pipeline_with_barrier_and_engine(
            TriggerPipelineParams {
                db: &fixture.db,
                repo_root: fixture.temp.path(),
                repo_path: &fixture.repo_path,
                repo_id: fixture.repo_id,
                commit_sha: &fixture.sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                head_branch: None,
                previous_sha: None,
                inputs: None,
                schedule: None,
                triggered_by: Some(fixture.user_id),
                docker_enabled: false,
                // Published only: the runner below is driven and awaited.
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
            },
            &engine,
            None,
        )
        .await
    }

    /// Run the pipeline on the embedded runner; the log of its one job.
    async fn run_log(fixture: &Fixture, pipeline_id: i64) -> (String, String) {
        let mut runner = runner::PipelineRunner::new_local_only(
            fixture.db.clone(),
            fixture.temp.path(),
            &fixture.repo_path,
            pipeline_id,
        );
        runner.set_repo_id(fixture.repo_id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();
        let stage = rg_db::ops::pipeline_ops::list_stages_by_pipeline(&fixture.db, pipeline_id)
            .await
            .unwrap()
            .remove(0);
        let job = rg_db::ops::pipeline_ops::list_jobs_by_stage(&fixture.db, stage.id)
            .await
            .unwrap()
            .remove(0);
        (job.status, job.log.unwrap_or_default())
    }

    /// The card's acceptance: a composite action from a repository of this
    /// instance runs with the caller's input and its declared default, and the
    /// next step reads its output; the commit it resolved to is in the log.
    #[tokio::test]
    async fn an_instance_action_runs_with_its_inputs_and_hands_back_its_outputs() {
        let fixture = fixture(&CALLER.replace("USES", "tools/greet@v1")).await;
        let pipeline_id = trigger(&fixture, actions::ActionsPolicy::default())
            .await
            .expect("the default policy loads public actions of this instance");
        let (status, log) = run_log(&fixture, pipeline_id).await;
        assert_eq!(status, "success", "{log}");
        assert!(log.contains("composite says Hello, world"), "{log}");
        assert!(log.contains("caller read <Hello world>"), "{log}");
        let action_sha = git(
            &["rev-parse", "v1"],
            &fixture.temp.path().join("tools/greet.git"),
        );
        assert!(
            log.contains(&format!("Action tools/greet@v1 is commit {action_sha}")),
            "the resolved commit must be in the job log: {log}"
        );
    }

    /// `./path` reads the workflow's own repository at the commit being built,
    /// whatever the policy allows for other repositories.
    #[tokio::test]
    async fn a_local_action_runs_from_the_commit_being_built() {
        let temp_workflow = CALLER.replace("USES", "./.gitea/actions/greet");
        let fixture = fixture(&temp_workflow).await;
        // Rebuild the workflow repository with the action beside the workflow.
        std::fs::remove_dir_all(&fixture.repo_path).unwrap();
        let sha = bare_with(
            &fixture.repo_path,
            &[
                (".gitea/workflows/build.yml", &temp_workflow),
                (".gitea/actions/greet/action.yml", GREET),
            ],
        );
        let fixture = Fixture { sha, ..fixture };
        let none = actions::ActionsPolicy::default()
            .with_sources("ci.actions_sources", &[])
            .unwrap();
        let pipeline_id = trigger(&fixture, none).await.expect("a local action loads");
        let (status, log) = run_log(&fixture, pipeline_id).await;
        assert_eq!(status, "success", "{log}");
        assert!(log.contains("caller read <Hello world>"), "{log}");
        assert!(log.contains("from this repository"), "{log}");
    }

    const SCRIPTS: &str = "name: Scripts\ninputs:\n  who:\n    required: true\nruns:\n  using: composite\n  steps:\n    - shell: sh\n      run: sh ${{ github.action_path }}/hello.sh \"${{ inputs.who }}\"\n    - shell: sh\n      working-directory: ${{ github.action_path }}/data\n      env:\n        GREETING_FILE: ${{ github.action_path }}/data/greeting.txt\n      run: echo \"dir=$(basename \"$PWD\") file=$(cat \"$GREETING_FILE\") direct=$(cat \"$GITHUB_ACTION_PATH/data/greeting.txt\")\"\n";

    const SCRIPTS_CALLER: &str = "on: push\njobs:\n  build:\n    steps:\n      - uses: USES\n        with:\n          who: world\n";

    /// card_3e5298bfab47's acceptance on the embedded runner: a composite
    /// action that runs a script from its own directory — `${{
    /// github.action_path }}` in `run:`, `working-directory` and `env:`, and
    /// `$GITHUB_ACTION_PATH` — runs it from this instance and from `./path`.
    #[tokio::test]
    async fn a_composite_action_runs_the_scripts_beside_its_action_yml() {
        let files = [
            ("action.yml", SCRIPTS),
            ("hello.sh", "echo \"script says hello, $1\"\n"),
            ("data/greeting.txt", "hi-from-the-action\n"),
        ];
        let fixture = fixture(&SCRIPTS_CALLER.replace("USES", "tools/greet@v1")).await;
        let action = fixture.temp.path().join("tools/greet.git");
        std::fs::remove_dir_all(&action).unwrap();
        bare_with(&action, &files);
        let pipeline_id = trigger(&fixture, actions::ActionsPolicy::default())
            .await
            .expect("a composite action reading its own directory loads");
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&fixture.db, pipeline_id)
            .await
            .unwrap();
        let steps = jobs[0].steps.as_deref().unwrap_or_default();
        assert!(
            steps.contains("\"using\":\"composite\"") && steps.contains("${GITHUB_ACTION_PATH}"),
            "{steps}"
        );
        let (status, log) = run_log(&fixture, pipeline_id).await;
        assert_eq!(status, "success", "{log}");
        assert!(log.contains("script says hello, world"), "{log}");
        assert!(
            log.contains("dir=data file=hi-from-the-action direct=hi-from-the-action"),
            "{log}"
        );

        let workflow = SCRIPTS_CALLER.replace("USES", "./.gitea/actions/scripts");
        let fixture = self::fixture(&workflow).await;
        std::fs::remove_dir_all(&fixture.repo_path).unwrap();
        let mut local = vec![(".gitea/workflows/build.yml", workflow.as_str())];
        let placed = files
            .iter()
            .map(|(path, body)| (format!(".gitea/actions/scripts/{path}"), *body))
            .collect::<Vec<_>>();
        local.extend(placed.iter().map(|(path, body)| (path.as_str(), *body)));
        let sha = bare_with(&fixture.repo_path, &local);
        let fixture = Fixture { sha, ..fixture };
        let pipeline_id = trigger(&fixture, actions::ActionsPolicy::default())
            .await
            .expect("a local composite action reading its own directory loads");
        let (status, log) = run_log(&fixture, pipeline_id).await;
        assert_eq!(status, "success", "{log}");
        assert!(log.contains("script says hello, world"), "{log}");
        assert!(log.contains("dir=data file=hi-from-the-action"), "{log}");
    }

    /// `github.action_path` is read whole in a step's `run:`, `env:` and
    /// `working-directory`; anywhere else — an `if:`, the `with:` of an action
    /// the action calls, part of a larger expression — it is refused by name.
    #[tokio::test]
    async fn github_action_path_outside_the_text_of_a_step_is_refused_by_name() {
        for step in [
            "    - shell: sh\n      if: github.action_path != ''\n      run: echo x\n",
            "    - uses: tools/secret@v1\n      with:\n        who: ${{ github.action_path }}\n",
            "    - shell: sh\n      run: echo ${{ github.action_path || 'x' }}\n",
        ] {
            let fixture = fixture(&SCRIPTS_CALLER.replace("USES", "tools/greet@v1")).await;
            let action = fixture.temp.path().join("tools/greet.git");
            std::fs::remove_dir_all(&action).unwrap();
            let manifest = format!(
                "name: Scripts\ninputs:\n  who:\n    required: true\nruns:\n  using: composite\n  steps:\n{step}"
            );
            bare_with(&action, &[("action.yml", manifest.as_str())]);
            let error = trigger(&fixture, actions::ActionsPolicy::default())
                .await
                .expect_err("github.action_path outside a step's text is refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("reads github.action_path") && message.contains("nowhere else"),
                "`{step}`: {message}"
            );
        }
    }

    /// A source the policy does not allow is refused by its name, before any
    /// pipeline exists; so is a private repository of somebody else, and a tag
    /// where the operator asked for full SHAs.
    #[tokio::test]
    async fn what_the_policy_does_not_allow_is_refused_by_the_source_it_names() {
        let fixture = fixture(&CALLER.replace("USES", "tools/greet@v1")).await;
        let upstream_only = actions::ActionsPolicy::default()
            .with_sources("ci.actions_sources", &["upstream".into()])
            .unwrap();
        let error = trigger(&fixture, upstream_only).await.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some(),
            "a refused source is the workflow's to fix: {message}"
        );
        assert!(
            message.contains("source \"instance\"") && message.contains("tools/greet"),
            "the refusal names the source and the action: {message}"
        );

        let pinned = actions::ActionsPolicy::default().with_require_full_sha(true);
        let message = format!("{:#}", trigger(&fixture, pinned).await.unwrap_err());
        assert!(message.contains("ci.actions_require_full_sha"), "{message}");

        let fixture = self::fixture(&CALLER.replace("USES", "tools/secret@v1")).await;
        let message = format!(
            "{:#}",
            trigger(&fixture, actions::ActionsPolicy::default())
                .await
                .unwrap_err()
        );
        assert!(message.contains("private repository"), "{message}");

        let (_, total) = rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(
            &fixture.db,
            fixture.repo_id,
            0,
            10,
        )
        .await
        .unwrap();
        assert_eq!(total, 0, "a refused action published a pipeline");
    }

    /// An action the instance does not hold comes from the upstream when the
    /// policy allows it — here a `file://` directory of mirrors — and is
    /// cached by its commit; when the policy does not, it is refused naming
    /// the upstream.
    #[tokio::test]
    async fn an_upstream_action_is_fetched_cached_and_refused_when_not_allowed() {
        let fixture = fixture(&CALLER.replace("USES", "acme/greet@v1")).await;
        let mirrors = tempfile::tempdir().unwrap();
        let upstream_sha = bare_with(&mirrors.path().join("acme/greet"), &[("action.yml", GREET)]);
        let url = format!("file://{}", mirrors.path().display());

        let refused = format!(
            "{:#}",
            trigger(&fixture, actions::ActionsPolicy::default())
                .await
                .unwrap_err()
        );
        assert!(
            refused.contains("source \"upstream\"") && refused.contains("no repository acme/greet"),
            "{refused}"
        );

        let upstream = actions::ActionsPolicy::default()
            .with_sources("ci.actions_sources", &["upstream".into()])
            .unwrap()
            .with_upstream_url("ci.actions_upstream_url", &url)
            .unwrap();
        let pipeline_id = trigger(&fixture, upstream.clone())
            .await
            .expect("the upstream serves the action");
        let (status, log) = run_log(&fixture, pipeline_id).await;
        assert_eq!(status, "success", "{log}");
        assert!(log.contains("caller read <Hello world>"), "{log}");
        assert!(
            log.contains(&format!("is commit {upstream_sha} from {url}/acme/greet")),
            "{log}"
        );
        let cached = fixture
            .temp
            .path()
            .join(actions::ACTIONS_CACHE_DIR)
            .read_dir()
            .unwrap()
            .count();
        assert_eq!(cached, 1, "the fetch is cached under the repository root");

        // Pinned by the SHA it resolved to, the action is served from the
        // cache with the upstream gone.
        drop(mirrors);
        let pinned =
            self::fixture(&CALLER.replace("USES", &format!("acme/greet@{upstream_sha}"))).await;
        std::fs::rename(
            fixture.temp.path().join(actions::ACTIONS_CACHE_DIR),
            pinned.temp.path().join(actions::ACTIONS_CACHE_DIR),
        )
        .unwrap();
        trigger(&pinned, upstream)
            .await
            .expect("a SHA the cache holds needs no upstream");
    }
}

/// card_a1290adf28fd: the acceptance — the real `actions/setup-node` and
/// `actions/setup-go`, pinned by commit, loaded from GitHub the way an
/// operator's `actions_sources = ["upstream"]` loads them, packed into the
/// server's archives and run by the step runner with Node. Ignored: it needs
/// the network (github.com, nodejs.org, go.dev) and Node.js on the host.
#[cfg(test)]
mod javascript_action_tests {
    use super::*;

    const SETUP_NODE: &str = "actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020";
    const SETUP_GO: &str = "actions/setup-go@d35c59abb061a4a6fb18e82ac0862c26744d6ab5";

    #[tokio::test]
    #[ignore = "needs the network (github.com, nodejs.org, go.dev) and Node.js on the host"]
    async fn setup_node_and_setup_go_run_from_their_pinned_commits() {
        let root = tempfile::tempdir().unwrap();
        let repo_root = root.path().join("repos");
        let workflow_repo = root.path().join("workflow");
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::create_dir_all(&workflow_repo).unwrap();
        let mut workflow = gitea_actions::GiteaWorkflow::parse(&format!(
            "on: push\njobs:\n  build:\n    steps:\n      - id: node\n        uses: {SETUP_NODE}\n        with:\n          node-version: '20.18.0'\n      - run: echo \"node=$(node --version) said=${{{{ steps.node.outputs.node-version }}}}\"\n      - uses: {SETUP_GO}\n        with:\n          go-version: '1.22.5'\n          cache: false\n      - run: go version\n"
        ))
        .unwrap();
        let policy = actions::ActionsPolicy::default()
            .with_sources("ci.actions_sources", &["upstream".into()])
            .unwrap()
            .with_require_full_sha(true);
        let lookup = |_: &str, _: &str| Ok(None);
        let loader = actions::RepositoryActionLoader {
            policy: &policy,
            workflow_repo: &workflow_repo,
            workflow_repo_id: 1,
            commit_sha: &"0".repeat(40),
            repo_root: &repo_root,
            lookup: &lookup,
        };
        // Fetching and packing block, as they do on the trigger's blocking
        // thread; a test may wait on them in place.
        actions::expand_actions(&mut workflow, &loader)
            .unwrap()
            .unwrap();
        let expanded = workflow;
        expanded.validate_supported_actions().unwrap();
        let config = expanded.to_ci_config(&gitea_actions::WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "0".repeat(40),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
            event_schedule: None,
            head_ref: String::new(),
            base_ref: String::new(),
        });
        let job = &config.jobs["build"];
        let steps = job.steps.clone().unwrap();
        assert!(
            steps.iter().any(|step| step.action.is_some()),
            "no step runs an action: {steps:#?}"
        );

        let workspace = root.path().join("workspace");
        let tool_cache = root.path().join("tool-cache");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&tool_cache).unwrap();
        let mut env = vec![
            ("CI".to_string(), "true".to_string()),
            ("CI_REF".to_string(), "refs/heads/main".to_string()),
            ("CI_EVENT".to_string(), "push".to_string()),
            ("CI_REPOSITORY".to_string(), "o/r".to_string()),
            (
                "HOME".to_string(),
                root.path().to_string_lossy().into_owned(),
            ),
        ];
        if let Ok(path) = std::env::var("PATH") {
            env.push(("PATH".to_string(), path));
        }
        let archives = runner::ServerActionArchives {
            root: repo_root.join(actions::ACTION_ARCHIVES_DIR),
        };
        let mask = |log: &str| log.to_string();
        let run = rg_process::ci_steps::run_job_steps(
            &steps,
            &env,
            &workspace,
            None,
            None,
            &mask,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            None,
            rg_process::ci_steps::JobSupport {
                actions: Some(&archives),
                tool_cache: Some(&tool_cache),
            },
            &mut rg_process::ci_steps::Silent,
        )
        .await;
        assert_eq!(run.exit_code, 0, "{}", run.log);
        assert!(
            run.log.contains("node=v20.18.0 said=v20.18.0"),
            "{}",
            run.log
        );
        assert!(run.log.contains("go version go1.22.5"), "{}", run.log);
        assert!(
            std::fs::read_dir(&tool_cache).unwrap().count() >= 2,
            "the toolchains were not kept in the tool cache"
        );
    }
}
