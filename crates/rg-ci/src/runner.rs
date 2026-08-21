//! Pipeline runner — executes CI jobs sequentially by stage.
//!
//! Supports two execution modes:
//! - **Local**: `sh -c` (default, when no `image` is specified)
//! - **Docker**: `docker run --rm <image> sh -c` (when `image` field is set)
//!
//! Security measures:
//! - All job executions are bounded by a configurable timeout (default 1 hour).
//! - When Docker is requested but unavailable, the job **fails** instead of
//!   silently falling back to local execution (prevents privilege escalation).
//! - Local execution sanitizes the environment to avoid leaking sensitive vars.
//! - On timeout the child process is killed (not just abandoned).

use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;

use rg_db::ops::pipeline_ops;

/// A healthy embedded job must report liveness well inside the HTTP watchdog's
/// ten-minute stale window. External runners do the same through their 30s
/// runner heartbeat.
const JOB_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Poll an execution future and a scoped heartbeat loop together. The loop is
/// canceled as soon as execution completes, so no detached task can keep a
/// settled job alive. The callback seam keeps the timing contract directly
/// testable without a real 30-second wait.
async fn with_job_heartbeat<T, Execution, Heartbeat, HeartbeatFuture>(
    execution: Execution,
    interval: std::time::Duration,
    mut heartbeat: Heartbeat,
) -> T
where
    Execution: std::future::Future<Output = T>,
    Heartbeat: FnMut() -> HeartbeatFuture,
    HeartbeatFuture: std::future::Future<Output = ()>,
{
    let heartbeat_loop = async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `interval` ticks immediately once; the start transition already
        // stamped liveness, so the first refresh belongs one full period later.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            heartbeat().await;
        }
    };

    tokio::pin!(execution);
    tokio::pin!(heartbeat_loop);
    tokio::select! {
        output = &mut execution => output,
        () = &mut heartbeat_loop => unreachable!("job heartbeat loop is infinite"),
    }
}

/// Hard cap on the number of processes a job container may spawn (fork-bomb guard).
const DOCKER_PIDS_LIMIT: &str = "512";
/// Default memory ceiling for a job container.
const DOCKER_MEMORY_LIMIT: &str = "2g";
/// Default CPU quota for a job container.
const DOCKER_CPU_LIMIT: &str = "2";

/// Default CI token scopes for jobs.
/// Grants read access to the triggering repo and packages.
const DEFAULT_CI_TOKEN_SCOPES: &str = "repo:read packages:read";

/// Outcome of running one pipeline stage.
enum StageOutcome {
    /// The stage finished; the bool is whether it failed (a non-`allow_failure`
    /// job failed or errored).
    Completed(bool),
    /// The pipeline was paused mid-stage — a `manual` job or a job awaiting
    /// environment approval — so no further stages should run.
    Paused,
    /// The pipeline settled behind this runner's back — a cancellation landed
    /// while the stage was executing. Nothing more may be written for it: the
    /// server has already answered `canceled` to whoever asked.
    Settled,
}

/// Pipeline runner that executes stages/jobs sequentially.
pub struct PipelineRunner {
    db: DatabaseConnection,
    repo_path: std::path::PathBuf,
    pipeline_id: i64,
    repo_id: i64,
    /// Signs `CI_JOB_TOKEN`. Not the key the repository's CI secrets are
    /// encrypted with — see [`PipelineRunner::set_encryption_key`].
    jwt_secret: Option<String>,
    /// Opens `ci_secrets.encrypted_value`. Split from `jwt_secret` by
    /// card_d740512de0a8: they were one value, so rotating the token-signing
    /// secret made every stored CI secret undecryptable and each job failed on
    /// its own with "failed to decrypt CI secret".
    encryption_key: Option<String>,
    docker_enabled: bool,
    /// Whether a job without an `image:` may execute as a shell directly on the
    /// host. Defaults to `false` (secure): imageless jobs are refused so that
    /// pushed CI config cannot run arbitrary code on the server. Enabled only on
    /// trusted single-tenant instances via `ci.allow_host_runner = true`.
    allow_host_runner: bool,
    oidc_token_url: Option<String>,
    /// Per-job timeout in seconds (0 = no timeout).
    pub(crate) job_timeout_secs: u64,
    /// Hub and SMTP wiring for the post-push hooks a successful pipeline
    /// spawns. Default (both `None`) keeps every storage-side effect and drops
    /// only the real-time events and the mail — see [`crate::CiNotifications`].
    notifications: crate::CiNotifications,
}

impl PipelineRunner {
    pub fn new(db: DatabaseConnection, repo_path: &std::path::Path, pipeline_id: i64) -> Self {
        Self::new_with_job_timeout(
            db,
            repo_path,
            pipeline_id,
            rg_core::ci::DEFAULT_JOB_TIMEOUT_SECS,
        )
    }

    pub fn new_with_job_timeout(
        db: DatabaseConnection,
        repo_path: &std::path::Path,
        pipeline_id: i64,
        job_timeout_secs: u64,
    ) -> Self {
        Self {
            db,
            repo_path: repo_path.to_path_buf(),
            pipeline_id,
            repo_id: 0,
            jwt_secret: None,
            encryption_key: None,
            docker_enabled: true,
            allow_host_runner: false,
            oidc_token_url: None,
            job_timeout_secs,
            notifications: crate::CiNotifications::default(),
        }
    }

    /// Create a runner with Docker disabled (local-only mode).
    pub fn new_local_only(
        db: DatabaseConnection,
        repo_path: &std::path::Path,
        pipeline_id: i64,
    ) -> Self {
        Self::new_local_only_with_job_timeout(
            db,
            repo_path,
            pipeline_id,
            rg_core::ci::DEFAULT_JOB_TIMEOUT_SECS,
        )
    }

    pub fn new_local_only_with_job_timeout(
        db: DatabaseConnection,
        repo_path: &std::path::Path,
        pipeline_id: i64,
        job_timeout_secs: u64,
    ) -> Self {
        Self {
            db,
            repo_path: repo_path.to_path_buf(),
            pipeline_id,
            repo_id: 0,
            jwt_secret: None,
            encryption_key: None,
            docker_enabled: false,
            allow_host_runner: false,
            oidc_token_url: None,
            job_timeout_secs,
            notifications: crate::CiNotifications::default(),
        }
    }

    /// Set the repository ID (for CI_JOB_TOKEN generation).
    pub fn set_repo_id(&mut self, repo_id: i64) {
        self.repo_id = repo_id;
    }

    /// Allow (or forbid) executing imageless jobs as a shell on the host.
    ///
    /// Off by default. Turn on only for trusted, single-tenant deployments via
    /// `ci.allow_host_runner = true`; on shared/public instances leaving it off
    /// forces every job into a Docker sandbox or an external runner.
    pub fn set_allow_host_runner(&mut self, allow: bool) {
        self.allow_host_runner = allow;
    }

    /// Set the JWT secret (for CI_JOB_TOKEN generation).
    /// If not set, CI_JOB_TOKEN will not be provided.
    pub fn set_jwt_secret(&mut self, secret: String) {
        self.jwt_secret = Some(secret);
    }

    /// Set the at-rest encryption key (for decrypting the repository's CI
    /// secrets). If not set, no repository secrets are injected into jobs.
    pub fn set_encryption_key(&mut self, secret: String) {
        self.encryption_key = Some(secret);
    }

    pub fn set_oidc_token_url(&mut self, url: String) {
        self.oidc_token_url = Some(url);
    }

    /// Wire this runner's post-success hooks to the process's notification hub
    /// and SMTP configuration, so a merge this pipeline unblocks is as visible
    /// as the same merge made over REST (card_85b8d59246b5).
    pub fn set_notifications(&mut self, notifications: crate::CiNotifications) {
        self.notifications = notifications;
    }

    /// Run the pipeline: iterate stages in order, run jobs in each stage.
    ///
    /// If a non-allowed job in a stage fails, subsequent stages are skipped.
    pub async fn run(&self) -> Result<()> {
        if let Err(error) = self.prepare_workspace().await {
            let now = chrono::Utc::now().naive_utc();
            if let Err(update_error) = pipeline_ops::settle_pipeline_if_active(
                &self.db,
                self.pipeline_id,
                "failed",
                Some(now),
                Some(now),
            )
            .await
            {
                tracing::error!(pipeline_id = self.pipeline_id, %update_error, "failed to mark pipeline failed after workspace error");
            }
            return Err(error);
        }
        let result = self.run_pipeline().await;
        if let Err(error) = self.cleanup_workspace().await {
            tracing::warn!(
                pipeline_id = self.pipeline_id,
                error = %format!("{error:#}"),
                "failed to clean CI workspace"
            );
        }
        result
    }

    async fn run_pipeline(&self) -> Result<()> {
        let now = chrono::Utc::now().naive_utc();

        // Mark pipeline as running
        let pipeline_started_at = pipeline_ops::get_pipeline(&self.db, self.pipeline_id)
            .await?
            .filter(|pipeline| pipeline.started_at.is_none())
            .map(|_| now);
        if !pipeline_ops::settle_pipeline_if_active(
            &self.db,
            self.pipeline_id,
            "running",
            pipeline_started_at,
            None,
        )
        .await?
        {
            tracing::info!(
                pipeline_id = self.pipeline_id,
                "pipeline already settled before the runner started it — not running"
            );
            return Ok(());
        }

        // Get stages in order
        let stages = pipeline_ops::list_stages_by_pipeline(&self.db, self.pipeline_id).await?;

        let mut pipeline_failed = false;

        for stage in &stages {
            if matches!(stage.status.as_str(), "success" | "skipped") {
                continue;
            }
            if matches!(
                stage.status.as_str(),
                "failed" | "failure" | "error" | "canceled"
            ) {
                pipeline_failed = true;
                continue;
            }
            if pipeline_failed {
                self.skip_stage(stage).await?;
                continue;
            }

            match self.run_stage(stage).await? {
                StageOutcome::Paused | StageOutcome::Settled => return Ok(()),
                StageOutcome::Completed(stage_failed) => {
                    if stage_failed {
                        pipeline_failed = true;
                    }
                }
            }
        }

        // Mark pipeline as completed. Conditional: a cancellation that landed
        // while the last stage ran already answered `canceled`, and rewriting
        // it here would both contradict that answer and release the success
        // followups below.
        let pipeline_end = chrono::Utc::now().naive_utc();
        let pipeline_status = if pipeline_failed { "failed" } else { "success" };
        if !pipeline_ops::settle_pipeline_if_active(
            &self.db,
            self.pipeline_id,
            pipeline_status,
            None,
            Some(pipeline_end),
        )
        .await?
        {
            tracing::info!(
                pipeline_id = self.pipeline_id,
                would_be = pipeline_status,
                "pipeline settled while the runner was executing — keeping the recorded status"
            );
            return Ok(());
        }

        if pipeline_status == "success" {
            self.run_success_followups().await?;
        }

        tracing::info!(
            pipeline_id = self.pipeline_id,
            status = pipeline_status,
            "Pipeline completed"
        );

        Ok(())
    }

    /// Mark a stage and all of its jobs as `skipped`. Used once an earlier
    /// stage has already failed the pipeline.
    async fn skip_stage(&self, stage: &rg_db::entities::pipeline_stage::Model) -> Result<()> {
        pipeline_ops::settle_stage_if_active(&self.db, stage.id, "skipped", None, None).await?;
        let jobs = pipeline_ops::list_jobs_by_stage(&self.db, stage.id).await?;
        for job in jobs {
            pipeline_ops::settle_job_if_active(&self.db, job.id, "skipped", None, None, None)
                .await?;
        }
        Ok(())
    }

    /// Run every runnable job in one stage in order, updating stage/job status.
    ///
    /// Returns [`StageOutcome::Paused`] the moment a `manual` job or a job
    /// awaiting environment approval is reached (the pipeline stops there),
    /// [`StageOutcome::Settled`] when the pipeline was canceled mid-stage,
    /// otherwise [`StageOutcome::Completed`] carrying whether the stage failed.
    async fn run_stage(
        &self,
        stage: &rg_db::entities::pipeline_stage::Model,
    ) -> Result<StageOutcome> {
        // Mark stage as running
        let stage_start = chrono::Utc::now().naive_utc();
        if !pipeline_ops::settle_stage_if_active(
            &self.db,
            stage.id,
            "running",
            stage.started_at.is_none().then_some(stage_start),
            None,
        )
        .await?
        {
            return Ok(StageOutcome::Settled);
        }

        let mut stage_failed = false;
        let jobs = pipeline_ops::list_jobs_by_stage(&self.db, stage.id).await?;

        for job in &jobs {
            // `jobs` is a snapshot taken before the first job ran. A
            // cancellation that lands mid-stage is invisible in it, so the
            // liveness of the pipeline is re-read per job rather than assumed
            // for the whole loop.
            if !pipeline_ops::pipeline_is_active(&self.db, self.pipeline_id).await? {
                tracing::info!(
                    pipeline_id = self.pipeline_id,
                    stage_id = stage.id,
                    "pipeline settled mid-stage — stopping before the next job"
                );
                return Ok(StageOutcome::Settled);
            }
            if matches!(job.status.as_str(), "success" | "skipped" | "canceled") {
                continue;
            }
            if matches!(job.status.as_str(), "failed" | "failure" | "error") {
                if !job.allow_failure {
                    stage_failed = true;
                }
                continue;
            }
            if job.status == "manual" {
                if !pipeline_ops::settle_stage_if_active(&self.db, stage.id, "manual", None, None)
                    .await?
                {
                    return Ok(StageOutcome::Settled);
                }
                pipeline_ops::settle_pipeline_if_active(
                    &self.db,
                    self.pipeline_id,
                    "manual",
                    None,
                    None,
                )
                .await?;
                tracing::info!(
                    pipeline_id = self.pipeline_id,
                    job_id = job.id,
                    "Pipeline paused at manual job"
                );
                return Ok(StageOutcome::Paused);
            }
            if job.status == "waiting_approval" {
                if !pipeline_ops::settle_stage_if_active(
                    &self.db,
                    stage.id,
                    "waiting_approval",
                    None,
                    None,
                )
                .await?
                {
                    return Ok(StageOutcome::Settled);
                }
                pipeline_ops::settle_pipeline_if_active(
                    &self.db,
                    self.pipeline_id,
                    "waiting_approval",
                    None,
                    None,
                )
                .await?;
                tracing::info!(
                    pipeline_id = self.pipeline_id,
                    job_id = job.id,
                    "Pipeline paused for environment approval"
                );
                return Ok(StageOutcome::Paused);
            }
            if job.status != "pending" {
                anyhow::bail!(
                    "job {} cannot be resumed from unexpected status '{}'",
                    job.id,
                    job.status
                );
            }
            if self.run_and_record_job(job).await {
                stage_failed = true;
            }
        }

        let stage_end = chrono::Utc::now().naive_utc();
        let stage_status = if stage_failed { "failed" } else { "success" };
        if !pipeline_ops::settle_stage_if_active(
            &self.db,
            stage.id,
            stage_status,
            None,
            Some(stage_end),
        )
        .await?
        {
            return Ok(StageOutcome::Settled);
        }

        Ok(StageOutcome::Completed(stage_failed))
    }

    /// Execute a single pending job and persist its result. Returns `true`
    /// when the job failed in a way that should fail the stage (a non-
    /// `allow_failure` non-zero exit or an execution error). Persisting the
    /// result is best-effort: DB errors are logged, not propagated.
    ///
    /// The write is conditional. `job` is a snapshot taken before execution
    /// started, so a cancellation that landed during the run is not visible in
    /// it; [`pipeline_ops::settle_job_if_active`] refuses to move a row that
    /// already settled as something else, and the refusal is logged rather
    /// than silently dropped.
    async fn run_and_record_job(&self, job: &rg_db::entities::pipeline_job::Model) -> bool {
        let job_result = self
            .run_job(
                job.id,
                &job.script,
                job.image.as_deref(),
                job.variables.as_deref(),
                job.cache_key.as_deref(),
                job.cache_paths.as_deref(),
                job.artifacts.as_deref(),
                job.timeout_seconds,
            )
            .await;

        let (status, exit_code, log, stage_failed) = match job_result {
            Ok((exit_code, log)) => (
                if exit_code == 0 { "success" } else { "failed" },
                exit_code,
                log,
                exit_code != 0 && !job.allow_failure,
            ),
            Err(e) => {
                tracing::error!(job_id = job.id, "Job execution error: {:#}", e);
                (
                    "failed",
                    -1,
                    format!("Runner error: {}", e),
                    !job.allow_failure,
                )
            }
        };

        match pipeline_ops::settle_job_if_active(
            &self.db,
            job.id,
            status,
            Some(exit_code),
            Some(&log),
            None,
        )
        .await
        {
            Ok(true) => stage_failed,
            Ok(false) => {
                tracing::info!(
                    job_id = job.id,
                    would_be = status,
                    "job settled while the runner was executing it — result discarded"
                );
                // The stage's own status is settled too (the cascade is
                // whole-graph), so this must not push the stage to `failed`.
                false
            }
            Err(e) => {
                tracing::error!(job_id = job.id, error = %format!("{e:#}"), "Failed to update job result");
                stage_failed
            }
        }
    }

    /// After a successful pipeline, evaluate auto-merges and the merge queue
    /// for the head commit. These are best-effort side effects: failures are
    /// logged, never propagated back to the pipeline result.
    async fn run_success_followups(&self) -> Result<()> {
        let (Some(repo_root), Some(pipeline)) = (
            self.repo_path.parent().and_then(std::path::Path::parent),
            pipeline_ops::get_pipeline(&self.db, self.pipeline_id).await?,
        ) else {
            return Ok(());
        };

        self.post_push_context(repo_root)
            .evaluate_merges_and_spawn_hooks(&self.db, pipeline.repo_id, &pipeline.commit_sha, None)
            .await;

        Ok(())
    }

    /// This runner's post-push wiring, for the base-branch moves the merges it
    /// unblocks make.
    ///
    /// The merges move a base branch, and that move owes the post-push hooks — a
    /// pipeline on the merge commit, the `push` webhook, the watch fan-out
    /// (card_73a1ec5b32f3) — plus the real-time events and the mail the process
    /// handed over in [`set_notifications`](Self::set_notifications)
    /// (card_85b8d59246b5).
    fn post_push_context(
        &self,
        repo_root: &std::path::Path,
    ) -> rg_core::push_hooks::PostPushContext {
        let engine = crate::CiEngine::with_notifications_and_job_timeout(
            self.notifications.clone(),
            self.job_timeout_secs,
        );
        crate::post_push_context(
            repo_root,
            self.docker_enabled,
            false,
            self.allow_host_runner,
            self.jwt_secret.as_deref(),
            self.encryption_key.as_deref(),
            self.oidc_token_url
                .as_deref()
                .and_then(|url| url.strip_suffix("/api/v1/ci/oidc/token")),
            &engine,
        )
    }

    /// Run a single job.
    ///
    /// - If `image` is provided and Docker is available: `docker run --rm <image> sh -c <script>`
    /// - If `image` is provided but Docker is NOT available: **fail** (no silent fallback).
    /// - Otherwise: `sh -c <script>` (with timeout).
    ///
    /// Returns (exit_code, stdout+stderr output).
    #[allow(clippy::too_many_arguments)]
    async fn run_job(
        &self,
        job_id: i64,
        script: &str,
        image: Option<&str>,
        variables: Option<&str>,
        cache_key: Option<&str>,
        cache_paths: Option<&str>,
        artifacts: Option<&str>,
        timeout_seconds: Option<i64>,
    ) -> Result<(i32, String)> {
        let job_start = chrono::Utc::now().naive_utc();

        // Mark job as running
        if let Err(e) = pipeline_ops::start_job_if_active(&self.db, job_id, Some(job_start)).await {
            tracing::error!(job_id, error = %format!("{e:#}"), "Failed to update job status to running");
        }

        tracing::info!(job_id, "Running job");

        // Resolved once, so the token this job carries and the wall-clock it is
        // actually given come from the same number. They used to be derived
        // separately from the same column and could disagree.
        let timeout_secs =
            rg_core::ci::resolve_job_timeout_secs(job_id, timeout_seconds, self.job_timeout_secs);

        // Generate CI_JOB_TOKEN if we have the secret and repo_id
        let ci_job_token = if let Some(ref secret) = self.jwt_secret {
            if self.repo_id > 0 {
                rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
                    self.repo_id,
                    self.pipeline_id,
                    job_id,
                    DEFAULT_CI_TOKEN_SCOPES,
                    secret,
                    rg_core::ci::ci_job_token_ttl_secs(timeout_secs),
                )
                .ok()
            } else {
                None
            }
        } else {
            None
        };
        let (job_environment, secret_values) = self
            .job_environment(ci_job_token.as_deref(), variables)
            .await?;
        let cache = cache_spec(cache_key, cache_paths, &job_environment)?;
        // A cache failure never fails the job, so the server log is the only
        // place it would otherwise appear — and the person who has to act on it
        // is reading the job log, not the server's. Collected here and appended
        // to the job output below.
        let mut cache_notices: Vec<String> = Vec::new();
        if let Some((key, _)) = &cache {
            if let Err(error) = self.restore_cache(key).await {
                tracing::warn!(
                    job_id,
                    error = %format!("{error:#}"),
                    "CI cache restore failed; continuing without cache"
                );
                cache_notices.push(format!(
                    "CI cache restore failed; continuing without cache: {error:#}"
                ));
            }
        }

        // When an image is requested but Docker is disabled, fail immediately.
        // Silent fallback to local execution is a security risk: CI scripts
        // written for a sandboxed container would run with the server's full
        // permissions.
        if let Some(img) = image {
            if !self.docker_enabled {
                let msg = format!(
                    "Job requires Docker image '{}' but Docker is disabled on this runner. \
                     Refusing to fall back to local execution for security reasons.",
                    img
                );
                tracing::warn!(job_id, "{}", msg);
                return Err(anyhow::anyhow!("{}", msg));
            }
        }

        // A job without an `image:` would run as a shell directly on the host.
        // On shared/public instances this is disabled by default (CWE-250/CWE-269):
        // anyone able to push CI config could otherwise execute arbitrary code with
        // the server's privileges. Require a Docker sandbox or a dedicated runner.
        if image.is_none() && !self.allow_host_runner {
            let msg = "This job has no `image:` and would run directly on the host, but \
                       host-shell CI execution is disabled (ci.allow_host_runner = false). \
                       Add an `image:` to run in a sandboxed Docker container, dispatch the job \
                       to a dedicated runner with `tags:` (which needs ci.external_runners on), \
                       or enable ci.allow_host_runner on a trusted single-tenant instance.";
            tracing::warn!(job_id, "{}", msg);
            return Err(anyhow::anyhow!("{}", msg));
        }

        let exec_future = async {
            if let Some(img) = image {
                self.run_job_docker(job_id, script, img, &job_environment)
                    .await
            } else {
                self.run_job_local(script, &job_environment).await
            }
        };

        // Apply the execution timeout while independently refreshing the
        // watchdog liveness timestamp. The heartbeat future is scoped to this
        // await and disappears before the job is settled below.
        let execution = async {
            if timeout_secs > 0 {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs),
                    exec_future,
                )
                .await
                {
                    Ok(result) => result,
                    Err(_elapsed) => {
                        let msg = format!("Job timed out after {} seconds", timeout_secs);
                        tracing::warn!(job_id, "{}", msg);
                        Ok((-1, msg))
                    }
                }
            } else {
                exec_future.await
            }
        };
        let result = with_job_heartbeat(execution, JOB_HEARTBEAT_INTERVAL, || async {
            match pipeline_ops::touch_running_job(&self.db, job_id).await {
                Ok(true) => {}
                Ok(false) => tracing::debug!(
                    job_id,
                    "job heartbeat skipped because execution is no longer running"
                ),
                Err(error) => tracing::warn!(
                    job_id,
                    error = %format!("{error:#}"),
                    "failed to refresh embedded job heartbeat"
                ),
            }
        })
        .await;
        if let (Ok((0, _)), Some((key, paths))) = (&result, &cache) {
            if let Err(error) = self.save_cache(key, paths).await {
                tracing::warn!(job_id, error = %format!("{error:#}"), "CI cache save failed; job remains successful");
                cache_notices.push(format!(
                    "CI cache save failed; job remains successful: {error:#}"
                ));
            }
        }
        // Artifacts are published only for a job that succeeded: a failed run's
        // output is a partial build, and publishing it under the same name a
        // green run uses would hand whoever downloads it a broken artifact with
        // nothing in the metadata saying so.
        //
        // A publish failure does not fail the job either — the script already
        // ran and its exit code is the answer — but unlike the cache, an
        // artifact nobody can find is the *point* of the job, so the notice has
        // to reach the person reading the log rather than only the server's.
        if let Ok((0, _)) = &result {
            match artifact_spec(artifacts) {
                Ok(Some((name, paths))) => {
                    if let Err(error) = self.publish_artifact(job_id, &name, &paths).await {
                        tracing::warn!(
                            job_id,
                            artifact = %name,
                            error = %format!("{error:#}"),
                            "CI artifact publication failed; job remains successful"
                        );
                        cache_notices.push(format!(
                            "CI artifact '{name}' was not published; job remains \
                             successful: {error:#}"
                        ));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(
                        job_id,
                        error = %format!("{error:#}"),
                        "stored CI artifact declaration is unreadable; nothing was published"
                    );
                    cache_notices.push(format!(
                        "CI artifact declaration could not be read; nothing was \
                         published: {error:#}"
                    ));
                }
            }
        }
        result.map(|(code, log)| {
            let log = append_job_notices(log, &cache_notices);
            (
                code,
                rg_core::auth::encryption::mask_values(&log, &secret_values),
            )
        })
    }

    /// Execute script locally via platform-appropriate shell.
    ///
    /// The process runs with a sanitized environment (standard CI vars + PATH/LANG)
    /// plus CI_JOB_TOKEN for authenticated API access.
    async fn run_job_local(
        &self,
        script: &str,
        job_environment: &[(String, String)],
    ) -> Result<(i32, String)> {
        #[cfg(unix)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c")
                .arg(script)
                .current_dir(self.workspace_path())
                .env_clear()
                .envs(
                    job_environment
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str())),
                )
                .kill_on_drop(true); // Kill child process when the handle is dropped (timeout)
            c
        };

        #[cfg(windows)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("powershell.exe");
            c.args(&["-NoProfile", "-NonInteractive", "-Command", script])
                .current_dir(self.workspace_path())
                .env_clear()
                .envs(
                    job_environment
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str())),
                )
                .kill_on_drop(true);
            c
        };

        let output = cmd.output().await.context("failed to spawn job process")?;

        let exit_code = output.status.code().unwrap_or(-1);
        let mut log = String::new();
        if !output.stdout.is_empty() {
            log.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !log.is_empty() {
                log.push('\n');
            }
            log.push_str(&String::from_utf8_lossy(&output.stderr));
        }

        Ok((exit_code, log))
    }

    /// Execute script inside a Docker container.
    ///
    /// Uses `docker run --rm` with the specified image.
    /// The repo working directory is mounted as a volume.
    /// If Docker is unavailable, the job **fails** — no silent fallback to local.
    async fn run_job_docker(
        &self,
        job_id: i64,
        script: &str,
        image: &str,
        job_environment: &[(String, String)],
    ) -> Result<(i32, String)> {
        let workspace_path = self.workspace_path();
        let repo_path_str = workspace_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("repo path is not valid UTF-8"))?;

        // Check if Docker is available
        let docker_check = tokio::process::Command::new("docker")
            .arg("info")
            .output()
            .await
            .context("Docker not found — is docker installed and running?")?;

        if !docker_check.status.success() {
            // SECURITY: Do NOT fall back to local execution.
            // The CI script was written expecting a sandboxed container;
            // running it locally with server permissions is a privilege escalation.
            return Err(anyhow::anyhow!(
                "Docker daemon not available. Job requires image '{}' but cannot run in container. \
                 Refusing to fall back to local execution.",
                image
            ));
        }

        // Generate a unique container name
        let container_name = format!("forgekeep-job-{}", job_id);

        // Run: docker run --rm --name <name> <hardening flags> -v <repo_path>:/workspace -w /workspace <image> sh -c <script>
        //
        // SECURITY (CWE-269 privilege escalation): the container is confined so a
        // malicious job cannot break out onto the host:
        // - `--cap-drop=ALL` strips every Linux capability (no raw sockets, no mount…).
        // - `--security-opt=no-new-privileges` blocks setuid/gain-privilege via execve.
        // - `--pids-limit` / `--memory` / `--cpus` bound resource exhaustion (fork bomb, OOM).
        // The Docker socket is deliberately NOT mounted and `--privileged` is never
        // passed, so the job has no path to the daemon or host devices.
        let mut args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "--name".to_string(),
            container_name,
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--pids-limit".to_string(),
            DOCKER_PIDS_LIMIT.to_string(),
            "--memory".to_string(),
            DOCKER_MEMORY_LIMIT.to_string(),
            "--cpus".to_string(),
            DOCKER_CPU_LIMIT.to_string(),
            "-v".to_string(),
            format!("{}:/workspace", repo_path_str),
            "-w".to_string(),
            "/workspace".to_string(),
        ];
        for (key, _) in job_environment {
            if key == "HOME" {
                continue;
            }
            args.push("-e".to_string());
            // Pass only the variable name on the command line. The value is
            // inherited from the Docker CLI environment so CI_JOB_TOKEN and
            // future secrets are not exposed in the host process arguments.
            args.push(key.clone());
        }
        args.extend([
            "-e".to_string(),
            "HOME=/tmp".to_string(),
            image.to_string(),
            "sh".to_string(),
            "-c".to_string(),
            script.to_string(),
        ]);
        let mut command = tokio::process::Command::new("docker");
        command.args(&args);
        for (key, value) in job_environment {
            if key != "HOME" {
                command.env(key, value);
            }
        }
        let output = command
            .output()
            .await
            .context("failed to spawn docker run")?;

        let exit_code = output.status.code().unwrap_or(-1);
        let mut log = String::new();
        if !output.stdout.is_empty() {
            log.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !log.is_empty() {
                log.push('\n');
            }
            log.push_str(&String::from_utf8_lossy(&output.stderr));
        }

        // If docker run itself failed (e.g. image not found), provide a clear message
        if exit_code != 0 && log.is_empty() {
            log = format!(
                "Docker container exited with code {} (no output)",
                exit_code
            );
        }

        Ok((exit_code, log))
    }

    async fn job_environment(
        &self,
        ci_job_token: Option<&str>,
        variables: Option<&str>,
    ) -> Result<(Vec<(String, String)>, Vec<String>)> {
        let pipeline = pipeline_ops::get_pipeline(&self.db, self.pipeline_id)
            .await?
            .context("pipeline not found while preparing job environment")?;
        let mut env = std::collections::BTreeMap::new();
        let mut secret_values = Vec::new();
        if let Some(json) = variables {
            let configured: std::collections::HashMap<String, String> =
                serde_json::from_str(json).context("invalid stored CI job variables")?;
            for (key, value) in configured {
                if valid_environment_name(&key) && !is_reserved_ci_variable(&key) {
                    env.insert(key, value);
                } else {
                    tracing::warn!(variable = %key, "ignoring invalid or reserved CI variable");
                }
            }
        }
        if self.repo_id > 0 {
            if let Some(encryption_key) = &self.encryption_key {
                let key = rg_core::auth::encryption::derive_key(encryption_key);
                for secret in
                    rg_db::ops::ci_secret_ops::list_by_repo(&self.db, self.repo_id).await?
                {
                    if !valid_environment_name(&secret.name)
                        || is_reserved_ci_variable(&secret.name)
                    {
                        continue;
                    }
                    let value = rg_core::auth::encryption::decrypt(&secret.encrypted_value, &key)
                        .with_context(|| {
                        format!("failed to decrypt CI secret '{}'", secret.name)
                    })?;
                    secret_values.push(value.clone());
                    env.insert(secret.name, value);
                }
            }
        }
        env.insert("CI".into(), "true".into());
        env.insert("FORGEKEEP".into(), "true".into());
        env.insert("CI_PIPELINE_ID".into(), self.pipeline_id.to_string());
        env.insert("CI_COMMIT_SHA".into(), pipeline.commit_sha.clone());
        env.insert("CI_SHA".into(), pipeline.commit_sha);
        env.insert("CI_REF".into(), pipeline.ref_name);
        env.insert("CI_EVENT".into(), pipeline.trigger_type);
        let (repository_owner, repository_name) =
            rg_core::repo::service::repository_identity(&self.db, pipeline.repo_id).await?;
        env.insert(
            "CI_REPOSITORY".into(),
            format!("{repository_owner}/{repository_name}"),
        );
        env.insert("CI_REPOSITORY_OWNER".into(), repository_owner);
        if let Some(token) = ci_job_token {
            env.insert("CI_JOB_TOKEN".into(), token.to_string());
        }
        if let Some(url) = &self.oidc_token_url {
            env.insert("CI_OIDC_TOKEN_URL".into(), url.clone());
        }
        if let Ok(path) = std::env::var("PATH") {
            env.insert("PATH".into(), path);
        }
        if let Ok(lang) = std::env::var("LANG") {
            env.insert("LANG".into(), lang);
        }
        env.insert(
            "HOME".into(),
            self.workspace_path().to_string_lossy().into_owned(),
        );
        Ok((env.into_iter().collect(), secret_values))
    }

    fn workspace_path(&self) -> std::path::PathBuf {
        self.storage_root()
            .join("_ci_workspaces")
            .join(self.repo_id.to_string())
            .join(self.pipeline_id.to_string())
    }

    async fn prepare_workspace(&self) -> Result<()> {
        let pipeline = pipeline_ops::get_pipeline(&self.db, self.pipeline_id)
            .await?
            .context("pipeline not found while preparing workspace")?;
        let repo_path = self.repo_path.clone();
        let workspace = self.workspace_path();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if workspace.exists() {
                std::fs::remove_dir_all(&workspace).with_context(|| {
                    format!(
                        "failed to remove stale CI workspace `{}`",
                        workspace.display()
                    )
                })?;
            }
            if let Some(parent) = workspace.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    anyhow::anyhow!(
                        "{}",
                        rg_core::platform::fs::describe_path_error(
                            "CI workspace parent directory",
                            parent,
                            &error,
                            WORKSPACE_DIR_HINT,
                        )
                    )
                })?;
            }
            let gateway = rg_git::cli_gateway::global_gateway()
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let workspace_text = workspace.to_string_lossy().into_owned();
            let output = gateway.run(
                &[
                    "worktree",
                    "add",
                    "--detach",
                    &workspace_text,
                    &pipeline.commit_sha,
                ],
                Some(&repo_path),
            )?;
            if !output.success() {
                anyhow::bail!(
                    "failed to create CI worktree: {}",
                    output.stderr_str().trim()
                );
            }
            Ok(())
        })
        .await?
    }

    async fn cleanup_workspace(&self) -> Result<()> {
        let repo_path = self.repo_path.clone();
        let workspace = self.workspace_path();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let gateway = rg_git::cli_gateway::global_gateway()
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let workspace_text = workspace.to_string_lossy().into_owned();
            let output = gateway.run(
                &["worktree", "remove", "--force", &workspace_text],
                Some(&repo_path),
            )?;
            if !output.success() && workspace.exists() {
                std::fs::remove_dir_all(&workspace).with_context(|| {
                    format!("failed to remove CI workspace `{}`", workspace.display())
                })?;
            }
            Ok(())
        })
        .await?
    }

    /// Where this repository's cache archives live.
    fn cache_archive_dir(&self) -> std::path::PathBuf {
        self.storage_root()
            .join("_ci_cache")
            .join(self.repo_id.to_string())
    }

    /// The directory every repository on this instance lives under — the same
    /// root the HTTP process hands to `LocalBlobStorage`, so an artifact this
    /// runner publishes is found under the key the download route resolves.
    fn storage_root(&self) -> &std::path::Path {
        self.repo_path
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap_or_else(|| self.repo_path.parent().unwrap_or(&self.repo_path))
    }

    async fn restore_cache(&self, key: &str) -> Result<()> {
        let key_hash = cache_key_hash(key);
        let policy = rg_db::ops::ci_retention_ops::get_policy(&self.db, self.repo_id).await?;
        let existing =
            rg_db::ops::ci_retention_ops::find_cache_entry(&self.db, self.repo_id, &key_hash)
                .await?;
        // Publications are named per-save, so the row is the only handle on the
        // archive: no row means no cache to restore, whatever is lying in the
        // directory.
        let Some(entry) = existing else {
            return Ok(());
        };
        let Some(archive) = recorded_cache_archive(&self.cache_archive_dir(), &entry.file_path)
        else {
            anyhow::bail!("CI cache entry {} names no archive file", entry.id);
        };
        if entry.expires_at <= chrono::Utc::now() {
            remove_cache_archive(&archive, "the cache entry expired");
            rg_db::ops::ci_retention_ops::delete_cache_entry(&self.db, entry.id).await?;
            return Ok(());
        }
        if !archive.exists() {
            return Ok(());
        }
        let size = std::fs::metadata(&archive)
            .map_err(|error| cache_path_error("CI cache archive", &archive, &error))?
            .len() as i64;
        // Integrity: verify the on-disk archive against the digest recorded when
        // it was saved before unpacking it into the workspace — a poisoned or
        // corrupted cache must never inject files into the build. Legacy entries
        // carry no digest and are restored without this guard.
        let digest = hash_archive(&archive)
            .map_err(|error| cache_path_error("CI cache archive", &archive, &error))?;
        if let Some(expected) = entry.sha256.as_deref() {
            if digest != expected {
                anyhow::bail!(
                    "CI cache integrity check failed: expected sha256 {expected}, got {digest}"
                );
            }
        }
        let workspace = self.workspace_path();
        let file = std::fs::File::open(&archive)
            .map_err(|error| cache_path_error("CI cache archive", &archive, &error))?;
        tar::Archive::new(file)
            .unpack(&workspace)
            .with_context(|| {
                format!(
                    "failed to unpack CI cache archive `{}` into workspace `{}`",
                    archive.display(),
                    workspace.display()
                )
            })?;
        rg_db::ops::ci_retention_ops::upsert_cache_entry(
            &self.db,
            self.repo_id,
            &key_hash,
            archive.to_string_lossy().as_ref(),
            size,
            Some(&digest),
            policy.cache_retention_days,
        )
        .await?;
        Ok(())
    }

    async fn save_cache(&self, key: &str, paths: &[String]) -> Result<()> {
        let key_hash = cache_key_hash(key);
        let directory = self.cache_archive_dir();
        // What the live entry names before this save rewrites it, read while the
        // row still points at the previous run's archive.
        let replaced =
            rg_db::ops::ci_retention_ops::find_cache_entry(&self.db, self.repo_id, &key_hash)
                .await?
                .map(|entry| entry.file_path);
        // Each save publishes under a name of its own. Packing over a stable
        // `<key_hash>.tar` destroyed the previous run's archive before anything
        // had confirmed this one — and then every failure below compensated by
        // deleting that same path, leaving the live row pointing at bytes that
        // no longer exist. Here the only file this save can roll back is the one
        // it created.
        let archive = directory.join(format!("{key_hash}.{}.tar", uuid::Uuid::new_v4()));

        // Nothing names this archive until `record_cache_entry` succeeds, and
        // retention walks rows — so every exit below has to take it along.
        if let Err(error) = self.pack_cache_archive(paths, &archive) {
            remove_cache_archive(&archive, "packing the cache archive failed");
            return Err(error);
        }
        if let Err(error) = self.record_cache_entry(key, &archive).await {
            remove_cache_archive(&archive, "the cache entry could not be recorded");
            return Err(error);
        }

        // The row names this save's archive now, which is what makes the one it
        // named before ours to retire — and only now. A failure above left the
        // previous run's cache exactly where it was, still restorable.
        if let Some(previous) = replaced
            .as_deref()
            .and_then(|recorded| recorded_cache_archive(&directory, recorded))
        {
            if previous != archive {
                remove_cache_archive(&previous, "a newer archive took over the cache entry");
            }
        }
        Ok(())
    }

    /// Pack `paths` from the workspace into this save's archive.
    fn pack_cache_archive(&self, paths: &[String], temporary: &std::path::Path) -> Result<()> {
        if let Some(parent) = temporary.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| cache_path_error("CI cache directory", parent, &error))?;
        }
        let file = std::fs::File::create(temporary)
            .map_err(|error| cache_path_error("CI cache archive", temporary, &error))?;
        let mut builder = tar::Builder::new(file);
        let workspace = self.workspace_path();
        for path in paths {
            let source = workspace.join(path);
            if source.is_dir() {
                builder.append_dir_all(path, &source).with_context(|| {
                    format!(
                        "failed to add directory `{}` to CI cache archive `{}`",
                        source.display(),
                        temporary.display()
                    )
                })?;
            } else if source.is_file() {
                builder
                    .append_path_with_name(&source, path)
                    .with_context(|| {
                        format!(
                            "failed to add file `{}` to CI cache archive `{}`",
                            source.display(),
                            temporary.display()
                        )
                    })?;
            }
        }
        builder.finish().with_context(|| {
            format!(
                "failed to finalize CI cache archive `{}`",
                temporary.display()
            )
        })?;
        Ok(())
    }

    /// Pack this job's declared paths and publish them as a downloadable CI
    /// artifact.
    ///
    /// The archive is written next to the repositories, under the same
    /// `_artifacts/jobs/<job>/` staging directory the runner-facing HTTP route
    /// uses, and then handed to blob storage under the key the download route
    /// resolves. The staging copy is removed once the row names the blob: it is
    /// a second full copy of the artifact that no row and no retention sweep
    /// would ever come back for.
    async fn publish_artifact(&self, job_id: i64, name: &str, paths: &[String]) -> Result<()> {
        let staging = self
            .storage_root()
            .join("_artifacts")
            .join("jobs")
            .join(job_id.to_string());
        let archive = staging.join(format!("{}.{name}.tar", uuid::Uuid::new_v4()));
        let workspace = self.workspace_path();
        let pack_paths = paths.to_vec();
        let pack_archive = archive.clone();
        // Packing walks the workspace and can be arbitrarily large; keep it off
        // the async worker the way the cache save does.
        let packed = tokio::task::spawn_blocking(move || {
            pack_archive_from(&workspace, &pack_paths, &pack_archive)
        })
        .await?;
        if let Err(error) = packed {
            remove_artifact_staging(&archive);
            return Err(error);
        }

        let published = self.store_artifact(job_id, name, &archive).await;
        // Whether the row was written or the publication failed, the staging
        // copy has done its job — on the failure path nothing points at it at
        // all, which is exactly when it would be left behind forever.
        remove_artifact_staging(&archive);
        published
    }

    /// Copy a packed archive into blob storage and record the row that names
    /// it. On any failure after the bytes land, the blob is removed again: the
    /// row is the only handle on it, so a blob with no row is unreachable
    /// storage that no retention sweep walks.
    async fn store_artifact(
        &self,
        job_id: i64,
        name: &str,
        archive: &std::path::Path,
    ) -> Result<()> {
        let size = std::fs::metadata(archive)
            .map_err(|error| artifact_path_error("CI artifact archive", archive, &error))?
            .len() as i64;
        let sha256 = hash_archive(archive)
            .map_err(|error| artifact_path_error("CI artifact archive", archive, &error))?;
        let storage = rg_core::blob_storage::LocalBlobStorage::new(self.storage_root());
        let key = rg_core::blob_storage::BlobKey::from_segments([
            "artifacts",
            "jobs",
            &job_id.to_string(),
            &format!("{}-{name}", uuid::Uuid::new_v4()),
        ])?;
        rg_core::blob_storage::BlobStorage::put_file(&storage, &key, archive).await?;

        let policy = rg_db::ops::ci_retention_ops::get_policy(&self.db, self.repo_id).await;
        let expires_at = match policy {
            Ok(policy) => Some(rg_db::ops::ci_retention_ops::expires_after(
                policy.artifact_retention_days,
            )),
            Err(error) => {
                discard_artifact_blob(&storage, &key, job_id, name).await;
                return Err(error);
            }
        };
        if let Err(error) = rg_db::ops::artifact_ops::create_artifact(
            &self.db,
            job_id,
            name,
            key.as_str(),
            size,
            Some(sha256),
            expires_at,
        )
        .await
        {
            discard_artifact_blob(&storage, &key, job_id, name).await;
            return Err(error);
        }
        Ok(())
    }

    /// Record the published archive so something points at it.
    async fn record_cache_entry(&self, key: &str, archive: &std::path::Path) -> Result<()> {
        let size = std::fs::metadata(archive)
            .map_err(|error| cache_path_error("CI cache archive", archive, &error))?
            .len() as i64;
        let digest = hash_archive(archive)
            .map_err(|error| cache_path_error("CI cache archive", archive, &error))?;
        let policy = rg_db::ops::ci_retention_ops::get_policy(&self.db, self.repo_id).await?;
        rg_db::ops::ci_retention_ops::upsert_cache_entry(
            &self.db,
            self.repo_id,
            &cache_key_hash(key),
            archive.to_string_lossy().as_ref(),
            size,
            Some(&digest),
            policy.cache_retention_days,
        )
        .await?;
        Ok(())
    }
}

/// Where a CI workspace lives, for the same reason as
/// [`rg_core::platform::fs::CI_CACHE_DIR_HINT`].
const WORKSPACE_DIR_HINT: &str =
    "CI workspaces live in `_ci_workspaces/<repo_id>/` next to the repository storage root; \
     that directory must be writable by the user running forgekeep";

/// One actionable line for a filesystem failure on a CI cache path.
///
/// The path is computed internally — the archive is named after a SHA-256 of
/// the cache key — so a bare `io::Error` reaching the job log names neither the
/// file that failed nor the directory an operator would have to fix. The remedy
/// is shared with the server half of the same directory (`rg-http`'s cache
/// endpoints), so one bind-mount never yields two different stories.
fn cache_path_error(what: &str, path: &std::path::Path, error: &std::io::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        rg_core::platform::fs::describe_path_error(
            what,
            path,
            error,
            rg_core::platform::fs::CI_CACHE_DIR_HINT
        )
    )
}

/// Best-effort removal of a cache archive we no longer want on disk.
///
/// The caller is already on an error path, so a failure here must not replace
/// the original error — but swallowing it silently is how an orphaned archive
/// keeps occupying the cache quota with nothing in the log to explain it.
fn remove_cache_archive(archive: &std::path::Path, why: &str) {
    if let Err(error) = std::fs::remove_file(archive) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                path = %archive.display(),
                %error,
                "failed to remove CI cache archive after {why}; the orphaned file stays on disk \
                 until cache retention reclaims it"
            );
        }
    }
}

/// Prefix used for runner-generated lines so they are distinguishable from the
/// job script's own output.
const JOB_NOTICE_PREFIX: &str = "[forgekeep] ";

/// Append runner diagnostics to a job's captured output.
///
/// Notices are multi-line (an actionable path error carries a `hint:` line), so
/// every line is prefixed individually — a diagnostic that blends into script
/// output is one nobody reads.
fn append_job_notices(mut log: String, notices: &[String]) -> String {
    if notices.is_empty() {
        return log;
    }
    if !log.is_empty() && !log.ends_with('\n') {
        log.push('\n');
    }
    for notice in notices {
        for line in notice.lines() {
            log.push_str(JOB_NOTICE_PREFIX);
            log.push_str(line);
            log.push('\n');
        }
    }
    log
}

fn cache_key_hash(key: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(key.as_bytes()))
}

/// Resolve the archive a cache row names, inside `directory` and nowhere else.
///
/// A row records the full path it was written under, and the two writers — this
/// runner and `rg_http`'s cache endpoints — spell that root differently (a path
/// derived from the repository versus the configured `repo_root`), so the prefix
/// is not something either side can compare against. The file name is: every
/// archive of a repository sits directly in this one directory, which is also
/// what keeps a row from ever addressing a file outside it.
fn recorded_cache_archive(
    directory: &std::path::Path,
    recorded: &str,
) -> Option<std::path::PathBuf> {
    std::path::Path::new(recorded)
        .file_name()
        .map(|name| directory.join(name))
}

/// Hex-encoded SHA-256 of a file's *contents*, streamed in bounded chunks so a
/// large cache archive is never buffered in memory just to be hashed.
/// The artifact declaration stored on the job row, as the publish path needs
/// it.
///
/// `None` means the job declared no artifact. An unreadable value is an error
/// rather than a `None`: the two used to be the same answer on the cache path,
/// and a job whose caching had silently turned itself off went green saying
/// nothing (see `poll_job`'s decode of the same columns).
fn artifact_spec(stored: Option<&str>) -> Result<Option<(String, Vec<String>)>> {
    let Some(stored) = stored else {
        return Ok(None);
    };
    #[derive(serde::Deserialize)]
    struct StoredArtifacts {
        name: String,
        paths: Vec<String>,
    }
    let spec: StoredArtifacts =
        serde_json::from_str(stored).context("invalid stored CI artifact declaration")?;
    if spec.paths.is_empty() {
        anyhow::bail!("stored CI artifact declaration names no paths");
    }
    Ok(Some((spec.name, spec.paths)))
}

/// Pack `paths`, resolved inside `workspace`, into a `tar` at `archive`.
///
/// Shares the cache packer's shape deliberately — same relative-path entries,
/// same silent skip of a path the job never produced — so an artifact and a
/// cache of the same directory unpack the same way.
fn pack_archive_from(
    workspace: &std::path::Path,
    paths: &[String],
    archive: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = archive.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| artifact_path_error("CI artifact directory", parent, &error))?;
    }
    let file = std::fs::File::create(archive)
        .map_err(|error| artifact_path_error("CI artifact archive", archive, &error))?;
    let mut builder = tar::Builder::new(file);
    let mut packed = 0usize;
    for path in paths {
        let source = workspace.join(path);
        if source.is_dir() {
            builder.append_dir_all(path, &source).with_context(|| {
                format!(
                    "failed to add directory `{}` to CI artifact archive",
                    source.display()
                )
            })?;
            packed += 1;
        } else if source.is_file() {
            builder
                .append_path_with_name(&source, path)
                .with_context(|| {
                    format!(
                        "failed to add file `{}` to CI artifact archive",
                        source.display()
                    )
                })?;
            packed += 1;
        }
    }
    builder
        .finish()
        .context("failed to finalize CI artifact archive")?;
    // An artifact that matched nothing is the author's mistake, and publishing
    // an empty archive hides it behind a downloadable file: the pipeline shows
    // an artifact, and only whoever unpacks it finds out the build produced no
    // such path.
    if packed == 0 {
        anyhow::bail!(
            "none of the declared artifact paths exist in the workspace: {}",
            paths.join(", ")
        );
    }
    Ok(())
}

/// Drop a staging archive, reporting a failure rather than leaving an
/// artifact-sized file behind without a word.
fn remove_artifact_staging(archive: &std::path::Path) {
    match std::fs::remove_file(archive) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            archive = %archive.display(),
            error = %error,
            "failed to remove the staged CI artifact archive; it stays on disk with nothing pointing at it"
        ),
    }
}

/// Roll back artifact bytes whose row was never written.
async fn discard_artifact_blob(
    storage: &rg_core::blob_storage::LocalBlobStorage,
    key: &rg_core::blob_storage::BlobKey,
    job_id: i64,
    name: &str,
) {
    if let Err(error) = rg_core::blob_storage::BlobStorage::delete(storage, key).await {
        tracing::warn!(
            job_id,
            artifact = %name,
            storage_path = %key.as_str(),
            error = %error,
            "orphaned CI artifact blob: the artifact row was not created and the rollback delete failed too — the blob stays in storage with no row pointing at it"
        );
    }
}

/// One actionable line for a filesystem failure while publishing an artifact.
fn artifact_path_error(
    what: &str,
    path: &std::path::Path,
    error: &std::io::Error,
) -> anyhow::Error {
    anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
        what,
        path,
        error,
        rg_core::platform::fs::BLOB_STORAGE_HINT,
    ))
}

fn hash_archive(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn cache_spec(
    key: Option<&str>,
    paths: Option<&str>,
    environment: &[(String, String)],
) -> Result<Option<(String, Vec<String>)>> {
    let (Some(key), Some(paths)) = (key, paths) else {
        return Ok(None);
    };
    let mut key = key.to_owned();
    for (name, value) in environment {
        key = key
            .replace(&format!("${{{name}}}"), value)
            .replace(&format!("${name}"), value);
    }
    if key.is_empty() || key.len() > 512 {
        anyhow::bail!("CI cache key must contain 1-512 bytes");
    }
    let paths: Vec<String> =
        serde_json::from_str(paths).context("invalid stored CI cache paths")?;
    if paths.is_empty() || paths.len() > 64 {
        anyhow::bail!("CI cache requires 1-64 paths");
    }
    for path in &paths {
        let path_value = std::path::Path::new(path);
        if path_value.is_absolute()
            || path_value.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            anyhow::bail!("CI cache path must stay within the workspace: {path}");
        }
    }
    Ok(Some((key, paths)))
}

fn valid_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z' | 'a'..='z'))
        && chars.all(|ch| matches!(ch, '_' | 'A'..='Z' | 'a'..='z' | '0'..='9'))
}

fn is_reserved_ci_variable(name: &str) -> bool {
    rg_core::ci::is_builtin_ci_variable(name) || matches!(name, "HOME" | "PATH")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, NotSet, Set};

    /// The runner is the second of the two CI-completion paths, and it built the
    /// same stripped context: a pipeline finishing under the embedded runner
    /// auto-merged a PR whose merge commit nobody was told about
    /// (card_85b8d59246b5).
    #[tokio::test]
    async fn a_wired_runner_hands_its_post_success_hooks_the_hub_and_smtp() {
        let (recorder, notifications) = crate::test_notifier::wiring();
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        let mut runner = PipelineRunner::new_local_only_with_job_timeout(
            db,
            std::path::Path::new("/srv/repos/o/r.git"),
            1,
            731,
        );
        assert_eq!(runner.job_timeout_secs, 731);
        runner.set_notifications(notifications);

        let context = runner.post_push_context(std::path::Path::new("/srv/repos"));

        let notifier = context
            .notifier
            .expect("a merge the embedded runner unblocks owes the same events as a REST merge");
        notifier.notify(9, "push", serde_json::json!({}));
        assert_eq!(recorder.events(), vec![(9, "push".to_string())]);
        assert!(context.smtp_config.is_some());
    }

    #[tokio::test]
    async fn execution_waits_for_and_drives_its_scoped_heartbeat() {
        let (heartbeat_sent, heartbeat_received) = tokio::sync::oneshot::channel();
        let mut heartbeat_sent = Some(heartbeat_sent);
        let execution = async {
            heartbeat_received
                .await
                .expect("the heartbeat loop must stay live with the execution");
            42
        };

        let output =
            with_job_heartbeat(execution, std::time::Duration::from_millis(1), move || {
                let heartbeat_sent = heartbeat_sent.take();
                async move {
                    if let Some(heartbeat_sent) = heartbeat_sent {
                        heartbeat_sent
                            .send(())
                            .expect("execution must still be waiting for heartbeat");
                    }
                }
            })
            .await;

        assert_eq!(output, 42);
    }

    #[test]
    fn validates_environment_names_and_protects_runner_variables() {
        assert!(valid_environment_name("DEPLOY_TARGET_2"));
        assert!(!valid_environment_name("2TARGET"));
        assert!(!valid_environment_name("BAD-NAME"));
        assert!(is_reserved_ci_variable("CI_JOB_TOKEN"));
        assert!(is_reserved_ci_variable("CI_REPOSITORY"));
        assert!(is_reserved_ci_variable("CI_REPOSITORY_OWNER"));
        assert!(!is_reserved_ci_variable("PROJECT_MODE"));
    }

    #[test]
    fn masks_longest_secret_values_first() {
        assert_eq!(
            rg_core::auth::encryption::mask_values(
                "token=abcdef and abcd",
                &["abcd".into(), "abcdef".into()]
            ),
            "token=*** and ***"
        );
    }

    #[test]
    fn cache_config_resolves_variables_and_rejects_workspace_escape() {
        let env = vec![("CI_SHA".into(), "abc123".into())];
        let cache = cache_spec(Some("build-${CI_SHA}"), Some(r#"["target"]"#), &env)
            .unwrap()
            .unwrap();
        assert_eq!(cache.0, "build-abc123");
        assert!(cache_spec(Some("bad"), Some(r#"["../outside"]"#), &env).is_err());
    }

    /// A `PipelineRunner` wired to a migrated throwaway database and a repo
    /// path under `root`, so cache tests only spell out what they exercise.
    async fn cache_runner(root: &std::path::Path) -> PipelineRunner {
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", root.join("cache.db").display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "cache-owner",
            "cache-owner@example.com",
            "unused",
            "Cache Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repository = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("repo".into()),
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
        let repo_path = root.join("repos/owner/repo.git");
        std::fs::create_dir_all(&repo_path).unwrap();
        let mut runner = PipelineRunner::new_local_only(db, &repo_path, 77);
        runner.set_repo_id(repository.id);
        runner
    }

    #[tokio::test]
    async fn repository_scoped_cache_round_trips_workspace_paths() {
        let temp = tempfile::tempdir().unwrap();
        let runner = cache_runner(temp.path()).await;
        let workspace = runner.workspace_path();
        std::fs::create_dir_all(workspace.join("target")).unwrap();
        std::fs::write(workspace.join("target/cache.txt"), "cached").unwrap();
        runner
            .save_cache("build-main", &["target".into()])
            .await
            .unwrap();
        std::fs::remove_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        runner.restore_cache("build-main").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("target/cache.txt")).unwrap(),
            "cached"
        );
    }

    /// The archive lives in a directory derived from the repository storage
    /// root — nothing an operator can guess. A save failure that only says
    /// `Permission denied (os error 13)` names neither the file nor the
    /// directory to fix, which is exactly what a bind-mounted repo root owned by
    /// another uid produces.
    #[tokio::test]
    async fn cache_save_failure_names_the_archive_directory_and_the_remedy() {
        let temp = tempfile::tempdir().unwrap();
        let runner = cache_runner(temp.path()).await;
        let archive_dir = runner.cache_archive_dir();
        // A regular file where `_ci_cache/` belongs fails the directory
        // creation deterministically, independent of the uid the tests run as.
        std::fs::write(archive_dir.parent().unwrap(), "not a directory").unwrap();

        let error = runner
            .save_cache("build-main", &["target".into()])
            .await
            .expect_err("save must fail when the cache directory cannot be created");
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&archive_dir.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("CI cache directory"), "{rendered}");
        assert!(rendered.contains("_ci_cache/<repo_id>/"), "{rendered}");
    }

    /// Everything left lying in the repository's cache directory, sorted.
    fn cache_dir_leftovers(directory: &std::path::Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        names.sort();
        names
    }

    /// Put `content` in the workspace and save it under `key`.
    async fn save_workspace_cache(runner: &PipelineRunner, key: &str, content: &str) -> Result<()> {
        let workspace = runner.workspace_path();
        std::fs::create_dir_all(workspace.join("target")).unwrap();
        std::fs::write(workspace.join("target/cache.txt"), content).unwrap();
        runner.save_cache(key, &["target".into()]).await
    }

    /// What a restore into an emptied workspace produces.
    async fn restored_cache_content(runner: &PipelineRunner, key: &str) -> Option<String> {
        let workspace = runner.workspace_path();
        std::fs::remove_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        runner.restore_cache(key).await.unwrap();
        std::fs::read_to_string(workspace.join("target/cache.txt")).ok()
    }

    /// A save that fails after packing must leave the previous run's cache
    /// exactly where it was.
    ///
    /// This is the retry case: a repeat of a key that already has an archive.
    /// Packing over the stable `<hash>.tar` destroyed it before anything had
    /// confirmed the new one, and the compensation then deleted that same path
    /// as if the failed save owned it — so one failed retry turned a working
    /// cache into a row pointing at bytes that no longer exist.
    #[tokio::test]
    async fn a_failed_second_save_keeps_the_previous_cache_restorable() {
        let temp = tempfile::tempdir().unwrap();
        let runner = cache_runner(temp.path()).await;
        save_workspace_cache(&runner, "build-main", "first")
            .await
            .unwrap();
        let published = cache_dir_leftovers(&runner.cache_archive_dir());

        // The row exists now, so the second save updates it — and that update is
        // the last step before the new archive would become the live one.
        runner
            .db
            .execute_unprepared(
                "CREATE TRIGGER fk_fault_cache_update BEFORE UPDATE ON ci_cache_entries \
                 BEGIN SELECT RAISE(ABORT, 'injected failure: UPDATE on ci_cache_entries'); END;",
            )
            .await
            .unwrap();
        save_workspace_cache(&runner, "build-main", "second")
            .await
            .expect_err("save must fail when the cache entry cannot be recorded");
        runner
            .db
            .execute_unprepared("DROP TRIGGER fk_fault_cache_update")
            .await
            .unwrap();

        assert_eq!(
            restored_cache_content(&runner, "build-main")
                .await
                .as_deref(),
            Some("first"),
            "the failed retry destroyed the cache the live entry still names"
        );
        assert_eq!(
            cache_dir_leftovers(&runner.cache_archive_dir()),
            published,
            "the failed retry kept an archive no row points at"
        );
    }

    /// A save that succeeds owns the archive it replaced, and takes it with it.
    ///
    /// The other half of the same rule: only a save that got its row through may
    /// remove the previous one's bytes. Skipping that leaves one dead archive
    /// per run — retention walks rows, so nothing else would ever collect them.
    #[tokio::test]
    async fn a_successful_save_retires_the_archive_it_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let runner = cache_runner(temp.path()).await;
        save_workspace_cache(&runner, "build-main", "first")
            .await
            .unwrap();
        save_workspace_cache(&runner, "build-main", "second")
            .await
            .unwrap();

        assert_eq!(
            cache_dir_leftovers(&runner.cache_archive_dir()).len(),
            1,
            "the superseded archive stayed on disk with nothing naming it"
        );
        assert_eq!(
            restored_cache_content(&runner, "build-main")
                .await
                .as_deref(),
            Some("second"),
        );
    }

    /// An archive whose entry was never recorded must not stay on disk.
    ///
    /// A first save has no previous archive to fall back on, so its own bytes
    /// are the only thing on disk — and until the row lands they are invisible
    /// to retention (which walks rows) and unreachable through `download_cache`
    /// (which resolves through the database).
    #[tokio::test]
    async fn a_cache_archive_whose_entry_was_not_recorded_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let runner = cache_runner(temp.path()).await;

        // Take the retention policy away: `get_policy` sits after the archive is
        // packed and before the row, which is exactly the stretch that had no
        // cleanup.
        runner
            .db
            .execute_unprepared("DROP TABLE ci_retention_policies")
            .await
            .unwrap();

        save_workspace_cache(&runner, "build-main", "cached")
            .await
            .expect_err("save must fail when the retention policy cannot be read");

        assert_eq!(
            cache_dir_leftovers(&runner.cache_archive_dir()),
            Vec::<String>::new(),
            "the failed save kept an archive no row points at"
        );
    }

    /// A cache failure never fails the job, so the job log is the only place
    /// its author will ever see it — the server log belongs to the operator.
    #[test]
    fn job_notices_are_appended_to_the_log_line_by_line() {
        let log = append_job_notices(
            "building".into(),
            &["CI cache save failed: /srv/_ci_cache/1/ab.tar\n  hint: chown it".into()],
        );

        assert_eq!(
            log,
            "building\n\
             [forgekeep] CI cache save failed: /srv/_ci_cache/1/ab.tar\n\
             [forgekeep]   hint: chown it\n"
        );
        assert_eq!(append_job_notices("kept".into(), &[]), "kept");
    }

    #[tokio::test]
    async fn persisted_job_variables_reach_the_local_runner() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("ci.db");
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", db_path.display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "ci-vars",
            "ci-vars@example.com",
            "unused",
            "CI Vars",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("variables".into()),
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
        let repo_path = temp.path().join("repos/ci-vars/variables.git");
        std::fs::create_dir_all(&repo_path).unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(&repo_path)).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI Test"], Some(&repo_path))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(&repo_path)
            )
            .unwrap()
            .success());
        std::fs::write(repo_path.join("README.md"), "workspace").unwrap();
        assert!(git
            .run(&["add", "README.md"], Some(&repo_path))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "initial"], Some(&repo_path))
            .unwrap()
            .success());
        let commit_sha = git
            .run(&["rev-parse", "HEAD"], Some(&repo_path))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            &commit_sha,
            "refs/heads/main",
            "manual",
            Some(user.id),
        )
        .await
        .unwrap();
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
            .await
            .unwrap();
        let job = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "variables",
            &format!("test \"$MESSAGE\" = hello && test \"$CI_SHA\" = {commit_sha} && test \"$CI_REPOSITORY\" = ci-vars/variables && test \"$CI_REPOSITORY_OWNER\" = ci-vars && test -f README.md && echo \"secret=$DEPLOY_SECRET\""),
            None,
            None,
            Some(r#"{"MESSAGE":"hello","CI_JOB_TOKEN":"must-not-override"}"#),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let allowed_failure = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "allowed-failure",
            "exit 7",
            None,
            None,
            None,
            None,
            None,
            None,
            true,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let allowed_timeout = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "allowed-timeout",
            "sleep 2",
            None,
            None,
            None,
            None,
            None,
            None,
            true,
            Some(1),
            None,
            None,
        )
        .await
        .unwrap();

        // Deliberately different strings: the CI secret is keyed with the
        // *encryption* key, and a runner handed only a signing secret must not
        // be able to open it. Keeping the two equal here would let the two
        // fields silently collapse back into one (card_d740512de0a8).
        let jwt_secret = "runner-token-signing-secret";
        let encryption_key = "runner-at-rest-encryption-key";
        let encrypted = rg_core::auth::encryption::encrypt(
            "super-secret-value",
            &rg_core::auth::encryption::derive_key(encryption_key),
        )
        .unwrap();
        rg_db::ops::ci_secret_ops::upsert(&db, repo.id, "DEPLOY_SECRET", &encrypted, user.id)
            .await
            .unwrap();

        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, pipeline.id);
        runner.set_repo_id(repo.id);
        runner.set_jwt_secret(jwt_secret.into());
        runner.set_encryption_key(encryption_key.into());
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();

        let completed = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "success", "{:?}", completed.log);
        let log = completed.log.unwrap_or_default();
        assert!(log.contains("secret=***"));
        assert!(!log.contains("super-secret-value"));
        let allowed_failure = rg_db::ops::pipeline_ops::get_job(&db, allowed_failure.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(allowed_failure.status, "failed");
        let allowed_timeout = rg_db::ops::pipeline_ops::get_job(&db, allowed_timeout.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(allowed_timeout.status, "failed");
        assert!(allowed_timeout
            .log
            .unwrap_or_default()
            .contains("timed out"));
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "success"
        );

        let manual_pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            &commit_sha,
            "refs/heads/main",
            "manual-resume-test",
            Some(user.id),
        )
        .await
        .unwrap();
        let manual_stage =
            rg_db::ops::pipeline_ops::create_stage(&db, manual_pipeline.id, "deploy", 0)
                .await
                .unwrap();
        let automatic_job = rg_db::ops::pipeline_ops::create_job(
            &db,
            manual_stage.id,
            "prepare",
            "echo prepared",
            None,
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
        .await
        .unwrap();
        let manual_job = rg_db::ops::pipeline_ops::create_job(
            &db,
            manual_stage.id,
            "deploy",
            "echo deployed",
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            Some("manual"),
            None,
        )
        .await
        .unwrap();

        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, manual_pipeline.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, manual_pipeline.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "manual"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::get_job(&db, manual_job.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "manual"
        );

        assert!(
            rg_db::ops::pipeline_ops::play_manual_job(&db, manual_job.id)
                .await
                .unwrap()
        );
        assert!(
            !rg_db::ops::pipeline_ops::play_manual_job(&db, manual_job.id)
                .await
                .unwrap()
        );
        rg_db::ops::pipeline_ops::resume_pipeline_chain(&db, manual_pipeline.id, manual_stage.id)
            .await
            .unwrap();
        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, manual_pipeline.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, manual_pipeline.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "success"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::get_job(&db, automatic_job.id)
                .await
                .unwrap()
                .unwrap()
                .log
                .unwrap_or_default()
                .matches("prepared")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn host_runner_disabled_by_default_refuses_imageless_jobs() {
        let temp = tempfile::tempdir().unwrap();
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("host.db").display()
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
            "host-owner",
            "host-owner@example.com",
            "unused",
            "Host Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("host".into()),
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
        let repo_path = temp.path().join("repos/host-owner/host.git");
        std::fs::create_dir_all(&repo_path).unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(&repo_path)).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI"], Some(&repo_path))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(&repo_path)
            )
            .unwrap()
            .success());
        std::fs::write(repo_path.join("README.md"), "hi").unwrap();
        assert!(git
            .run(&["add", "README.md"], Some(&repo_path))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "init"], Some(&repo_path))
            .unwrap()
            .success());
        let commit_sha = git
            .run(&["rev-parse", "HEAD"], Some(&repo_path))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            &commit_sha,
            "refs/heads/main",
            "manual",
            Some(user.id),
        )
        .await
        .unwrap();
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
            .await
            .unwrap();
        let job = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "hostjob",
            "echo should-not-run",
            None,
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
        .await
        .unwrap();

        // Default runner: host shell execution is NOT allowed, so an imageless
        // job must be refused and its script must never run.
        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, pipeline.id);
        runner.set_repo_id(repo.id);
        runner.run().await.unwrap();

        let completed = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "failed");
        let log = completed.log.unwrap_or_default();
        assert!(
            log.contains("host-shell CI execution is disabled"),
            "expected host-runner refusal, got: {log}"
        );
        assert!(
            !log.contains("should-not-run"),
            "job body executed despite host runner being disabled: {log}"
        );

        // With host execution explicitly enabled, the same job now runs.
        let allow_pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            &commit_sha,
            "refs/heads/main",
            "manual",
            Some(user.id),
        )
        .await
        .unwrap();
        let allow_stage = rg_db::ops::pipeline_ops::create_stage(&db, allow_pipeline.id, "test", 0)
            .await
            .unwrap();
        let allow_job = rg_db::ops::pipeline_ops::create_job(
            &db,
            allow_stage.id,
            "hostjob",
            "echo did-run",
            None,
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
        .await
        .unwrap();
        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, allow_pipeline.id);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();
        let completed = rg_db::ops::pipeline_ops::get_job(&db, allow_job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "success", "{:?}", completed.log);
        assert!(completed.log.unwrap_or_default().contains("did-run"));
    }
}
