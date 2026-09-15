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
    /// The process is stopping. The job this runner was holding has been handed
    /// back to `pending`, and nothing about the stage or the pipeline is
    /// rewritten: they are unfinished, not failed, and saying `failed` here
    /// would blame a restart on the code being built (card_34368880dc20).
    Interrupted,
}

/// Longest this runner waits on `docker rm`.
///
/// The stop path is on a stopwatch — the process has been told to go and will be
/// `SIGKILL`ed if it dawdles — so an unreachable Docker daemon must cost a
/// bounded pause, not the whole remaining grace window. Same number and same
/// reasoning as the external runner's (card_3027b2187d42).
const CONTAINER_REMOVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The container name a job runs under.
///
/// One function rather than a `format!` at each site: the stop path removes the
/// container by name, so a name that drifts from the one `docker run --name`
/// was given leaves a live container behind with nothing pointing at it.
pub(crate) fn job_container_name(job_id: i64) -> String {
    format!("forgekeep-job-{job_id}")
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
    /// Labels this runner answers to, for the `tags:` / `runs-on:` a job
    /// declares. Defaults to [`rg_core::ci::default_runner_labels`]; the
    /// operator replaces the list with `ci.runner_labels`.
    ///
    /// Before it existed the in-process runner carried no labels and never
    /// asked: `find_pending_job_matching_labels` is the external scheduler's
    /// question, reachable only from the poll route, so a job that asked to run
    /// somewhere else ran here (card_4f7703a8b575).
    runner_labels: Vec<String>,
    /// The process-wide graceful-shutdown signal, when the embedder has one.
    ///
    /// `None` for a test or a one-off `rg-cli` command: the pipeline then runs
    /// to its end, exactly as before. See [`PipelineRunner::set_shutdown`].
    shutdown: Option<tokio::sync::watch::Receiver<bool>>,
    /// Executable used for Docker CLI calls.
    ///
    /// Production always uses `docker`; keeping the program on the runner lets
    /// timeout tests use an isolated daemon/client fixture without mutating the
    /// process-wide `PATH` seen by the rest of the test binary.
    docker_program: std::path::PathBuf,
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
            runner_labels: rg_core::ci::default_runner_labels(),
            shutdown: None,
            docker_program: "docker".into(),
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
            runner_labels: rg_core::ci::default_runner_labels(),
            shutdown: None,
            docker_program: "docker".into(),
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

    /// Replace the labels this runner answers to.
    ///
    /// An empty list is the honest description of a runner that answers to
    /// nothing, and it is a legitimate setting: it refuses every job that
    /// declares a label, which is what an operator who wants all routed work to
    /// wait for a real runner asks for.
    pub fn set_runner_labels(&mut self, labels: Vec<String>) {
        self.runner_labels = labels;
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

    /// Tell this runner about the process's graceful-shutdown signal.
    ///
    /// `forgekeep serve` fans one `watch` channel out to the HTTP server, the
    /// SSH transport and every background worker. The embedded pipeline runner
    /// was the consumer it never reached: a `SIGTERM` severed the pipeline
    /// wherever it stood and left its `pipeline_jobs` row `running` until the
    /// stuck-job sweep reclaimed it ten minutes later. Worse than the same
    /// event on an external runner, because this executor is the *same process*
    /// that is at that moment draining HTTP, the log queue and the delivery
    /// tracker — and on an instance with no external runner it is the only
    /// executor there is (card_34368880dc20).
    ///
    /// With the signal wired, the interrupted job's container is removed and
    /// its row goes straight back to `pending`; the drain that makes the write
    /// land is `rg_core::task_tracker::ci_tracker()`.
    pub fn set_shutdown(&mut self, shutdown: Option<tokio::sync::watch::Receiver<bool>>) {
        self.shutdown = shutdown;
    }

    /// Point this runner at an isolated Docker CLI fixture.
    #[cfg(test)]
    fn set_docker_program(&mut self, program: impl Into<std::path::PathBuf>) {
        self.docker_program = program.into();
    }

    /// Build every Docker client with the same drop contract.
    ///
    /// The timeout boundary owns the returned future, not the child handle. If
    /// it expires, dropping `output()` must kill the Docker CLI; otherwise a
    /// wedged client survives even after the job has been recorded as failed.
    fn docker_command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.docker_program);
        command.kill_on_drop(true);
        command
    }

    /// Force-remove the container a job ran in.
    ///
    /// `docker run` here is not `-d`, but the container is still not this
    /// process's child: dropping the Docker *client* leaves daemon-owned work
    /// running, so it has to be removed by name. Never fails the caller — there
    /// is nothing left to abort — but the failure is named, because what it
    /// leaves behind holds CPU, memory and the job's workspace mount.
    async fn remove_job_container(&self, job_id: i64) {
        let container_name = job_container_name(job_id);
        let mut command = self.docker_command();
        command.args(["rm", "-f", &container_name]);
        let removal = command.output();
        match tokio::time::timeout(CONTAINER_REMOVAL_TIMEOUT, removal).await {
            Ok(Ok(output)) if output.status.success() => {}
            Ok(Ok(output)) => tracing::warn!(
                job_id,
                status = ?output.status.code(),
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "failed to remove the job's docker container"
            ),
            Ok(Err(error)) => {
                tracing::warn!(job_id, %error, "failed to remove the job's docker container")
            }
            Err(_) => tracing::warn!(
                job_id,
                "`docker rm` did not answer within {CONTAINER_REMOVAL_TIMEOUT:?}; the job's \
                 container may still be running"
            ),
        }
    }

    /// Whether this runner was given a shutdown signal to observe at all.
    ///
    /// Not the same question as [`Self::shutting_down`], and the distinction is
    /// the point: a runner with no signal never stops early, which is right for
    /// a test or a one-off command and wrong for the server. Exposed so the
    /// engine's wiring can be asserted where it is made.
    #[cfg(test)]
    pub(crate) fn hears_shutdown(&self) -> bool {
        self.shutdown.is_some()
    }

    /// Whether the process has already been told to stop.
    ///
    /// Read between two jobs, so a stop that lands while one is finishing does
    /// not get one more started on top of it.
    fn shutting_down(&self) -> bool {
        self.shutdown
            .as_ref()
            .is_some_and(|shutdown| *shutdown.borrow())
    }

    /// Resolve when the process is asked to stop; never, when there is no
    /// signal to listen to.
    ///
    /// The `borrow()` first is not redundant with [`Self::shutting_down`].
    /// A `watch` clone counts the value it was born with as already seen, so a
    /// signal that lands between the caller's check and this clone would leave
    /// `changed()` waiting for a second one that never comes — the job would
    /// run on through the very shutdown it is supposed to notice. That is why
    /// this is spelled out here rather than delegated to
    /// `task_tracker::wait_optional_shutdown`, whose callers all clone their
    /// receiver once at startup, before any signal exists.
    async fn shutdown_requested(&self) {
        match self.shutdown.clone() {
            Some(mut shutdown) => {
                if *shutdown.borrow() {
                    return;
                }
                if shutdown.changed().await.is_err() {
                    // Sender dropped: treat it the same as an explicit stop.
                }
            }
            None => std::future::pending::<()>().await,
        }
    }

    /// Give one interrupted job back to the pool.
    ///
    /// Order matters the same way it does on the external runner
    /// (card_3027b2187d42): the container goes first, while this process still
    /// knows the job id, and the row goes back only afterwards — the moment it
    /// reads `pending`, any external runner may claim it, and it must not find
    /// the previous container still holding the workspace mount.
    ///
    /// Neither half is allowed to fail the stop path. A container that will not
    /// die still has to be named, and a row that cannot be handed back is not
    /// lost — it is exactly what the ten-minute stuck-job sweep is for.
    async fn hand_job_back(&self, job: &rg_db::entities::pipeline_job::Model) {
        if job.image.is_some() {
            self.remove_job_container(job.id).await;
        }
        match pipeline_ops::hand_back_active_job(&self.db, job.id).await {
            Ok(true) => tracing::info!(
                job_id = job.id,
                pipeline_id = self.pipeline_id,
                "server is stopping — interrupted CI job handed back as pending"
            ),
            Ok(false) => tracing::info!(
                job_id = job.id,
                pipeline_id = self.pipeline_id,
                "interrupted CI job had already settled — leaving its recorded status"
            ),
            Err(error) => tracing::error!(
                job_id = job.id,
                pipeline_id = self.pipeline_id,
                error = %format!("{error:#}"),
                "failed to hand an interrupted CI job back; the stuck-job sweep will reclaim it"
            ),
        }
    }

    /// Run the pipeline: iterate stages in order, run jobs in each stage.
    ///
    /// If a non-allowed job in a stage fails, subsequent stages are skipped.
    pub async fn run(&self) -> Result<()> {
        if let Err(error) = self.prepare_workspace().await {
            // The whole graph, not just its root. Nothing below will run — the
            // workspace this pipeline needed does not exist — so a `failed`
            // pipeline over `pending` stages and jobs would advertise work that
            // no scheduler will ever pick up and no reader can find a reason
            // for. One transaction, because three writes in a row can stop
            // between two of them (card_944be22fcd3c).
            let log = format!("runner could not prepare the workspace: {error:#}");
            match pipeline_ops::fail_pipeline_chain(&self.db, self.pipeline_id, &log).await {
                Ok(true) => rg_core::metrics_hook::record_ci_pipeline_finished("failed"),
                Ok(false) => {}
                Err(update_error) => {
                    tracing::error!(pipeline_id = self.pipeline_id, %update_error, "failed to mark pipeline failed after workspace error");
                }
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

            // A stop that lands between two stages must not start the next
            // one: the pipeline is unfinished either way, and one more stage
            // is one more container to interrupt.
            if self.shutting_down() {
                tracing::info!(
                    pipeline_id = self.pipeline_id,
                    stage_id = stage.id,
                    "server is stopping — not starting the next CI stage"
                );
                return Ok(());
            }

            match self.run_stage(stage).await? {
                StageOutcome::Paused | StageOutcome::Settled | StageOutcome::Interrupted => {
                    return Ok(())
                }
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
        // Counted only on the write that actually landed: a pipeline someone
        // else settled (a cancel that raced the last stage) is their outcome,
        // not a second one.
        rg_core::metrics_hook::record_ci_pipeline_finished(pipeline_status);

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
    ///
    /// One transition, not `1 + jobs` of them: the stage used to be published
    /// terminal before the jobs underneath it were told, so a fault in the
    /// middle left a `skipped` stage owning `pending` jobs that no pass of this
    /// runner walks again.
    async fn skip_stage(&self, stage: &rg_db::entities::pipeline_stage::Model) -> Result<()> {
        pipeline_ops::skip_embedded_stage(&self.db, self.pipeline_id, stage.id).await?;
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
                if !pipeline_ops::pause_embedded_stage_at_gate(
                    &self.db,
                    self.pipeline_id,
                    stage.id,
                    "manual",
                )
                .await?
                {
                    return Ok(StageOutcome::Settled);
                }
                tracing::info!(
                    pipeline_id = self.pipeline_id,
                    job_id = job.id,
                    "Pipeline paused at manual job"
                );
                return Ok(StageOutcome::Paused);
            }
            if job.status == "waiting_approval" {
                if !pipeline_ops::pause_embedded_stage_at_gate(
                    &self.db,
                    self.pipeline_id,
                    stage.id,
                    "waiting_approval",
                )
                .await?
                {
                    return Ok(StageOutcome::Settled);
                }
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
            // Two checks, not one. This is the cheap one: a stop already in
            // effect stops the loop before the next job is even started.
            if self.shutting_down() {
                tracing::info!(
                    pipeline_id = self.pipeline_id,
                    job_id = job.id,
                    "server is stopping — not starting the next CI job"
                );
                return Ok(StageOutcome::Interrupted);
            }

            // And this is the one that matters: the signal is honoured *inside*
            // a running job, not only between two of them. A ten-minute build
            // would otherwise outlive the container's grace period and be
            // `SIGKILL`ed — the very outcome this path exists to avoid. Biased,
            // so a stop arriving together with a finished job is not passed
            // over in favour of recording it and starting another.
            //
            // Dropping the job future cancels it: `run_job_local` kills its
            // shell on drop, and the docker branch is removed by name in
            // `hand_job_back` below.
            let job_failed = tokio::select! {
                biased;
                () = self.shutdown_requested() => {
                    self.hand_job_back(job).await;
                    return Ok(StageOutcome::Interrupted);
                }
                failed = self.run_and_record_job(job) => failed?,
            };
            if job_failed {
                stage_failed = true;
            }
        }

        // The confirming write. On a stage that ran at least one job the last
        // completion already rolled this stage — and, when it was the last
        // stage, the pipeline — up inside that job's transaction, so this
        // rewrites the status it already carries. It is still the only write
        // for a stage whose jobs were all terminal in the snapshot (and for an
        // empty one), which is why it carries the pipeline roll-up too rather
        // than leaving a terminal stage under a running pipeline.
        let stage_end = chrono::Utc::now().naive_utc();
        let stage_status = if stage_failed { "failed" } else { "success" };
        if !pipeline_ops::settle_embedded_stage(
            &self.db,
            self.pipeline_id,
            stage.id,
            stage_status,
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
    /// `allow_failure` non-zero exit or an execution error).
    ///
    /// The write is conditional. `job` is a snapshot taken before execution
    /// started, so a cancellation that landed during the run is not visible in
    /// it; [`pipeline_ops::settle_job_if_active`] refuses to move a row that
    /// already settled as something else, and the refusal is logged rather
    /// than silently dropped.
    ///
    /// The write is also not best-effort any more. A logged database error used
    /// to leave the job as the runner found it while the caller went on to
    /// settle the stage from an accumulator that counted the run — publishing a
    /// terminal stage over a job still calling itself `running`. The result now
    /// travels with the roll-up it makes due, in one transaction
    /// ([`pipeline_ops::finish_embedded_job`]), and a failure aborts the
    /// pipeline pass instead: the graph is left exactly as the transaction
    /// found it, which is the state the next pass can resume from.
    async fn run_and_record_job(&self, job: &rg_db::entities::pipeline_job::Model) -> Result<bool> {
        let execution_started = std::time::Instant::now();
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
                job.tags.as_deref(),
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

        let transition = pipeline_ops::finish_embedded_job(
            &self.db,
            self.pipeline_id,
            job.stage_id,
            job.id,
            status,
            Some(exit_code),
            Some(&log),
            None,
        )
        .await
        .with_context(|| format!("failed to record the result of CI job {}", job.id))?;

        if !transition.job_settled {
            tracing::info!(
                job_id = job.id,
                would_be = status,
                "job settled while the runner was executing it — result discarded"
            );
            // The stage's own status is settled too (the cascade is
            // whole-graph), so this must not push the stage to `failed`.
            return Ok(false);
        }

        // Post-commit, and only post-commit. The embedded runner is the only
        // executor on a default instance, and it settles its own jobs down here
        // rather than through the runner API — so without this hook nothing
        // produced `ci_jobs_total` or `ci_job_duration_seconds` at all
        // (card_e309fbb5a3fd). Counted only on the write that landed, for the
        // same reason the pipeline outcome is.
        rg_core::metrics_hook::record_ci_job_finished(status, Some(execution_started.elapsed()));
        Ok(stage_failed)
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
        tags: Option<&str>,
    ) -> Result<(i32, String)> {
        // Asked before the row is marked running, and before a token or a cache
        // is spent on it: a job whose labels this runner does not carry is not
        // a job this runner may run at all.
        //
        // The rule is `pipeline_ops::uncovered_job_tags` — the same one the
        // external scheduler routes by — rather than a second copy written
        // here, because two copies of a routing rule is how the declaration
        // stopped being honoured on this side in the first place.
        //
        // Reaching it does not require the trigger-time gate to have failed:
        // a pipeline created while `ci.external_runners` was on carries its
        // tags in the rows, and a retry after the flag came off hands those
        // rows straight to this runner with the trigger long behind them.
        let declared_tags = pipeline_ops::decode_job_tags(tags).map_err(|error| {
            anyhow::anyhow!(
                "This job declares runner tags this server cannot read ({error}), so there is no \
                 way to tell whether it was meant to run here. Fix the tags: / runs-on: value on \
                 the job."
            )
        })?;
        let uncovered = pipeline_ops::uncovered_job_tags(&declared_tags, &self.runner_labels);
        if !uncovered.is_empty() {
            let msg = format!(
                "This job asks for a runner labelled [{}], and CI on this instance runs in-process \
                 on a runner labelled [{}] — it cannot honour [{}]. Change the job's tags: / \
                 runs-on: to a label this instance carries, add the label to ci.runner_labels if \
                 this server really is that machine, or turn on ci.external_runners and register a \
                 runner that carries it.",
                declared_tags.join(", "),
                self.runner_labels.join(", "),
                uncovered.join(", "),
            );
            tracing::warn!(job_id, "{}", msg);
            return Err(anyhow::anyhow!("{}", msg));
        }

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
                        if image.is_some() {
                            self.remove_job_container(job_id).await;
                        }
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
                );
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
                );
            c
        };

        let output = rg_process::output_in_process_tree(&mut cmd)
            .await
            .context("failed to spawn job process")?;

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
        let mut docker_check_command = self.docker_command();
        let docker_check = docker_check_command
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
        let container_name = job_container_name(job_id);

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
        let mut command = self.docker_command();
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
            if rg_db::ops::ci_retention_ops::delete_cache_entry_if_expired(&self.db, &entry).await?
            {
                remove_cache_archive(&archive, "the cache entry expired");
            }
            return Ok(());
        }
        if !archive.exists() {
            return Ok(());
        }
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
        rg_db::ops::ci_retention_ops::refresh_cache_entry(
            &self.db,
            &entry,
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
        let policy = rg_db::ops::ci_retention_ops::get_policy(&self.db, self.repo_id).await;
        let expires_at = Some(rg_db::ops::ci_retention_ops::expires_after(
            policy?.artifact_retention_days,
        ));
        let storage = rg_core::blob_storage::LocalBlobStorage::new(self.storage_root());
        rg_core::artifact::publish_from_file(
            &self.db,
            &storage,
            job_id,
            name,
            archive,
            size,
            Some(sha256),
            expires_at,
        )
        .await?;
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

    /// card_e29c8d4274af: a run that cannot lay down its workspace settles the
    /// whole graph, not only its root.
    ///
    /// `prepare_workspace` fails here for the most ordinary reason there is —
    /// the repository path is a directory, not a git repository, so
    /// `git worktree add` refuses. Before the fix `run` wrote the pipeline row
    /// and returned, leaving the stage and both jobs `pending` under a `failed`
    /// pipeline: work nothing will ever schedule, advertised as waiting, with
    /// no reason recorded anywhere a person looks.
    #[tokio::test]
    async fn a_workspace_that_cannot_be_prepared_settles_the_whole_graph() {
        let temp = tempfile::tempdir().unwrap();
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", temp.path().join("ci.db").display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "workspace-owner",
            "workspace-owner@example.invalid",
            "unused",
            "Workspace Owner",
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
        let pipeline = pipeline_ops::create_pipeline(
            &db,
            repository.id,
            "1234567890123456789012345678901234567890",
            "refs/heads/main",
            "push",
            Some(user.id),
        )
        .await
        .unwrap();
        let stage = pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
            .await
            .unwrap();
        let mut job_ids = Vec::new();
        for name in ["build", "lint"] {
            job_ids.push(
                pipeline_ops::create_job(
                    &db, stage.id, name, "true", None, None, None, None, None, None, false, None,
                    None, None,
                )
                .await
                .unwrap()
                .id,
            );
        }

        // A directory, not a git repository: `git worktree add` refuses, which
        // is the shape of every real workspace failure — a repo that was
        // deleted, a bind mount that is not there, a path the server cannot
        // read.
        let repo_path = temp.path().join("repos/workspace-owner/repo.git");
        std::fs::create_dir_all(&repo_path).unwrap();
        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, pipeline.id);
        runner.set_repo_id(repository.id);

        let error = runner
            .run()
            .await
            .expect_err("a workspace that cannot be prepared is not a successful run");
        assert!(
            format!("{error:#}").contains("worktree"),
            "the failure under test is the worktree one: {error:#}"
        );

        assert_eq!(
            pipeline_ops::get_pipeline(&db, pipeline.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        assert_eq!(
            pipeline_ops::get_stage_by_id(&db, stage.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "failed",
            "the stage was left waiting under a pipeline that had already failed"
        );
        for job_id in job_ids {
            let job = pipeline_ops::get_job(&db, job_id).await.unwrap().unwrap();
            assert_eq!(
                job.status, "failed",
                "job {job_id} was left pending under a failed pipeline"
            );
            assert!(
                job.log
                    .as_deref()
                    .is_some_and(|log| log.contains("could not prepare the workspace")),
                "job {job_id} settled without saying why: {:?}",
                job.log
            );
        }
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

    /// A repository with one commit, plus the migrated database behind it.
    ///
    /// The same shape `persisted_job_variables_reach_the_local_runner` builds
    /// inline; factored out because the shutdown test needs an identical one and
    /// two copies of a git fixture drift.
    async fn repo_with_one_commit(
        temp: &std::path::Path,
        slug: &str,
    ) -> (
        rg_db::DatabaseConnection,
        rg_db::entities::repository::Model,
        std::path::PathBuf,
        String,
        i64,
    ) {
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", temp.join("ci.db").display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            slug,
            &format!("{slug}@example.com"),
            "unused",
            "CI Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set(slug.into()),
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
        let repo_path = temp.join(format!("repos/{slug}/{slug}.git"));
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
        (db, repo, repo_path, commit_sha, user.id)
    }

    /// A one-job pipeline plus the embedded runner that will execute it.
    async fn runner_with_one_job(
        temp: &std::path::Path,
        slug: &str,
        script: &str,
        image: Option<&str>,
        timeout_seconds: i64,
    ) -> (
        PipelineRunner,
        rg_db::DatabaseConnection,
        rg_db::entities::pipeline_job::Model,
    ) {
        let (db, repo, repo_path, commit_sha, user_id) = repo_with_one_commit(temp, slug).await;
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            &commit_sha,
            "refs/heads/main",
            "manual",
            Some(user_id),
        )
        .await
        .unwrap();
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
            .await
            .unwrap();
        let job = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "timeout-contract",
            script,
            image,
            None,
            None,
            None,
            None,
            None,
            false,
            Some(timeout_seconds),
            None,
            None,
        )
        .await
        .unwrap();
        let mut runner = if image.is_some() {
            PipelineRunner::new(db.clone(), &repo_path, pipeline.id)
        } else {
            PipelineRunner::new_local_only(db.clone(), &repo_path, pipeline.id)
        };
        runner.set_repo_id(repo.id);
        if image.is_none() {
            runner.set_allow_host_runner(true);
        }
        (runner, db, job)
    }

    #[cfg(unix)]
    struct DockerFixture {
        root: std::path::PathBuf,
        program: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl DockerFixture {
        fn new(root: &std::path::Path) -> Self {
            use std::os::unix::fs::PermissionsExt;

            std::fs::create_dir_all(root).unwrap();
            let program = root.join("docker-fixture");
            std::fs::write(
                &program,
                r#"#!/bin/sh
state=$(dirname "$0")
case "$1" in
  info)
    exit 0
    ;;
  rm)
    printf '%s\n' "$*" > "$state/rm.args"
    rm -f "$state/container.live"
    exit 0
    ;;
  run)
    printf '%s\n' "$$" > "$state/client.pid"
    for arg in "$@"; do
      if [ "$arg" = "fixture:success" ]; then
        printf 'docker success\n'
        exit 0
      fi
    done
    : > "$state/client.live"
    : > "$state/container.live"
    (
      while [ -e "$state/container.live" ]; do
        sleep 0.02
      done
    ) &
    container_pid=$!
    printf '%s\n' "$container_pid" > "$state/container.pid"
    wait "$container_pid"
    while [ -e "$state/client.live" ]; do
      sleep 0.02
    done
    ;;
  *)
    exit 64
    ;;
esac
"#,
            )
            .unwrap();
            let mut permissions = std::fs::metadata(&program).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&program, permissions).unwrap();
            Self {
                root: root.to_path_buf(),
                program,
            }
        }

        fn pid(&self, name: &str) -> u32 {
            std::fs::read_to_string(self.root.join(name))
                .unwrap_or_else(|error| panic!("fixture did not record {name}: {error}"))
                .trim()
                .parse()
                .unwrap_or_else(|error| panic!("fixture recorded an invalid {name}: {error}"))
        }
    }

    #[cfg(unix)]
    impl Drop for DockerFixture {
        fn drop(&mut self) {
            for marker in ["container.live", "client.live"] {
                drop(std::fs::remove_file(self.root.join(marker)));
            }
            for pid_file in ["container.pid", "client.pid"] {
                let Ok(pid) = std::fs::read_to_string(self.root.join(pid_file)) else {
                    continue;
                };
                drop(
                    std::process::Command::new("kill")
                        .args(["-KILL", pid.trim()])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status(),
                );
            }
        }
    }

    #[cfg(unix)]
    fn process_is_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(unix)]
    fn recorded_pid(path: &std::path::Path) -> u32 {
        std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("fixture did not record {}: {error}", path.display()))
            .trim()
            .parse()
            .unwrap_or_else(|error| {
                panic!("fixture recorded an invalid {}: {error}", path.display())
            })
    }

    #[cfg(unix)]
    struct RecordedPidCleanup(Vec<std::path::PathBuf>);

    #[cfg(unix)]
    impl Drop for RecordedPidCleanup {
        fn drop(&mut self) {
            for path in &self.0 {
                let Ok(pid) = std::fs::read_to_string(path) else {
                    continue;
                };
                drop(
                    std::process::Command::new("kill")
                        .args(["-KILL", pid.trim()])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status(),
                );
            }
        }
    }

    #[cfg(unix)]
    async fn assert_process_stops(pid: u32, description: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while process_is_alive(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "{description} process {pid} survived the job timeout"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// A timed-out `docker run` has two owners to stop: the Docker CLI child and
    /// the daemon-owned named container. The fixture deliberately keeps its
    /// client alive even after `rm` stops the simulated container, so removing
    /// either `kill_on_drop` or the timeout cleanup makes a different PID stay
    /// live and turns this test red (card_17e323af43fa).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_docker_job_kills_the_client_and_named_container() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = DockerFixture::new(&temp.path().join("fake-docker"));
        let (mut runner, db, job) = runner_with_one_job(
            temp.path(),
            "docker-timeout",
            "sleep forever",
            Some("fixture:hang"),
            1,
        )
        .await;
        runner.set_docker_program(&fixture.program);

        tokio::time::timeout(std::time::Duration::from_secs(15), runner.run())
            .await
            .expect("the embedded runner must return after its own timeout")
            .expect("a timed-out job is a recorded failure, not a runner error");

        let completed = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "failed", "{:?}", completed.log);
        assert_eq!(completed.exit_code, Some(-1));
        assert!(
            completed
                .log
                .as_deref()
                .is_some_and(|log| log.contains("Job timed out after 1 seconds")),
            "{:?}",
            completed.log
        );
        assert_process_stops(fixture.pid("client.pid"), "docker client").await;
        assert_process_stops(fixture.pid("container.pid"), "docker container").await;
        assert_eq!(
            std::fs::read_to_string(fixture.root.join("rm.args"))
                .unwrap()
                .trim(),
            format!("rm -f {}", job_container_name(job.id))
        );
        assert!(!fixture.root.join("container.live").exists());
    }

    /// The timeout cleanup is exceptional: a normal Docker completion must
    /// still return its output and must not race a successful container with a
    /// force-remove.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_successful_docker_job_is_not_force_removed() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = DockerFixture::new(&temp.path().join("fake-docker"));
        let (mut runner, db, job) = runner_with_one_job(
            temp.path(),
            "docker-success",
            "echo accepted",
            Some("fixture:success"),
            30,
        )
        .await;
        runner.set_docker_program(&fixture.program);

        runner.run().await.unwrap();

        let completed = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "success", "{:?}", completed.log);
        assert_eq!(completed.exit_code, Some(0));
        assert!(completed.log.unwrap_or_default().contains("docker success"));
        assert!(
            !fixture.root.join("rm.args").exists(),
            "a successful container was force-removed"
        );
        assert_process_stops(fixture.pid("client.pid"), "successful docker client").await;
    }

    /// The embedded timeout owns the shell's whole process tree. Recording both
    /// PIDs before the wait makes the regression specific: direct-child cleanup
    /// alone kills the shell but leaves `sleep` alive (card_3d7ed7a5adee).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_local_job_timeout_kills_the_shell_and_its_descendant() {
        let temp = tempfile::tempdir().unwrap();
        let shell_pid_file = temp.path().join("local-shell.pid");
        let child_pid_file = temp.path().join("local-child.pid");
        let _pid_cleanup = RecordedPidCleanup(vec![shell_pid_file.clone(), child_pid_file.clone()]);
        let script = format!(
            "printf '%s\\n' \"$$\" > '{}'; sleep 30 & child=$!; \
             printf '%s\\n' \"$child\" > '{}'; wait \"$child\"",
            shell_pid_file.display(),
            child_pid_file.display()
        );
        let (runner, db, job) =
            runner_with_one_job(temp.path(), "local-timeout", &script, None, 1).await;

        tokio::time::timeout(std::time::Duration::from_secs(15), runner.run())
            .await
            .expect("the local job must return after its timeout")
            .unwrap();

        let completed = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "failed", "{:?}", completed.log);
        assert_eq!(completed.exit_code, Some(-1));
        assert!(completed
            .log
            .unwrap_or_default()
            .contains("Job timed out after 1 seconds"));
        assert_process_stops(recorded_pid(&shell_pid_file), "local shell").await;
        assert_process_stops(recorded_pid(&child_pid_file), "local shell descendant").await;
    }

    /// card_944be22fcd3c: the runner's job result and the stage roll-up it
    /// makes due are one write or none.
    ///
    /// The trigger refuses only the *terminal* stage update, so the run gets as
    /// far as executing its job — the start writes land, the completion does
    /// not. Before the fix the job result was committed on its own and its
    /// database error was logged as best-effort, leaving a `success` job under a
    /// stage still calling itself `running`, which no later pass of this runner
    /// walks again.
    #[tokio::test]
    async fn a_job_result_the_stage_cannot_follow_leaves_the_graph_untouched() {
        use sea_orm::ConnectionTrait;

        let temp = tempfile::tempdir().unwrap();
        let (runner, db, job) =
            runner_with_one_job(temp.path(), "atomic-graph", "true", None, 60).await;
        let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&db, job.stage_id)
            .await
            .unwrap()
            .unwrap();
        db.execute_unprepared(
            "CREATE TRIGGER fk_fault_terminal_stage BEFORE UPDATE ON pipeline_stages \
             WHEN NEW.status IN ('success', 'failed', 'skipped') \
             BEGIN SELECT RAISE(ABORT, 'injected failure: terminal stage update'); END;",
        )
        .await
        .expect("arm the terminal-stage fault");

        let error = runner
            .run()
            .await
            .expect_err("a completion the database refuses must not be reported as a finished run");
        assert!(
            format!("{error:#}").contains("injected failure"),
            "the failure that surfaced is not the injected one: {error:#}"
        );

        let recorded = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recorded.status, "running",
            "the job result was published even though its stage could not follow"
        );
        assert_eq!(
            recorded.log, None,
            "the job log was published even though the transition rolled back"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::get_stage_by_id(&db, stage.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "running"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, stage.pipeline_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "running"
        );
    }

    /// Poll one job row until it reads `status`, or give up.
    ///
    /// A hang-guard, not a deadline (the same reasoning as `log_write_queue`,
    /// card_2b890485c8d8): the state either arrives in milliseconds or the code
    /// under test is broken, so a generous bound fails just as loudly as a tight
    /// one without turning machine load into a red suite.
    async fn await_job_status(
        db: &rg_db::DatabaseConnection,
        job_id: i64,
        status: &str,
    ) -> rg_db::entities::pipeline_job::Model {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let job = rg_db::ops::pipeline_ops::get_job(db, job_id)
                .await
                .unwrap()
                .unwrap();
            if job.status == status {
                return job;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "job {job_id} never reached `{status}` (stuck at `{}`)",
                job.status
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// card_34368880dc20: a planned restart must not cost the pipeline ten
    /// minutes.
    ///
    /// `spawn_internal_runner` used a bare `tokio::spawn` and the runner had no
    /// signal to observe, so `SIGTERM` severed the pipeline wherever it stood
    /// and left the `pipeline_jobs` row `running` until the stuck-job sweep
    /// reclaimed it. This test never runs that sweep — `find_stuck_jobs` is not
    /// called here and could not fire anyway, its cutoff being ten minutes older
    /// than this fixture — so the only thing that can turn the row `pending` is
    /// the stop path itself.
    #[tokio::test]
    async fn a_stopping_server_hands_the_running_ci_job_straight_back() {
        let temp = tempfile::tempdir().unwrap();
        let (db, repo, repo_path, commit_sha, user_id) =
            repo_with_one_commit(temp.path(), "shutdown").await;

        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            &commit_sha,
            "refs/heads/main",
            "manual",
            Some(user_id),
        )
        .await
        .unwrap();
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
            .await
            .unwrap();
        // Long enough that the stop lands squarely inside it — the case the
        // whole path exists for. A job that finished on its own would prove
        // nothing about being interrupted.
        let running = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "long",
            "sleep 600",
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
        let next = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "next",
            "echo second",
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

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, pipeline.id);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.set_shutdown(Some(shutdown_rx));
        let execution = tokio::spawn(async move { runner.run().await });

        // Baseline: the runner really did start the job. Without this the
        // assertions below are satisfied by a runner that never ran at all.
        let started = await_job_status(&db, running.id, "running").await;
        assert_eq!(started.status, "running");

        shutdown_tx.send(true).expect("the runner is listening");
        tokio::time::timeout(std::time::Duration::from_secs(60), execution)
            .await
            .expect("the runner must unwind, not sit through the whole `sleep 600`")
            .expect("runner task panicked")
            .expect("an interrupted pipeline is not an error");

        let handed_back = rg_db::ops::pipeline_ops::get_job(&db, running.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            handed_back.status, "pending",
            "the interrupted job stayed `{}` — that is the ten-minute wait this path removes",
            handed_back.status
        );
        assert_eq!(
            handed_back.runner_id, None,
            "a job handed back to the pool must not still name an executor"
        );

        let untouched = rg_db::ops::pipeline_ops::get_job(&db, next.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            untouched.status, "pending",
            "a stopping runner started the next job instead of stopping"
        );

        // Unfinished, not failed: blaming a restart on the code being built is
        // the other way to get this wrong.
        let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            pipeline.status, "running",
            "the restart failed the pipeline"
        );

        // And nothing was left behind for the sweep to find.
        assert!(
            rg_db::ops::pipeline_ops::find_stuck_jobs(&db, 0)
                .await
                .unwrap()
                .is_empty(),
            "an `assigned`/`running` row survived the stop path"
        );
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
            rg_db::ops::pipeline_ops::play_manual_job_and_resume_pipeline_chain(
                &db,
                manual_pipeline.id,
                manual_stage.id,
                manual_job.id,
            )
            .await
            .unwrap()
        );
        assert!(
            !rg_db::ops::pipeline_ops::play_manual_job_and_resume_pipeline_chain(
                &db,
                manual_pipeline.id,
                manual_stage.id,
                manual_job.id,
            )
            .await
            .unwrap()
        );
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

    /// A job that named a machine other than this one ran on this one.
    ///
    /// `tags:` / `runs-on:` reaches `pipeline_jobs.tags`, and the only code that
    /// ever read that column is `find_pending_job_matching_labels` — the
    /// external scheduler's question, asked from the poll route and nowhere
    /// else. The embedded runner takes every job of its stage straight out of
    /// the database, so `runs-on: my-gpu-box` executed here with nothing said
    /// (card_4f7703a8b575).
    ///
    /// `set_allow_host_runner(true)` is what makes the test non-vacuous: with
    /// host execution off, an imageless job is refused for a completely
    /// different reason and the assertion below would pass over a runner that
    /// never looked at a label in its life.
    #[tokio::test]
    async fn a_job_labelled_for_another_machine_is_refused_by_the_runner() {
        let temp = tempfile::tempdir().unwrap();
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("labels.db").display()
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
            "label-owner",
            "label-owner@example.com",
            "unused",
            "Label Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("labels".into()),
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
        let repo_path = temp.path().join("repos/label-owner/labels.git");
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

        let queue_job = |script: &'static str| {
            let db = db.clone();
            let commit_sha = commit_sha.clone();
            async move {
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
                    "gpujob",
                    script,
                    None,
                    Some(r#"["my-gpu-box"]"#),
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
                (pipeline.id, job.id)
            }
        };

        // Host execution is ON, so nothing but the routing rule can stop this.
        let (pipeline_id, job_id) = queue_job("echo should-not-run").await;
        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, pipeline_id);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();

        let completed = rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            completed.status, "failed",
            "a job routed at another machine must not be reported as this machine's success"
        );
        let log = completed.log.unwrap_or_default();
        assert!(
            log.contains("my-gpu-box"),
            "the refusal must name the label the job asked for: {log}"
        );
        assert!(
            log.contains("ci.runner_labels") && log.contains("ci.external_runners"),
            "the refusal must name what to change: {log}"
        );
        assert!(
            !log.contains("should-not-run"),
            "the job body ran on the machine it asked to be routed away from: {log}"
        );

        // And the other half of the same rule: an instance that declares it IS
        // that machine runs the job. Without this, deleting the labels would
        // leave the assertions above green over a runner that refuses
        // everything.
        let (covered_pipeline, covered_job) = queue_job("echo did-run").await;
        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, covered_pipeline);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.set_runner_labels(vec!["MY-GPU-BOX".into()]);
        runner.run().await.unwrap();

        let completed = rg_db::ops::pipeline_ops::get_job(&db, covered_job)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            completed.status, "success",
            "a label this instance declares it carries is honoured by running the job: {:?}",
            completed.log
        );
        assert!(
            completed.log.unwrap_or_default().contains("did-run"),
            "the job that matched this runner's labels never ran its script"
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

    // ── The metrics hook the embedded runner reports through ──────────

    static JOB_OUTCOMES: std::sync::Mutex<Vec<(String, Option<std::time::Duration>)>> =
        std::sync::Mutex::new(Vec::new());
    static PIPELINE_OUTCOMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    fn observe_job_outcome(status: &str, duration: Option<std::time::Duration>) {
        JOB_OUTCOMES
            .lock()
            .expect("job outcome sink")
            .push((status.to_string(), duration));
    }

    fn observe_pipeline_outcome(status: &str) {
        PIPELINE_OUTCOMES
            .lock()
            .expect("pipeline outcome sink")
            .push(status.to_string());
    }

    /// How long the observed job runs. The sink is process-global and every
    /// other test in this binary that finishes a pipeline appends to it too, so
    /// the assertion identifies its own job by an execution time no `echo` can
    /// reach rather than by a status string four tests share.
    const OBSERVED_JOB_SECS: u64 = 3;

    /// `ci_jobs_total` and `ci_job_duration_seconds` had one producer, and it
    /// sat in the external runner's `finish` handler. The embedded runner —
    /// the only executor on a default instance — settles its own jobs down
    /// here, below the recorder, so neither series was produced at all while a
    /// default instance built (card_e309fbb5a3fd). Same for
    /// `ci_pipelines_total`, which `HighPipelineFailureRate` divides by.
    #[tokio::test]
    async fn a_finished_job_and_pipeline_reach_the_metrics_hook_with_their_outcome() {
        rg_core::metrics_hook::set_ci_job_finished_observer(observe_job_outcome);
        rg_core::metrics_hook::set_ci_pipeline_finished_observer(observe_pipeline_outcome);

        let temp = tempfile::tempdir().unwrap();
        let (db, repo, repo_path, commit_sha, user_id) =
            repo_with_one_commit(temp.path(), "metrics").await;
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            &commit_sha,
            "refs/heads/main",
            "manual",
            Some(user_id),
        )
        .await
        .unwrap();
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
            .await
            .unwrap();
        let job = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "observed",
            &format!("sleep {OBSERVED_JOB_SECS}"),
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

        let pipelines_before = PIPELINE_OUTCOMES.lock().expect("pipeline sink").len();

        let mut runner = PipelineRunner::new_local_only(db.clone(), &repo_path, pipeline.id);
        runner.set_repo_id(repo.id);
        runner.set_allow_host_runner(true);
        runner.run().await.unwrap();

        let completed = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, "success", "{:?}", completed.log);

        let observed = JOB_OUTCOMES.lock().expect("job outcome sink").clone();
        let slow_success = observed.iter().find(|(status, duration)| {
            status == "success"
                && duration.is_some_and(|measured| {
                    measured >= std::time::Duration::from_secs(OBSERVED_JOB_SECS)
                })
        });
        assert!(
            slow_success.is_some(),
            "no job outcome carried this job's execution time — the embedded runner reported              none of {observed:?}"
        );

        let pipelines = PIPELINE_OUTCOMES.lock().expect("pipeline sink").clone();
        assert!(
            pipelines.len() > pipelines_before,
            "the pipeline reached a terminal status without reporting one: {pipelines:?}"
        );
        assert!(
            pipelines[pipelines_before..].contains(&"success".to_string()),
            "the pipeline succeeded but reported {:?}",
            &pipelines[pipelines_before..]
        );
    }
}
