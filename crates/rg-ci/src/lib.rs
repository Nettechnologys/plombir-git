//! ForgeKeep CI/CD Engine.
//!
//! Parses `.forgekeep-ci.yml` or `.gitea/workflows/*.yml` (Gitea Actions format)
//! from the repository and executes pipelines.
//!
//! ## Native format (`.forgekeep-ci.yml`)
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

pub mod condition;
pub mod config;
pub mod gitea_actions;
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
#[derive(Default)]
pub struct CiEngine {
    /// The hub and SMTP wiring the post-push hooks this engine spawns need.
    /// See [`CiNotifications`].
    notifications: CiNotifications,
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
        Self { notifications }
    }
}

impl rg_core::ci::CiTrigger for CiEngine {
    fn has_ci_config(&self, repo_path: &std::path::Path, commit_sha: &str) -> bool {
        rg_core::ci::has_ci_config(repo_path, commit_sha)
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

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
        Box::pin(trigger_pipeline(params, &self.notifications))
    }

    fn resume_pipeline<'a>(
        &'a self,
        params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(resume_pipeline(params, &self.notifications))
    }
}

/// Resume an already-created pipeline. External runners only need the job to
/// be moved back to `pending`; an internal runner is recreated from persisted
/// pipeline state and skips terminal jobs.
pub async fn resume_pipeline(
    params: ResumePipelineParams<'_>,
    notifications: &CiNotifications,
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
        params.repo_path,
        params.repo_id,
        params.pipeline_id,
        params.docker_enabled,
        params.allow_host_runner,
        params.jwt_secret,
        params.encryption_key,
        params.external_url,
        notifications,
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
/// running. The canonical GitHub recipe
/// `${{ github.workflow }}-${{ github.head_ref || github.ref }}` lands here
/// because of the `||`, and it has to land loudly: silently degrading it to "no
/// concurrency" is how two deploys end up running at once, which is the very
/// thing the block was written to prevent.
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
             ${{{{ github.repository_owner }}}} — and ${{{{ ref }}}} / ${{{{ branch }}}} in \
             .forgekeep-ci.yml. Left unresolved it is one literal group shared by every ref of \
             this repository."
        )));
    }
    Ok(Some(group))
}

/// Trigger a CI pipeline for a push event.
///
/// This function:
/// 1. Reads `.forgekeep-ci.yml` from the repo at the given commit
/// 2. Parses the CI configuration
/// 3. Checks concurrency control (if configured)
/// 4. Creates pipeline/stage/job records in the DB
/// 5. Spawns the pipeline runner in a background task, injecting CI_JOB_TOKEN
pub async fn trigger_pipeline(
    params: TriggerPipelineParams<'_>,
    notifications: &CiNotifications,
) -> Result<i64> {
    trigger_pipeline_with_barrier(params, notifications, None).await
}

/// Internal entrypoint with a rendezvous immediately before a grouped trigger
/// tries to acquire its database lock. Production passes `None`; concurrency
/// tests use the barrier to put two real `trigger_pipeline` executions at the
/// old empty-group race window without sleeps or scheduler luck.
async fn trigger_pipeline_with_barrier(
    params: TriggerPipelineParams<'_>,
    notifications: &CiNotifications,
    before_concurrency_lock: Option<&tokio::sync::Barrier>,
) -> Result<i64> {
    let TriggerPipelineParams {
        db,
        repo_path,
        repo_id,
        commit_sha,
        ref_name,
        trigger_type,
        base_branch,
        previous_sha,
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
    let (identity_owner, identity_name) = identity
        .as_ref()
        .map(|(owner, name)| (owner.as_str(), name.as_str()))
        .unwrap_or(("", ""));
    let config = read_ci_config(
        repo_path,
        RepositoryName {
            owner: identity_owner,
            name: identity_name,
        },
        commit_sha,
        ref_name,
        trigger_type,
        base_branch,
        previous_sha,
    )?;
    validate_execution_semantics(&config)?;
    // Held until every verdict about the client's file has been given, and still
    // before the first write.
    let (repo_owner, repo_name) = identity?;

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

    for stage in rg_db::ops::pipeline_ops::list_stages_by_pipeline(db, pipeline_id).await? {
        rg_db::ops::pipeline_ops::try_update_stage(db, stage.id).await?;
    }
    rg_db::ops::pipeline_ops::try_update_pipeline(db, pipeline_id).await?;

    if rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id)
        .await?
        .is_some_and(|pipeline| pipeline.status == "success")
    {
        evaluate_initial_success(
            db,
            repo_path,
            repo_id,
            commit_sha,
            docker_enabled,
            external_runners,
            allow_host_runner,
            jwt_secret,
            encryption_key,
            external_url,
            notifications,
        )
        .await;
        return Ok(pipeline_id);
    }

    // 6. Spawn pipeline runner in background (only if not using external runners)
    if !external_runners {
        spawn_internal_runner(
            db,
            repo_path,
            repo_id,
            pipeline_id,
            docker_enabled,
            allow_host_runner,
            jwt_secret,
            encryption_key,
            external_url,
            notifications,
        );
    } else {
        if let Some(first_stage) =
            rg_db::ops::pipeline_ops::list_stages_by_pipeline(db, pipeline_id)
                .await?
                .into_iter()
                .next()
        {
            rg_db::ops::pipeline_ops::try_pause_stage_at_manual(db, first_stage.id).await?;
        }
        tracing::info!(
            pipeline_id = pipeline_id,
            "Pipeline created with external runner mode — jobs will be picked up by registered runners"
        );
    }

    Ok(pipeline_id)
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
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline_in_group(
            tx,
            self.repo_id,
            self.commit_sha,
            self.ref_name,
            self.trigger_type,
            self.triggered_by,
            self.concurrency_group,
        )
        .await?;

        let pipeline_id = pipeline.id;

        let stage_names = self.config.stages.as_ref().cloned().unwrap_or_default();
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
            config,
            ..
        } = *self;
        for (job_name, job_config) in &config.jobs {
            // Filter by `only` — if specified, skip jobs that don't match the ref
            if let Some(only) = &job_config.only {
                let ref_short = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
                if !only
                    .iter()
                    .any(|pattern| pattern == ref_short || pattern == ref_name)
                {
                    continue;
                }
            }

            let stage_name = job_config.stage.as_deref().unwrap_or("default");
            let stage_id = stage_id_map.get(stage_name).copied().unwrap_or(-1);

            if stage_id < 0 {
                tracing::warn!(job = %job_name, stage = %stage_name, "Job references unknown stage, skipping");
                continue;
            }

            for variant in expand_matrix(job_name, job_config)? {
                let fields = resolve_action_job_fields(
                    job_name,
                    job_config,
                    &variant,
                    ref_name,
                    trigger_type,
                    commit_sha,
                    repository,
                )?;
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
                let cache_paths_json = job_config
                    .cache
                    .as_ref()
                    .map(|cache| serde_json::to_string(&cache.paths))
                    .transpose()?;
                let job = rg_db::ops::pipeline_ops::create_job(
                    tx,
                    stage_id,
                    &variant.name,
                    &job_config.script.join("\n"),
                    fields.image.as_deref(),
                    tags_json.as_deref(),
                    variables_json.as_deref(),
                    job_config.cache.as_ref().map(|cache| cache.key.as_str()),
                    cache_paths_json.as_deref(),
                    job_config.allow_failure.unwrap_or(false),
                    job_config.timeout_seconds,
                    job_config.when.as_deref(),
                    job_config.condition.as_deref(),
                )
                .await?;
                let should_run = if let Some(condition) = job_config.condition.as_deref() {
                    condition::evaluate_condition(
                        condition,
                        &job_condition_context(
                            ref_name,
                            trigger_type,
                            commit_sha,
                            repository,
                            &variant.variables,
                            job_config,
                        ),
                    )?
                } else {
                    true
                };
                if !should_run {
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
                } else if let Some(environment_name) = fields.environment.as_deref() {
                    let environment =
                        rg_db::ops::ci_environment_ops::find_by_name(tx, repo_id, environment_name)
                            .await?;
                    rg_db::ops::ci_environment_ops::attach_job(
                        tx,
                        job.id,
                        environment.as_ref(),
                        environment_name,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn evaluate_initial_success(
    db: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    repo_id: i64,
    commit_sha: &str,
    docker_enabled: bool,
    external_runners: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    encryption_key: Option<&str>,
    external_url: Option<&str>,
    notifications: &CiNotifications,
) {
    let Some(repo_root) = repo_path.parent().and_then(std::path::Path::parent) else {
        return;
    };
    post_push_context(
        repo_root,
        docker_enabled,
        external_runners,
        allow_host_runner,
        jwt_secret,
        encryption_key,
        external_url,
        notifications,
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
/// They now arrive as [`CiNotifications`], wired once at startup, and the nested
/// engine (the one that triggers the merge commit's own pipeline, which can
/// cascade into another merge) inherits the same wiring rather than resetting it
/// to `None` one hop down.
#[allow(clippy::too_many_arguments)]
fn post_push_context(
    repo_root: &std::path::Path,
    docker_enabled: bool,
    external_runners: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    encryption_key: Option<&str>,
    external_url: Option<&str>,
    notifications: &CiNotifications,
) -> rg_core::push_hooks::PostPushContext {
    rg_core::push_hooks::PostPushContext {
        repo_root: repo_root.to_path_buf(),
        docker_enabled,
        external_runners,
        allow_host_runner,
        jwt_secret: jwt_secret.map(str::to_string),
        encryption_key: encryption_key.map(str::to_string),
        smtp_config: notifications.smtp_config.clone(),
        ci_engine: nested_engine(notifications),
        external_url: external_url.map(str::to_string),
        notifier: notifications.notifier.clone(),
        delivery_tracker: rg_core::task_tracker::delivery_tracker().clone(),
    }
}

/// The engine the hooks spawned here trigger through.
///
/// The merge commit those hooks produce gets a pipeline of its own, and that
/// pipeline can unblock the next merge — so the engine one hop down must carry
/// the same wiring, or the effects fade out on the second merge instead of at
/// the process boundary.
fn nested_engine(notifications: &CiNotifications) -> std::sync::Arc<CiEngine> {
    std::sync::Arc::new(CiEngine::with_notifications(notifications.clone()))
}

#[allow(clippy::too_many_arguments)]
fn spawn_internal_runner(
    db: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    repo_id: i64,
    pipeline_id: i64,
    docker_enabled: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    encryption_key: Option<&str>,
    external_url: Option<&str>,
    notifications: &CiNotifications,
) {
    let db_clone = db.clone();
    let notifications = notifications.clone();
    let repo_path_owned = repo_path.to_path_buf();
    let jwt_secret_owned = jwt_secret.map(str::to_string);
    let encryption_key_owned = encryption_key.map(str::to_string);
    let oidc_token_url =
        external_url.map(|url| format!("{}/api/v1/ci/oidc/token", url.trim_end_matches('/')));
    tokio::spawn(async move {
        let mut runner = if docker_enabled {
            PipelineRunner::new(db_clone, &repo_path_owned, pipeline_id)
        } else {
            PipelineRunner::new_local_only(db_clone, &repo_path_owned, pipeline_id)
        };
        runner.set_repo_id(repo_id);
        runner.set_allow_host_runner(allow_host_runner);
        runner.set_notifications(notifications);
        if let Some(secret) = jwt_secret_owned {
            runner.set_jwt_secret(secret);
        }
        if let Some(secret) = encryption_key_owned {
            runner.set_encryption_key(secret);
        }
        if let Some(url) = oidc_token_url {
            runner.set_oidc_token_url(url);
        }
        if let Err(error) = runner.run().await {
            tracing::error!(pipeline_id, error = %format!("{error:#}"), "pipeline runner error");
        }
    });
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
    for (name, job) in &config.jobs {
        if let Some(when) = job.when.as_deref() {
            if when != "on_success" && when != "manual" {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{name}' uses unsupported when: '{when}'; supported values are 'on_success' and 'manual'"
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
            if let Err(error) = crate::condition::validate_condition(condition) {
                return Err(rg_core::error::invalid_request(format!(
                    "job '{name}' has an unsupported if condition: {error:#}"
                )));
            }
        }
    }
    Ok(())
}

fn job_condition_context(
    ref_name: &str,
    event: &str,
    sha: &str,
    repository: RepositoryName<'_>,
    variables: &std::collections::BTreeMap<String, String>,
    config: &config::JobConfig,
) -> std::collections::HashMap<String, String> {
    let mut context = std::collections::HashMap::from([
        ("github.ref".into(), ref_name.to_string()),
        (
            "github.ref_name".into(),
            ref_name
                .strip_prefix("refs/heads/")
                .or_else(|| ref_name.strip_prefix("refs/tags/"))
                .unwrap_or(ref_name)
                .to_string(),
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
    ]);
    for (name, value) in variables {
        context.insert(format!("env.{name}"), value.clone());
    }
    if let Some(matrix) = &config.matrix {
        for name in matrix.keys() {
            if let Some(value) = variables.get(name) {
                context.insert(format!("matrix.{name}"), value.clone());
            }
        }
    }
    context
}

#[derive(Debug)]
struct MatrixVariant {
    name: String,
    variables: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ResolvedActionJobFields {
    image: Option<String>,
    tags: Option<Vec<String>>,
    environment: Option<String>,
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
        config::ActionExpression::GithubSha => Some(context.sha.to_owned()),
        config::ActionExpression::GithubEventName => Some(context.event.to_owned()),
        config::ActionExpression::GithubRepository => Some(format!(
            "{}/{}",
            context.repository.owner, context.repository.name
        )),
        config::ActionExpression::GithubRepositoryOwner => {
            Some(context.repository.owner.to_owned())
        }
        config::ActionExpression::Matrix(name) => context.variant.variables.get(name).cloned(),
        config::ActionExpression::Input(name) => context
            .variant
            .variables
            .get(&format!(
                "INPUT_{}",
                name.to_ascii_uppercase().replace('-', "_")
            ))
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

    Ok(ResolvedActionJobFields {
        image,
        tags,
        environment,
    })
}

fn expand_matrix(job_name: &str, config: &config::JobConfig) -> Result<Vec<MatrixVariant>> {
    let base: std::collections::BTreeMap<String, String> = config
        .variables
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let Some(matrix) = &config.matrix else {
        return Ok(vec![MatrixVariant {
            name: job_name.to_owned(),
            variables: base,
        }]);
    };
    // Same split as `validate_execution_semantics`: an unusable `matrix:` block
    // is the user's file being wrong, not this server failing, and it is only
    // reached from the pipeline build inside `trigger_pipeline` — where a bare
    // `anyhow` became a sanitized 500 that named neither the job nor the limit.
    if matrix.values().any(Vec::is_empty) {
        return Err(rg_core::error::invalid_request(format!(
            "job '{job_name}' has an empty matrix dimension"
        )));
    }
    let count = matrix
        .values()
        .try_fold(1usize, |total, values| total.checked_mul(values.len()))
        .ok_or_else(|| {
            rg_core::error::invalid_request(format!(
                "job '{job_name}' matrix is too large to expand; maximum is 256 variants"
            ))
        })?;
    if count > 256 {
        return Err(rg_core::error::invalid_request(format!(
            "job '{job_name}' matrix expands to {count} variants; maximum is 256"
        )));
    }
    let mut variants = vec![(Vec::<(String, String)>::new(), base)];
    for (key, values) in matrix {
        let mut next = Vec::new();
        for (labels, variables) in variants {
            for value in values {
                let mut labels = labels.clone();
                labels.push((key.clone(), value.clone()));
                let mut variables = variables.clone();
                variables.insert(key.clone(), value.clone());
                variables.insert(
                    format!("MATRIX_{}", key.to_ascii_uppercase().replace('-', "_")),
                    value.clone(),
                );
                next.push((labels, variables));
            }
        }
        variants = next;
    }
    Ok(variants
        .into_iter()
        .map(|(labels, variables)| MatrixVariant {
            name: format!(
                "{job_name} [{}]",
                labels
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            variables,
        })
        .collect())
}

/// The `<owner>/<name>` a pipeline is running for.
///
/// Carried down to [`gitea_actions::WorkflowContext`] so `${{ github.repository }}`
/// and `${{ github.repository_owner }}` resolve to something. It is resolved once
/// in [`trigger_pipeline`] from the database rather than passed in by each
/// producer: five call sites construct [`TriggerPipelineParams`], and a field
/// every one of them has to remember is a field one of them will forget — which
/// is how both halves of this identity came to be `String::new() // filled later`
/// in the first place (card_054e997a46e6).
#[derive(Debug, Clone, Copy)]
pub struct RepositoryName<'a> {
    pub owner: &'a str,
    pub name: &'a str,
}

/// Read CI configuration from the repo at the given commit.
///
/// Tries formats in order:
/// 1. `.gitea/workflows/*.yml` (Gitea Actions format)
/// 2. `.forgekeep-ci.yml` (native format)
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
fn read_ci_config(
    repo_path: &std::path::Path,
    repository: RepositoryName<'_>,
    commit_sha: &str,
    ref_name: &str,
    event: &str,
    base_branch: Option<&str>,
    previous_sha: Option<&str>,
) -> Result<CiConfig> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let tree = tree_at_commit(&repo, commit_sha)?;

    // Try Gitea Actions format first
    let gitea = try_read_gitea_workflows(
        &repo,
        repository,
        commit_sha,
        ref_name,
        event,
        base_branch,
        previous_sha,
    )?;
    let workflows_untriggered = matches!(gitea, GiteaWorkflows::NoneTriggered);
    if let GiteaWorkflows::Config(config) = gitea {
        tracing::info!("Using Gitea Actions workflow from {}/", WORKFLOW_DIR);
        return Ok(config);
    }

    // Fall back to the native CI config.
    let ci_filename = [".forgekeep-ci.yml"]
        .into_iter()
        .find_map(|name| {
            tree.lookup_entry_by_path(name)
                .with_context(|| format!("failed to look up {name} at commit {commit_sha}"))
                .transpose()
                .map(|entry| entry.map(|_| name))
        })
        .transpose()?
        .ok_or_else(|| {
            // Workflows that exist but sit out this event are not "no config":
            // saying so is the difference between fixing an `on:` filter and
            // hunting for a file that is already there.
            if workflows_untriggered {
                rg_core::error::invalid_request(format!(
                    "no workflow in {}/ is triggered by event {} on {}, and no native CI config (.forgekeep-ci.yml) at commit {}",
                    WORKFLOW_DIR,
                    event,
                    ref_name,
                    commit_sha
                ))
            } else {
                rg_core::error::invalid_request(format!(
                    "no CI config found (.gitea/workflows/*.yml or .forgekeep-ci.yml) at commit {}",
                    commit_sha
                ))
            }
        })?;

    let entry = tree
        .lookup_entry_by_path(ci_filename)
        .with_context(|| format!("failed to look up {ci_filename} at commit {commit_sha}"))?
        .expect("the CI config was found above");
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
    // whole file here used to bury the actual complaint. `serde_yaml` reports
    // only a line/column and its own complaint — no filesystem path — so it is
    // safe to hand over (H-05).
    let config: CiConfig = serde_yaml::from_str(&ci_yml).map_err(|e| {
        rg_core::error::invalid_request(format!("failed to parse {ci_filename}: {e}"))
    })?;

    Ok(config)
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

/// Outcome of looking for Gitea Actions workflows at a commit.
enum GiteaWorkflows {
    /// The commit has no `.gitea/workflows` directory at all.
    Absent,
    /// The directory is there, but no workflow in it is triggered by this event.
    NoneTriggered,
    /// Merged configuration of every workflow triggered by this event.
    Config(CiConfig),
}

/// Try to find and parse Gitea Actions workflow files in `.gitea/workflows/`.
///
/// `Absent` / `NoneTriggered` are the two legitimate fallbacks to the native
/// `.forgekeep-ci.yml`, and they are told apart so the caller can explain which
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
    event: &str,
    base_branch: Option<&str>,
    previous_sha: Option<&str>,
) -> Result<GiteaWorkflows> {
    let Some(workflow_sources) = load_workflow_sources(repo, commit_sha)? else {
        // Nothing at that path in this commit: the native format is next in line.
        return Ok(GiteaWorkflows::Absent);
    };

    let match_branch = event_match_branch(repo, base_branch)?;
    let changed = gitea_actions::ChangedPaths::of_commit(repo, previous_sha, commit_sha);

    let mut all_jobs: std::collections::HashMap<String, config::JobConfig> =
        std::collections::HashMap::new();
    let mut all_stages: Vec<String> = Vec::new();
    // `(workflow file, resolved concurrency)` for every triggered workflow that
    // declared a `concurrency:` block. See `merge_concurrency` for why the list
    // is kept rather than folded as it goes.
    let mut declared_concurrency: Vec<(String, config::ConcurrencyConfig)> = Vec::new();

    for (name, yml) in sorted_workflows(&workflow_sources) {
        // The cause is folded into the message instead of being a `with_context`
        // source: callers log this error with `Display`, and the YAML line/column
        // is the whole point of reporting it. All three failures below are the
        // committed workflow being wrong — an unparseable file, a reusable
        // workflow that does not resolve, a feature this engine cannot run — so
        // they carry `InvalidRequest` and reach the client as a 400 that names
        // the file, instead of the sanitized 500 they used to produce.
        let workflow = gitea_actions::GiteaWorkflow::parse(yml).map_err(|e| {
            rg_core::error::invalid_request(format!("failed to parse {WORKFLOW_DIR}/{name}: {e}"))
        })?;

        // Before the match, not after it: a workflow that asks for an event
        // nothing emits never matches, so a check placed below would be the one
        // thing it can never reach — which is exactly how `on: schedule` came to
        // be accepted and silently never run (card_c8f24edaee89).
        workflow.validate_supported_triggers().map_err(|e| {
            rg_core::error::invalid_request(format!(
                "unsupported trigger in {WORKFLOW_DIR}/{name}: {e:#}"
            ))
        })?;

        // Check if this workflow should be triggered
        if !workflow.matches_event(event, ref_name, &match_branch, &changed) {
            continue;
        }
        let workflow = workflow
            .expand_local_reusable_workflows(&workflow_sources)
            .map_err(|e| {
                rg_core::error::invalid_request(format!(
                    "failed to expand {WORKFLOW_DIR}/{name}: {e:#}"
                ))
            })?;
        workflow.validate_supported_actions().map_err(|e| {
            rg_core::error::invalid_request(format!(
                "unsupported workflow {WORKFLOW_DIR}/{name}: {e:#}"
            ))
        })?;

        tracing::info!("Triggering workflow from {}/{}", WORKFLOW_DIR, name);

        let ctx = gitea_actions::WorkflowContext {
            ref_name: ref_name.to_string(),
            sha: commit_sha.to_string(),
            event: event.to_string(),
            repo_owner: repository.owner.to_string(),
            repo_name: repository.name.to_string(),
        };

        let mut wf_config = workflow.to_ci_config(&ctx);

        // `concurrency.group` is expanded here rather than at trigger time: it
        // is built from `${{ github.* }}`, and only this loop still holds the
        // workflow that `github.workflow` names.
        if let Some(mut concurrency) = wf_config.concurrency.take() {
            let label = workflow
                .name
                .clone()
                .unwrap_or_else(|| workflow_prefix(name).to_string());
            concurrency.group =
                gitea_actions::expand_concurrency_group(&concurrency.group, &ctx, &label);
            declared_concurrency.push((name.clone(), concurrency));
        }

        // Prefix job names with workflow filename to avoid collisions
        let wf_prefix = workflow_prefix(name);
        let mut renamed_jobs = std::collections::HashMap::new();
        for (job_name, mut job) in wf_config.jobs {
            let new_name = format!("{}/{}", wf_prefix, job_name);
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
            event,
            ref_name
        );
        return Ok(GiteaWorkflows::NoneTriggered);
    }

    Ok(GiteaWorkflows::Config(CiConfig {
        stages: Some(all_stages),
        concurrency: merge_concurrency(declared_concurrency)?,
        jobs: all_jobs,
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
                 ForgeKeep runs every workflow triggered by one event as a single pipeline, which \
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

    let object = workflow_dir
        .object()
        .with_context(|| format!("failed to read {} at commit {}", WORKFLOW_DIR, commit_sha))?;
    // Shape of the committed tree, not a storage failure: the client put a file
    // where the workflow directory belongs, and only the client can move it.
    let tree = object.try_into_tree().map_err(|_| {
        rg_core::error::invalid_request(format!(
            "{WORKFLOW_DIR} exists at commit {commit_sha} but is a file, not a directory"
        ))
    })?;

    let mut workflow_sources = std::collections::HashMap::new();
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
/// The event query in the shape the tests ask it: no previous revision, so a
/// path filter falls back to the commit's own diff.
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
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let Some(sources) = load_workflow_sources(&repo, commit_sha)? else {
        return Ok(false);
    };
    let match_branch = event_match_branch(&repo, base_branch)?;
    let changed = gitea_actions::ChangedPaths::of_commit(&repo, previous_sha, commit_sha);

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
                user: "forgekeep".into(),
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

    #[test]
    fn the_completion_path_hands_its_hooks_the_process_hub_and_smtp() {
        let (recorder, notifications) = wiring();

        let context = post_push_context(
            std::path::Path::new("/srv/repos"),
            false,
            false,
            false,
            None,
            None,
            None,
            &notifications,
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

    /// The merge commit gets a pipeline of its own, and that pipeline can unblock
    /// the next merge, so the wiring has to survive an arbitrary number of hops
    /// rather than only the first. This pins [`nested_engine`]'s own contract;
    /// that `post_push_context` feeds it the *real* wiring is pinned by the
    /// notifier assertion above (both read the same argument).
    #[test]
    fn the_engine_the_hooks_trigger_through_keeps_the_same_wiring() {
        let (recorder, notifications) = wiring();

        let mut engine = nested_engine(&notifications);
        for hop in 1..=3 {
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
            engine = nested_engine(&engine.notifications);
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
        let context = post_push_context(
            std::path::Path::new("/srv/repos"),
            false,
            false,
            false,
            None,
            None,
            None,
            &CiEngine::new().notifications,
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
            action_templates: None,
        }
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
            temp.path().join(".forgekeep-ci.yml"),
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
            .run(&["add", ".forgekeep-ci.yml"], Some(temp.path()))
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
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
            rg_db::ops::pipeline_ops::find_pending_job_matching_labels(&db, &[])
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
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/dev",
                trigger_type: "push",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
            temp.path().join(".forgekeep-ci.yml"),
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
            .run(&["add", ".forgekeep-ci.yml"], Some(temp.path()))
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
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
            rg_db::ops::pipeline_ops::find_pending_job_matching_labels(&db, &[])
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
            &tx, stage.id, "first", "echo one", None, None, None, None, None, false, None, None,
            None,
        )
        .await
        .unwrap();
        assert!(
            rg_db::ops::pipeline_ops::find_pending_job_matching_labels(&db, &[])
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
        let (temp, sha) = commit_repo(&[(".forgekeep-ci.yml", config)]);
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
            repo_path,
            repo_id,
            commit_sha: sha,
            ref_name: "refs/heads/main",
            trigger_type,
            base_branch: None,
            previous_sha: None,
            triggered_by: Some(user_id),
            docker_enabled: false,
            external_runners: true,
            allow_host_runner: false,
            jwt_secret: Some("secret"),
            encryption_key: Some("secret"),
            external_url: None,
        }
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
            ".forgekeep-ci.yml",
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
        let in_progress = rg_db::ops::pipeline_ops::create_pipeline_in_group(
            &db,
            repo.id,
            &sha,
            "refs/heads/main",
            "push",
            Some(user.id),
            Some("refs/heads/main"),
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
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
            ".forgekeep-ci.yml",
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
        let in_progress = rg_db::ops::pipeline_ops::create_pipeline_in_group(
            &db,
            repo.id,
            &sha,
            "refs/heads/main",
            "push",
            Some(user.id),
            Some("refs/heads/main"),
        )
        .await
        .unwrap();

        let error = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "manual",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/other",
                trigger_type: "manual",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
            ".forgekeep-ci.yml",
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
                        repo_path: &path,
                        repo_id,
                        commit_sha: &sha,
                        ref_name,
                        trigger_type: "push",
                        base_branch: None,
                        previous_sha: None,
                        triggered_by: Some(user_id),
                        docker_enabled: false,
                        external_runners: true,
                        allow_host_runner: false,
                        jwt_secret: Some("secret"),
                        encryption_key: Some("secret"),
                        external_url: None,
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
                rg_db::ops::pipeline_ops::create_pipeline_in_group(
                    &db,
                    repo_id,
                    &sha,
                    ref_name,
                    "push",
                    Some(user_id),
                    Some(group),
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
            "on: workflow_call\njobs:\n  build:\n    steps:\n      - run: echo '${{ inputs.target }}'\n",
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
    #[test]
    fn every_condition_key_the_validator_accepts_is_answered_by_both_contexts() {
        let job = job_condition_context(
            "refs/heads/main",
            "push",
            "abc123",
            RepositoryName {
                owner: "acme",
                name: "api",
            },
            &std::collections::BTreeMap::new(),
            &config::JobConfig {
                stage: None,
                script: vec!["echo ok".into()],
                image: None,
                only: None,
                variables: None,
                when: None,
                condition: None,
                environment: None,
                allow_failure: None,
                timeout_seconds: None,
                tags: None,
                matrix: None,
                cache: None,
                action_templates: None,
            },
        );
        let step = gitea_actions::actions_condition_context(
            &gitea_actions::WorkflowContext {
                ref_name: "refs/heads/main".into(),
                sha: "abc123".into(),
                event: "push".into(),
                repo_owner: "acme".into(),
                repo_name: "api".into(),
            },
            &std::collections::HashMap::new(),
        );

        for key in condition::GITHUB_CONTEXT_KEYS {
            for (label, context) in [("job", &job), ("step", &step)] {
                let value = context.get(key).unwrap_or_else(|| {
                    panic!(
                        "the validator accepts `{key}` in an `if:` condition, but the {label} \
                         context has no answer for it — the condition would compare against the \
                         empty string and report a plain false"
                    )
                });
                assert!(
                    !value.is_empty(),
                    "the {label} context answers `{key}` with the empty string, which is what a \
                     `// filled later` placeholder looks like from a condition"
                );
            }
        }

        for (label, context) in [("job", &job), ("step", &step)] {
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
                repo_path: repo_dir.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(org_owner.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
        // the `.forgekeep-ci.yml` fallback below would otherwise hide the typo.
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/ci.yml",
                b"on: push\njobs:\n  build:\n   steps:\n  - run: echo broken\n" as &[u8],
            ),
            (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
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
            (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
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

    #[test]
    fn broken_native_config_reports_the_reason_not_the_whole_file() {
        let (temp, sha) = commit_repo(&[(
            ".forgekeep-ci.yml",
            b"build:\n  script: [echo ok]\n   nested: bad\n" as &[u8],
        )]);

        let error =
            read_ci_config_for_test(temp.path(), &sha, "refs/heads/main", "push", None, None)
                .unwrap_err();
        let rendered = format!("{error}");
        assert!(
            rendered.contains(".forgekeep-ci.yml") && rendered.contains("line"),
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
            ".forgekeep-ci.yml",
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
            (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
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
            ".forgekeep-ci.yml",
            b"build:\n  script: [echo native]\n" as &[u8],
        )]);
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let object_id = git
            .run(
                &["rev-parse", &format!("{sha}:.forgekeep-ci.yml")],
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
            (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
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
            rendered.contains("failed to read .gitea/workflows/ci.yml from the object database"),
            "{rendered}"
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
                    repo_path,
                    repo_id: 1,
                    commit_sha: sha,
                    ref_name: "refs/heads/main",
                    trigger_type: "manual",
                    base_branch: None,
                    previous_sha: None,
                    triggered_by: Some(1),
                    docker_enabled: false,
                    external_runners: true,
                    allow_host_runner: false,
                    jwt_secret: Some("secret"),
                    encryption_key: Some("secret"),
                    external_url: None,
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
            ".forgekeep-ci.yml",
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
            ".forgekeep-ci.yml",
            b"build:\n  script: [echo ok\n" as &[u8],
        )]);
        let error = trigger(broken.path(), &broken_sha).await;
        let invalid = error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .unwrap_or_else(|| panic!("unparseable YAML must be a 400, got: {error:#}"));
        let message = invalid.to_string();
        assert!(
            message.contains(".forgekeep-ci.yml"),
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
            ".forgekeep-ci.yml",
            b"build:\n  script: [echo ok]\n" as &[u8],
        )]);
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let object_id = git
            .run(
                &["rev-parse", &format!("{dangling_sha}:.forgekeep-ci.yml")],
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
                (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
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

    /// card_e949057aaa0d, on the path a push actually takes: an input nothing
    /// consumes must stop the trigger, not produce a job that ran none of what
    /// the workflow asked for.
    ///
    /// The repository also carries a native `.forgekeep-ci.yml`, so a refusal
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
            (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
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
                repo_path: temp.path(),
                repo_id: 1,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(1),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
    /// serialize on the template text. `${{ github.head_ref || github.ref }}`
    /// is the recipe GitHub's own documentation gives for cancelling superseded
    /// PR runs, and the `||` is beyond this evaluator.
    #[test]
    fn an_unresolved_group_expression_is_refused_not_serialized_on() {
        let error = resolved_concurrency_group(
            Some(&config::ConcurrencyConfig {
                group: "ci-${{ github.head_ref || github.ref }}".to_string(),
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
            ".forgekeep-ci.yml",
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

    #[test]
    fn manual_jobs_are_supported_and_invalid_execution_policies_fail_closed() {
        let mut job = config(BTreeMap::new());
        job.when = Some("manual".into());
        let config = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job.clone())]),
        };
        validate_execution_semantics(&config).unwrap();
        job.when = Some("delayed".into());
        let invalid_when = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job.clone())]),
        };
        assert!(validate_execution_semantics(&invalid_when).is_err());
        job.when = None;
        job.timeout_seconds = Some(0);
        let config = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job)]),
        };
        assert!(validate_execution_semantics(&config).is_err());
    }

    fn config_with_timeout(timeout: Option<i64>) -> CiConfig {
        let mut job = config(BTreeMap::new());
        job.timeout_seconds = timeout;
        CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job)]),
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
            ".forgekeep-ci.yml",
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
                    ".forgekeep-ci.yml",
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

    /// card_4223cbf9a0a1: a declaration below `jobs.<name>` must either be
    /// translated or fail where the committed workflow is read. Serde normally
    /// discards every field a struct does not name, so these three workflows
    /// used to create runnable jobs after silently losing `services`,
    /// `strategy.fail-fast`, or `container.credentials`.
    #[test]
    fn unknown_gitea_job_keys_are_reported_with_their_file_and_key() {
        for (key, job_body) in [
            (
                "services",
                "    services:\n      postgres:\n        image: postgres:17\n",
            ),
            (
                "fail-fast",
                "    strategy:\n      fail-fast: false\n      matrix:\n        os: [linux]\n",
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
                    ".forgekeep-ci.yml",
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
                "permissions",
                "on: push\npermissions:\n  contents: read\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
                &["name", "on", "jobs", "concurrency", "env", "defaults"] as &[&str],
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
            !serde_yaml::to_string(&config).unwrap().contains("${{"),
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
        assert!(!serde_yaml::to_string(&config).unwrap().contains("${{"));
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
        assert!(!serde_yaml::to_string(&config).unwrap().contains("${{"));
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
            let environment = rg_db::ops::ci_environment_ops::create(
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
            )
            .await
            .unwrap();
            protected_environment_ids.insert(name, environment.id);
        }
        let pipeline_id = trigger_pipeline(
            TriggerPipelineParams {
                db: &db,
                repo_path: temp.path(),
                repo_id: repo.id,
                commit_sha: &sha,
                ref_name: "refs/heads/main",
                trigger_type: "push",
                base_branch: None,
                previous_sha: None,
                triggered_by: Some(user.id),
                docker_enabled: false,
                external_runners: true,
                allow_host_runner: false,
                jwt_secret: Some("secret"),
                encryption_key: Some("secret"),
                external_url: None,
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
    /// workflow's jobs, while ForgeKeep flattens those jobs into the caller's
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
            ".forgekeep-ci.yml",
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
            message.contains(".forgekeep-ci.yml") && message.contains("retry"),
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
            let (temp, sha) = commit_repo(&[(".forgekeep-ci.yml", yaml.as_bytes())]);

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
                message.contains(".forgekeep-ci.yml") && message.contains(key),
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
    fn commit_again(temp: &tempfile::TempDir, files: &[(&str, &[u8])]) -> (String, String) {
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
        assert!(m("a.rs", "?.rs"));
        assert!(!m("ab.rs", "?.rs"));
        assert!(m("exact/path.rs", "exact/path.rs"));
    }
}
