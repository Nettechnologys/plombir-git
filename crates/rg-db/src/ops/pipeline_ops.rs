//! Database operations for CI/CD pipelines.

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::*;

use crate::entities::{pipeline, pipeline_concurrency_lock, pipeline_job, pipeline_stage};

// ── Pipeline ops ─────────────────────────────────────────────────

/// Create a new pipeline record.
///
/// Takes any [`ConnectionTrait`] — a pool *or* a transaction — because a
/// pipeline row on its own is not a pipeline: until its stages and jobs are
/// written too, its graph is a subset of what the CI config declared, and the
/// job scheduler cannot tell the two apart. `rg_ci::trigger_pipeline` therefore
/// writes the whole graph through one transaction; see the note there.
pub async fn create_pipeline(
    db: &impl ConnectionTrait,
    repo_id: i64,
    commit_sha: &str,
    ref_name: &str,
    trigger_type: &str,
    triggered_by: Option<i64>,
) -> Result<pipeline::Model> {
    create_pipeline_row(
        db,
        NewPipeline {
            repo_id,
            commit_sha,
            ref_name,
            trigger_type,
            triggered_by,
            concurrency_group: None,
            dispatch_inputs: None,
            base_branch: None,
            previous_sha: None,
        },
    )
    .await
}

/// Everything a pipeline row records about the run that produced it.
///
/// A struct rather than nine positional arguments because four of them — the
/// concurrency group, the dispatch inputs, the base branch and the previous
/// revision — are all `Option<&str>` and would sit next to each other:
/// swapping any two compiles, and produces a pipeline serialized on a JSON blob
/// while its inputs are silently a group name, or one filtered on a commit sha
/// while its diff is taken against a branch name.
pub struct NewPipeline<'a> {
    pub repo_id: i64,
    pub commit_sha: &'a str,
    pub ref_name: &'a str,
    pub trigger_type: &'a str,
    pub triggered_by: Option<i64>,
    /// The resolved `concurrency.group` this pipeline joins — the value
    /// [`find_active_pipelines_by_group`] matches on. `None` is the honest
    /// answer for a workflow with no `concurrency:` block: it neither waits for
    /// a group nor is cancelled by one.
    pub concurrency_group: Option<&'a str>,
    /// The caller's own `workflow_dispatch` inputs as a JSON object, for the
    /// retry that has to run this pipeline again with the values it was started
    /// with. See [`pipeline::Model::dispatch_inputs`].
    pub dispatch_inputs: Option<&'a str>,
    /// The branch this run's `on:` filters were matched against, for the retry
    /// that has to be filtered the same way. See
    /// [`pipeline::Model::base_branch`].
    pub base_branch: Option<&'a str>,
    /// Where the ref stood before the event that produced this run, for the
    /// retry whose `paths:` filters have to see the same diff. See
    /// [`pipeline::Model::previous_sha`].
    pub previous_sha: Option<&'a str>,
}

/// Create a pipeline row recording the full provenance of its run.
///
/// [`create_pipeline`] is the plain spelling for a run whose provenance is the
/// event, the ref and the commit alone.
pub async fn create_pipeline_row(
    db: &impl ConnectionTrait,
    new: NewPipeline<'_>,
) -> Result<pipeline::Model> {
    let now = chrono::Utc::now().naive_utc();
    let model = pipeline::ActiveModel {
        repo_id: Set(new.repo_id),
        commit_sha: Set(new.commit_sha.to_string()),
        ref_name: Set(new.ref_name.to_string()),
        status: Set("pending".to_string()),
        trigger_type: Set(new.trigger_type.to_string()),
        triggered_by: Set(new.triggered_by),
        concurrency_group: Set(new.concurrency_group.map(str::to_string)),
        dispatch_inputs: Set(new.dispatch_inputs.map(str::to_string)),
        base_branch: Set(new.base_branch.map(str::to_string)),
        previous_sha: Set(new.previous_sha.map(str::to_string)),
        started_at: Set(None),
        finished_at: Set(None),
        created_at: Set(now),
        ..Default::default()
    };
    let result = model.insert(db).await.context("db: create pipeline")?;
    Ok(result)
}

/// Get a pipeline by ID.
pub async fn get_pipeline(db: &impl ConnectionTrait, id: i64) -> Result<Option<pipeline::Model>> {
    pipeline::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: get pipeline")
}

/// Paginated list of pipelines for a repo. Returns (data, total).
pub async fn list_pipelines_by_repo_paginated(
    db: &DatabaseConnection,
    repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<pipeline::Model>, i64)> {
    let base = pipeline::Entity::find()
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .order_by_desc(pipeline::Column::CreatedAt)
        .order_by_desc(pipeline::Column::Id);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count pipelines by repo")? as i64;
    let pipelines = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list pipelines by repo (paginated)")?;

    Ok((pipelines, total))
}

/// Every job id owned by `repo_id`, walked repo → pipeline → stage → job.
///
/// Repository deletion needs this because CI artifacts are stored under
/// `artifacts/jobs/<job_id>/…` — a key that carries the job, not the
/// repository, so there is no single storage prefix to hand to the deletion
/// path. Ownership has to be read out of the database instead of guessed from
/// a string prefix.
///
/// Returns bare ids through three id-only statements rather than the
/// per-stage walk [`crate::ops::artifact_ops::list_by_pipeline`] does: a
/// repository with a long CI history would otherwise cost one query per
/// stage, and the caller has no use for the rows themselves.
pub async fn list_job_ids_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<i64>> {
    let pipeline_ids: Vec<i64> = pipeline::Entity::find()
        .select_only()
        .column(pipeline::Column::Id)
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .into_tuple()
        .all(db)
        .await
        .context("db: list pipeline ids by repo")?;
    if pipeline_ids.is_empty() {
        return Ok(Vec::new());
    }

    let stage_ids: Vec<i64> = pipeline_stage::Entity::find()
        .select_only()
        .column(pipeline_stage::Column::Id)
        .filter(pipeline_stage::Column::PipelineId.is_in(pipeline_ids))
        .into_tuple()
        .all(db)
        .await
        .context("db: list stage ids by repo")?;
    if stage_ids.is_empty() {
        return Ok(Vec::new());
    }

    pipeline_job::Entity::find()
        .select_only()
        .column(pipeline_job::Column::Id)
        .filter(pipeline_job::Column::StageId.is_in(stage_ids))
        .into_tuple()
        .all(db)
        .await
        .context("db: list job ids by repo")
}

/// Update pipeline status.
pub async fn update_pipeline_status(
    db: &impl ConnectionTrait,
    id: i64,
    status: &str,
    started_at: Option<chrono::NaiveDateTime>,
    finished_at: Option<chrono::NaiveDateTime>,
) -> Result<()> {
    let model = pipeline::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find pipeline for status update")?
        .ok_or_else(|| anyhow::anyhow!("pipeline {} not found", id))?;

    let mut active: pipeline::ActiveModel = model.into();
    active.status = Set(status.to_string());
    if started_at.is_some() {
        active.started_at = Set(started_at);
    }
    if finished_at.is_some() {
        active.finished_at = Set(finished_at);
    }
    active
        .update(db)
        .await
        .context("db: update pipeline status")?;
    Ok(())
}

// ── Stage ops ────────────────────────────────────────────────────

/// Create a pipeline stage.
///
/// Connection-agnostic for the same reason as [`create_pipeline`].
pub async fn create_stage(
    db: &impl ConnectionTrait,
    pipeline_id: i64,
    name: &str,
    stage_order: i32,
) -> Result<pipeline_stage::Model> {
    let model = pipeline_stage::ActiveModel {
        pipeline_id: Set(pipeline_id),
        name: Set(name.to_string()),
        stage_order: Set(stage_order),
        status: Set("pending".to_string()),
        started_at: Set(None),
        finished_at: Set(None),
        ..Default::default()
    };
    let result = model.insert(db).await.context("db: create stage")?;
    Ok(result)
}

/// Get stages for a pipeline.
pub async fn list_stages_by_pipeline(
    db: &impl ConnectionTrait,
    pipeline_id: i64,
) -> Result<Vec<pipeline_stage::Model>> {
    pipeline_stage::Entity::find()
        .filter(pipeline_stage::Column::PipelineId.eq(pipeline_id))
        .order_by_asc(pipeline_stage::Column::StageOrder)
        .all(db)
        .await
        .context("db: list stages by pipeline")
}

/// Update stage status.
pub async fn update_stage_status(
    db: &impl ConnectionTrait,
    id: i64,
    status: &str,
    started_at: Option<chrono::NaiveDateTime>,
    finished_at: Option<chrono::NaiveDateTime>,
) -> Result<()> {
    let model = pipeline_stage::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find stage for status update")?
        .ok_or_else(|| anyhow::anyhow!("stage {} not found", id))?;

    let mut active: pipeline_stage::ActiveModel = model.into();
    active.status = Set(status.to_string());
    if started_at.is_some() {
        active.started_at = Set(started_at);
    }
    if finished_at.is_some() {
        active.finished_at = Set(finished_at);
    }
    active.update(db).await.context("db: update stage status")?;
    Ok(())
}

// ── Job ops ──────────────────────────────────────────────────────

/// What a job that names no `when:` waits for.
///
/// The value is the CI engine's, but the fallback is applied here — a caller
/// that has nothing to say about `when` passes `None`, and this is the row that
/// gets written. `docs/ci.md` states it in the `when` row of its job table, so
/// it is a name rather than a literal: the check that holds that page to this
/// value has to have something to hold it to (`rg-ci/src/config.rs`).
pub const DEFAULT_JOB_WHEN: &str = "on_success";

/// Create a pipeline job.
///
/// A job is born `pending`, i.e. schedulable the moment it is visible — so a
/// caller building several of them writes through a transaction and lets the
/// commit publish them all at once (see [`create_pipeline`]).
// Wide by design: mirrors the pipeline_job column set (a params struct would just
// re-list the same fields with no call-site clarity gain).
#[allow(clippy::too_many_arguments)]
pub async fn create_job(
    db: &impl ConnectionTrait,
    stage_id: i64,
    name: &str,
    script: &str,
    image: Option<&str>,
    tags: Option<&str>,
    variables: Option<&str>,
    cache_key: Option<&str>,
    cache_paths: Option<&str>,
    artifacts: Option<&str>,
    allow_failure: bool,
    timeout_seconds: Option<i64>,
    when_condition: Option<&str>,
    if_condition: Option<&str>,
) -> Result<pipeline_job::Model> {
    let when_condition = when_condition.unwrap_or(DEFAULT_JOB_WHEN);
    let model = pipeline_job::ActiveModel {
        stage_id: Set(stage_id),
        name: Set(name.to_string()),
        script: Set(script.to_string()),
        variables: Set(variables.map(str::to_string)),
        cache_key: Set(cache_key.map(str::to_string)),
        cache_paths: Set(cache_paths.map(str::to_string)),
        artifacts: Set(artifacts.map(str::to_string)),
        allow_failure: Set(allow_failure),
        timeout_seconds: Set(timeout_seconds),
        when_condition: Set(when_condition.to_string()),
        if_condition: Set(if_condition.map(str::to_string)),
        environment_id: Set(None),
        environment_name: Set(None),
        image: Set(image.map(|s| s.to_string())),
        status: Set(if when_condition == "manual" {
            "manual".to_string()
        } else {
            "pending".to_string()
        }),
        tags: Set(tags.map(|s| s.to_string())),
        exit_code: Set(None),
        log: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        ..Default::default()
    };
    let result = model.insert(db).await.context("db: create job")?;
    Ok(result)
}

/// Atomically release a manual job for execution. Returns false if another
/// request already released it or the job is not manual.
pub async fn play_manual_job(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let now = chrono::Utc::now().naive_utc();
    let result = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(id))
        .filter(pipeline_job::Column::Status.eq("manual"))
        .filter(pipeline_job::Column::WhenCondition.eq("manual"))
        .col_expr(pipeline_job::Column::Status, Expr::value("pending"))
        .col_expr(
            pipeline_job::Column::RunnerId,
            Expr::value(sea_orm::Value::BigInt(None)),
        )
        .col_expr(pipeline_job::Column::UpdatedAt, Expr::value(now))
        .exec(db)
        .await
        .context("db: play manual job")?;
    Ok(result.rows_affected == 1)
}

/// Put a stage and pipeline back into schedulable state after a manual job is
/// released. Existing start timestamps are retained for duration accounting.
pub async fn resume_pipeline_chain(
    db: &DatabaseConnection,
    pipeline_id: i64,
    stage_id: i64,
) -> Result<()> {
    pipeline_stage::Entity::update_many()
        .filter(pipeline_stage::Column::Id.eq(stage_id))
        .filter(pipeline_stage::Column::Status.eq("manual"))
        .col_expr(pipeline_stage::Column::Status, Expr::value("pending"))
        .col_expr(
            pipeline_stage::Column::FinishedAt,
            Expr::value(sea_orm::Value::ChronoDateTime(None)),
        )
        .exec(db)
        .await
        .context("db: resume manual stage")?;
    pipeline::Entity::update_many()
        .filter(pipeline::Column::Id.eq(pipeline_id))
        .filter(pipeline::Column::Status.eq("manual"))
        .col_expr(pipeline::Column::Status, Expr::value("pending"))
        .col_expr(
            pipeline::Column::FinishedAt,
            Expr::value(sea_orm::Value::ChronoDateTime(None)),
        )
        .exec(db)
        .await
        .context("db: resume manual pipeline")?;
    Ok(())
}

pub async fn resume_approval_chain(
    db: &DatabaseConnection,
    pipeline_id: i64,
    stage_id: i64,
) -> Result<()> {
    pipeline_stage::Entity::update_many()
        .filter(pipeline_stage::Column::Id.eq(stage_id))
        .filter(pipeline_stage::Column::Status.eq("waiting_approval"))
        .col_expr(pipeline_stage::Column::Status, Expr::value("pending"))
        .exec(db)
        .await
        .context("db: resume approved stage")?;
    pipeline::Entity::update_many()
        .filter(pipeline::Column::Id.eq(pipeline_id))
        .filter(pipeline::Column::Status.eq("waiting_approval"))
        .col_expr(pipeline::Column::Status, Expr::value("pending"))
        .exec(db)
        .await
        .context("db: resume approved pipeline")?;
    Ok(())
}

/// Get a job by ID.
pub async fn get_job(db: &DatabaseConnection, id: i64) -> Result<Option<pipeline_job::Model>> {
    pipeline_job::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: get job")
}

/// Finalize a CI job-token proof while the job can still speak for its run.
///
/// The caller has already checked the job's stage, pipeline and repository
/// binding. This conditional self-assignment is the final lifecycle boundary:
/// it contends with cancellation/completion and cascade deletion after those
/// reads, then returns the fresh job from the same retryable transaction.
/// `None` is a typed lifecycle loss; database failure remains `Err`.
///
/// The self-assignment is deliberate. It takes the row write lock without
/// claiming that token use edited the job. MySQL may report zero affected rows
/// for it, so the scoped re-read — not `rows_affected` alone — is the portable
/// success test.
pub async fn finalize_ci_job_token_job(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<pipeline_job::Model>> {
    const TOKEN_BEARING_STATUSES: [&str; 2] = ["assigned", "running"];

    crate::contention::retry_transaction("CI job-token lifecycle finalization", || async move {
        let transaction = db
            .begin()
            .await
            .context("db: begin CI job-token lifecycle finalization")?;
        let result: Result<Option<pipeline_job::Model>> = async {
            let update = pipeline_job::Entity::update_many()
                .col_expr(
                    pipeline_job::Column::Status,
                    Expr::col(pipeline_job::Column::Status).into(),
                )
                .filter(pipeline_job::Column::Id.eq(id))
                .filter(pipeline_job::Column::Status.is_in(TOKEN_BEARING_STATUSES))
                .exec(&transaction)
                .await
                .context("db: finalize CI job-token lifecycle")?;

            match update.rows_affected {
                0 | 1 => pipeline_job::Entity::find_by_id(id)
                    .filter(pipeline_job::Column::Status.is_in(TOKEN_BEARING_STATUSES))
                    .one(&transaction)
                    .await
                    .context("db: find job after CI job-token lifecycle finalization"),
                rows => anyhow::bail!(
                    "db: CI job-token lifecycle finalization affected {rows} rows for job {id}"
                ),
            }
        }
        .await;

        match result {
            Ok(job) => {
                transaction
                    .commit()
                    .await
                    .context("db: commit CI job-token lifecycle finalization")?;
                Ok(job)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!(
                        "db: roll back CI job-token lifecycle finalization: {rollback_error}"
                    ));
                }
                Err(error)
            }
        }
    })
    .await
}

/// Update the log and the watchdog liveness timestamp of a job.
///
/// A log chunk is direct evidence that the owning runner is still executing
/// this job. Keeping `updated_at` in the same write prevents the watchdog from
/// handing a healthy, long-running job to a second runner.
pub async fn update_job_log(db: &DatabaseConnection, id: i64, log: &str) -> Result<()> {
    use sea_orm::ActiveModelTrait;
    let model = pipeline_job::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find job for log update")?
        .ok_or_else(|| anyhow::anyhow!("job {} not found", id))?;

    let mut active: pipeline_job::ActiveModel = model.into();
    active.log = Set(Some(log.to_string()));
    active.updated_at = Set(Some(chrono::Utc::now().naive_utc()));
    active.update(db).await.context("db: update job log")?;
    Ok(())
}

/// List jobs for a stage.
pub async fn list_jobs_by_stage(
    db: &impl ConnectionTrait,
    stage_id: i64,
) -> Result<Vec<pipeline_job::Model>> {
    pipeline_job::Entity::find()
        .filter(pipeline_job::Column::StageId.eq(stage_id))
        .all(db)
        .await
        .context("db: list jobs by stage")
}

pub async fn stage_has_job_status(
    db: &DatabaseConnection,
    stage_id: i64,
    status: &str,
) -> Result<bool> {
    Ok(pipeline_job::Entity::find()
        .filter(pipeline_job::Column::StageId.eq(stage_id))
        .filter(pipeline_job::Column::Status.eq(status))
        .count(db)
        .await
        .context("db: count jobs by stage status")?
        > 0)
}

/// Update job result.
///
/// Connection-agnostic: pipeline creation settles the status of jobs it skips
/// inside the same transaction that wrote them.
pub async fn update_job_result(
    db: &impl ConnectionTrait,
    id: i64,
    status: &str,
    exit_code: Option<i32>,
    log: Option<&str>,
    started_at: Option<chrono::NaiveDateTime>,
    finished_at: Option<chrono::NaiveDateTime>,
) -> Result<()> {
    let model = pipeline_job::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find job for result update")?
        .ok_or_else(|| anyhow::anyhow!("job {} not found", id))?;

    let mut active: pipeline_job::ActiveModel = model.into();
    active.status = Set(status.to_string());
    active.exit_code = Set(exit_code);
    active.updated_at = Set(Some(chrono::Utc::now().naive_utc()));
    if log.is_some() {
        active.log = Set(log.map(|s| s.to_string()));
    }
    if started_at.is_some() {
        active.started_at = Set(started_at);
    }
    if finished_at.is_some() {
        active.finished_at = Set(finished_at);
    }
    active.update(db).await.context("db: update job result")?;
    Ok(())
}

/// Get a stage by ID.
pub async fn get_stage_by_id(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<pipeline_stage::Model>> {
    pipeline_stage::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: get stage by id")
}

/// List all jobs for a pipeline (across all stages).
pub async fn list_jobs_by_pipeline(
    db: &DatabaseConnection,
    pipeline_id: i64,
) -> Result<Vec<pipeline_job::Model>> {
    // First get all stages for this pipeline
    let stages = pipeline_stage::Entity::find()
        .filter(pipeline_stage::Column::PipelineId.eq(pipeline_id))
        .all(db)
        .await
        .context("db: list stages for jobs")?;

    let stage_ids: Vec<i64> = stages.iter().map(|s| s.id).collect();
    if stage_ids.is_empty() {
        return Ok(Vec::new());
    }

    pipeline_job::Entity::find()
        .filter(pipeline_job::Column::StageId.is_in(stage_ids))
        .all(db)
        .await
        .context("db: list jobs by pipeline")
}

/// Find the latest pipeline for a repo + commit SHA.
/// Used by branch protection status checks to verify CI passed.
pub async fn find_latest_by_repo_and_commit(
    db: &DatabaseConnection,
    repo_id: i64,
    commit_sha: &str,
) -> Result<Option<pipeline::Model>> {
    pipeline::Entity::find()
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .filter(pipeline::Column::CommitSha.eq(commit_sha))
        .order_by_desc(pipeline::Column::CreatedAt)
        .limit(1)
        .one(db)
        .await
        .context("db: find latest pipeline by repo and commit")
}

/// Find the merge-queue pipeline for a repo + speculative merge-group commit.
///
/// The merge queue creates the pipeline before it can record ownership of it on
/// the queue entry, so the two can disagree. The group commit is built
/// deterministically, which makes its SHA the key that finds an already-created
/// pipeline again instead of building a second one (card_55282a865b8e).
/// `trigger_type` is part of the filter because nothing but the queue may be
/// adopted this way.
pub async fn find_merge_group_pipeline(
    db: &DatabaseConnection,
    repo_id: i64,
    commit_sha: &str,
) -> Result<Option<pipeline::Model>> {
    pipeline::Entity::find()
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .filter(pipeline::Column::CommitSha.eq(commit_sha))
        .filter(pipeline::Column::TriggerType.eq("merge_group"))
        .order_by_desc(pipeline::Column::Id)
        .one(db)
        .await
        .context("db: find merge-group pipeline by commit")
}

// ── Status cascade helpers ──────────────────────────
// After a job finishes, check if its stage is done; if so, update stage status.
// After a stage finishes, check if all stages in the pipeline are done; if so, update pipeline status.

/// Check if all jobs in a stage are finished.
/// Returns (all_done, any_failure).
pub async fn check_stage_jobs(db: &DatabaseConnection, stage_id: i64) -> Result<(bool, bool)> {
    let jobs = list_jobs_by_stage(db, stage_id).await?;
    if jobs.is_empty() {
        return Ok((true, false));
    }
    let all_done = jobs.iter().all(|j| is_finished_pipeline_work(&j.status));
    let any_failure = jobs
        .iter()
        .any(|j| is_unsuccessful_outcome(&j.status) && !j.allow_failure);
    Ok((all_done, any_failure))
}

/// If every non-manual job in a stage has finished and a manual job remains,
/// expose the persisted gate on both the stage and pipeline.
pub async fn try_pause_stage_at_manual(db: &DatabaseConnection, stage_id: i64) -> Result<bool> {
    let jobs = list_jobs_by_stage(db, stage_id).await?;
    let gate_status = if jobs.iter().any(|job| job.status == "waiting_approval") {
        Some("waiting_approval")
    } else if jobs.iter().any(|job| job.status == "manual") {
        Some("manual")
    } else {
        None
    };
    let automatic_done = jobs
        .iter()
        .filter(|job| !matches!(job.status.as_str(), "manual" | "waiting_approval"))
        .all(|job| is_finished_pipeline_work(&job.status));
    let Some(gate_status) = gate_status else {
        return Ok(false);
    };
    if !automatic_done {
        return Ok(false);
    }
    let stage = get_stage_by_id(db, stage_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("stage {} not found", stage_id))?;
    if !settle_stage_if_active(db, stage_id, gate_status, None, None).await? {
        return Ok(false);
    }
    settle_pipeline_if_active(db, stage.pipeline_id, gate_status, None, None).await?;
    Ok(true)
}

/// After a job finishes, update stage status if all jobs in the stage are done.
/// Returns the new stage status if updated, or None if not all done.
///
/// `None` also covers "the stage already settled as something else" — a
/// cancellation that landed while this job was executing. The caller must not
/// roll the pipeline up on it: see [`settle_stage_if_active`].
pub async fn try_update_stage(db: &DatabaseConnection, stage_id: i64) -> Result<Option<String>> {
    if try_pause_stage_at_manual(db, stage_id).await? {
        return Ok(Some("manual".to_string()));
    }
    let (all_done, any_failure) = check_stage_jobs(db, stage_id).await?;
    if !all_done {
        return Ok(None);
    }
    let new_status = if any_failure { "failed" } else { "success" };
    let now = Some(chrono::Utc::now().naive_utc());
    if !settle_stage_if_active(db, stage_id, new_status, None, now).await? {
        return Ok(None);
    }
    if any_failure {
        skip_downstream_stages(db, stage_id).await?;
    }
    Ok(Some(new_status.to_string()))
}

async fn skip_downstream_stages(db: &DatabaseConnection, failed_stage_id: i64) -> Result<()> {
    let Some(failed_stage) = get_stage_by_id(db, failed_stage_id).await? else {
        return Ok(());
    };
    let now = Some(chrono::Utc::now().naive_utc());
    for stage in list_stages_by_pipeline(db, failed_stage.pipeline_id)
        .await?
        .into_iter()
        .filter(|stage| stage.stage_order > failed_stage.stage_order)
    {
        let jobs = list_jobs_by_stage(db, stage.id).await?;
        if jobs
            .iter()
            .any(|job| matches!(job.status.as_str(), "assigned" | "running"))
        {
            continue;
        }
        for job in jobs {
            if !is_finished_pipeline_work(&job.status) {
                update_job_result(db, job.id, "skipped", None, None, None, now).await?;
            }
        }
        if !is_finished_pipeline_work(&stage.status) {
            update_stage_status(db, stage.id, "skipped", None, now).await?;
        }
    }
    Ok(())
}

/// Check if all stages in a pipeline are done.
/// Returns (all_done, any_failure).
pub async fn check_pipeline_stages(
    db: &DatabaseConnection,
    pipeline_id: i64,
) -> Result<(bool, bool)> {
    let stages = list_stages_by_pipeline(db, pipeline_id).await?;
    if stages.is_empty() {
        return Ok((true, false));
    }
    let all_done = stages
        .iter()
        .all(|stage| is_finished_pipeline_work(&stage.status));
    let any_failure = stages
        .iter()
        .any(|stage| is_unsuccessful_outcome(&stage.status));
    Ok((all_done, any_failure))
}

/// After a stage finishes, update pipeline status if all stages are done.
///
/// A canceled pipeline stays canceled: `check_pipeline_stages` counts a
/// `canceled` stage as done, so without the guard the last late job would roll
/// the pipeline up out of `canceled` and contradict the answer the cancellation
/// already gave. The guard is what holds that, not the roll-up verdict — but
/// the verdict is `failed` rather than `success` for the same reason, so the
/// day this is reached by some path the guard does not cover, the success hooks
/// still do not fire.
pub async fn try_update_pipeline(
    db: &DatabaseConnection,
    pipeline_id: i64,
) -> Result<Option<String>> {
    let (all_done, any_failure) = check_pipeline_stages(db, pipeline_id).await?;
    if !all_done {
        return Ok(None);
    }
    let new_status = if any_failure { "failed" } else { "success" };
    let now = Some(chrono::Utc::now().naive_utc());
    if !settle_pipeline_if_active(db, pipeline_id, new_status, None, now).await? {
        return Ok(None);
    }
    Ok(Some(new_status.to_string()))
}

/// The tags a job asks for that a runner carrying `runner_labels` does not have.
///
/// The one place the routing rule is spelled. It used to live only inside
/// [`find_pending_job_matching_labels`], which is reachable from the poll route
/// external runners use and from nowhere else — so the in-process runner, which
/// executes jobs without ever asking this question, had no rule to disagree
/// with and simply ran everything (card_4f7703a8b575). A second copy written
/// next to that runner would be a second rule; this is the same one.
///
/// Empty result = the runner may take the job, which includes the untagged job
/// every runner may take. Matching is case-insensitive because the labels are
/// typed by hand at both ends — in a workflow's `runs-on:` and in the operator's
/// `ci.runner_labels`.
pub fn uncovered_job_tags(job_tags: &[String], runner_labels: &[String]) -> Vec<String> {
    let labels_lower: Vec<String> = runner_labels.iter().map(|l| l.to_lowercase()).collect();
    job_tags
        .iter()
        .filter(|tag| !labels_lower.contains(&tag.to_lowercase()))
        .cloned()
        .collect()
}

/// Decode `pipeline_jobs.tags`, the JSON array the column stores.
///
/// `Ok(empty)` for a job that asks for nothing; `Err` for a body that is not a
/// list of strings. The two callers answer a malformed value differently on
/// purpose — the scheduler skips the row, the in-process runner refuses to run
/// it — so this returns the failure rather than choosing for them.
pub fn decode_job_tags(raw: Option<&str>) -> std::result::Result<Vec<String>, serde_json::Error> {
    match raw {
        Some(tags) => serde_json::from_str::<Vec<String>>(tags),
        None => Ok(Vec::new()),
    }
}

/// Find a pending job that matches the given runner labels.
///
/// A job matches if:
/// - It has no tags (any runner can pick it up)
/// - OR every one of its tags matches one of the runner's labels
pub async fn find_pending_job_matching_labels(
    db: &DatabaseConnection,
    runner_labels: &[String],
) -> Result<Option<pipeline_job::Model>> {
    let all_pending: Vec<pipeline_job::Model> = pipeline_job::Entity::find()
        .filter(pipeline_job::Column::Status.eq("pending"))
        .filter(pipeline_job::Column::RunnerId.is_null())
        .order_by_asc(pipeline_job::Column::Id)
        .all(db)
        .await
        .context("db: find pending jobs")?;

    for job in all_pending {
        if !job_is_schedulable(db, &job).await? {
            continue;
        }
        let job_tags = match decode_job_tags(job.tags.as_deref()) {
            Ok(tags) => tags,
            Err(error) => {
                tracing::warn!(
                    job_id = job.id,
                    error = %error,
                    "skipping pending job with malformed runner tags"
                );
                continue;
            }
        };

        if uncovered_job_tags(&job_tags, runner_labels).is_empty() {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

async fn job_is_schedulable(db: &DatabaseConnection, job: &pipeline_job::Model) -> Result<bool> {
    let Some(stage) = get_stage_by_id(db, job.stage_id).await? else {
        return Ok(false);
    };
    if matches!(
        stage.status.as_str(),
        "manual" | "waiting_approval" | "canceled"
    ) {
        return Ok(false);
    }
    let Some(pipeline) = get_pipeline(db, stage.pipeline_id).await? else {
        return Ok(false);
    };
    if !matches!(pipeline.status.as_str(), "pending" | "running") {
        return Ok(false);
    }
    let stages = list_stages_by_pipeline(db, stage.pipeline_id).await?;
    Ok(stages
        .iter()
        .filter(|candidate| candidate.stage_order < stage.stage_order)
        .all(|candidate| candidate.status == "success"))
}

#[cfg(test)]
mod job_tag_matching_tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database};

    async fn setup_with_job(tags: Option<&str>) -> (DatabaseConnection, i64) {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect to in-memory db");
        crate::run_migrations(&db).await.expect("run migrations");
        for statement in [
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'ci-tags', 'ci-tags@test.com', 'x', 0, 1, '2024-01-01', '2024-01-01')",
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) \
             VALUES(1, 1, 'ci-tags', 0, 'main', 0, 0, '2024-01-01', '2024-01-01')",
        ] {
            db.execute(Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                statement,
            ))
            .await
            .expect("seed row");
        }
        let pipeline = create_pipeline(
            &db,
            1,
            "1111111111111111111111111111111111111111",
            "refs/heads/main",
            "push",
            None,
        )
        .await
        .expect("create pipeline");
        let stage = create_stage(&db, pipeline.id, "test", 0)
            .await
            .expect("create stage");
        let job = create_job(
            &db,
            stage.id,
            "deploy",
            "echo deploy",
            None,
            tags,
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
        .expect("create job");
        (db, job.id)
    }

    #[tokio::test]
    async fn an_unlabelled_runner_cannot_take_a_tagged_job() {
        let (db, job_id) = setup_with_job(Some(r#"["prod-deploy"]"#)).await;

        let matched = find_pending_job_matching_labels(&db, &[])
            .await
            .expect("look for work for an unlabelled runner");

        assert!(
            matched.is_none(),
            "an empty runner label set must not erase a job's tag requirement"
        );
        let persisted = get_job(&db, job_id)
            .await
            .expect("reload tagged job")
            .expect("tagged job still exists");
        assert_eq!(persisted.status, "pending");
        assert_eq!(persisted.runner_id, None);
    }

    #[tokio::test]
    async fn one_matching_label_does_not_satisfy_all_job_tags() {
        let (db, job_id) = setup_with_job(Some(r#"["linux","prod-deploy"]"#)).await;

        let matched = find_pending_job_matching_labels(&db, &["linux".to_string()])
            .await
            .expect("look for work with only one required label");

        assert!(
            matched.is_none(),
            "one shared label must not erase the job's remaining tag requirements"
        );
        let persisted = get_job(&db, job_id)
            .await
            .expect("reload multiply-tagged job")
            .expect("multiply-tagged job still exists");
        assert_eq!(persisted.status, "pending");
        assert_eq!(persisted.runner_id, None);
    }

    #[tokio::test]
    async fn a_runner_carrying_all_required_labels_can_take_the_tagged_job() {
        let (db, job_id) = setup_with_job(Some(r#"["linux","PROD-DEPLOY"]"#)).await;

        let matched = find_pending_job_matching_labels(
            &db,
            &["prod-deploy".to_string(), "LINUX".to_string()],
        )
        .await
        .expect("look for matching tagged work")
        .expect("every required label matches regardless of case or order");

        assert_eq!(matched.id, job_id);
    }

    #[tokio::test]
    async fn an_unlabelled_runner_can_still_take_an_untagged_job() {
        for tags in [None, Some("[]")] {
            let (db, job_id) = setup_with_job(tags).await;

            let matched = find_pending_job_matching_labels(&db, &[])
                .await
                .expect("look for untagged work")
                .expect("an untagged job remains eligible");

            assert_eq!(matched.id, job_id);
        }
    }
}

/// Find stuck jobs: "assigned"/"running" but not updated within timeout.
pub async fn find_stuck_jobs(
    db: &DatabaseConnection,
    timeout_secs: i64,
) -> Result<Vec<pipeline_job::Model>> {
    let cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::seconds(timeout_secs);

    pipeline_job::Entity::find()
        .filter(pipeline_job::Column::Status.is_in(["assigned", "running"]))
        .filter(
            pipeline_job::Column::UpdatedAt
                .is_not_null()
                .and(pipeline_job::Column::UpdatedAt.lte(cutoff)),
        )
        .all(db)
        .await
        .context("db: find stuck jobs")
}

/// Pipelines that were still unfinished when a *previous* process stopped.
///
/// `created_before` is the instant this process started, and that is the whole
/// safety argument: the embedded runner lives in memory, so a pipeline created
/// before this process existed has no executor in it — nothing here can collide
/// with a run that is actually in flight. A wall-clock grace ("older than 30
/// seconds") would not give that: a slow startup could reach a pipeline this
/// process had just begun, and two runners on one pipeline execute the same
/// job twice.
///
/// Only `pending` / `running` are leftovers. `manual` and `waiting_approval`
/// are pipelines waiting for a *person*, which a restart does not interrupt —
/// resuming those would be answering a question nobody asked.
pub async fn find_interrupted_pipelines(
    db: &DatabaseConnection,
    created_before: chrono::NaiveDateTime,
) -> Result<Vec<pipeline::Model>> {
    pipeline::Entity::find()
        .filter(pipeline::Column::Status.is_in(["pending", "running"]))
        .filter(pipeline::Column::CreatedAt.lte(created_before))
        .order_by_asc(pipeline::Column::Id)
        .all(db)
        .await
        .context("db: find interrupted pipelines")
}

/// Refresh watchdog liveness for one job while it is still executing.
///
/// The status filter is intentional: a late heartbeat must not make a
/// canceled or otherwise settled job look active again. Returns whether the
/// job was still running and therefore touched.
pub async fn touch_running_job(db: &impl ConnectionTrait, job_id: i64) -> Result<bool> {
    let result = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .filter(pipeline_job::Column::Status.eq("running"))
        .col_expr(
            pipeline_job::Column::UpdatedAt,
            Expr::value(chrono::Utc::now().naive_utc()),
        )
        .exec(db)
        .await
        .context("db: touch running job")?;
    Ok(result.rows_affected == 1)
}

/// Refresh every running job owned by a runner that just heartbeated.
///
/// `assigned` is deliberately excluded: a live runner that accepted work but
/// never started it must still hit the watchdog's assignment deadline.
pub async fn touch_running_jobs_for_runner(
    db: &impl ConnectionTrait,
    runner_id: i64,
) -> Result<u64> {
    let result = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::RunnerId.eq(Some(runner_id)))
        .filter(pipeline_job::Column::Status.eq("running"))
        .col_expr(
            pipeline_job::Column::UpdatedAt,
            Expr::value(chrono::Utc::now().naive_utc()),
        )
        .exec(db)
        .await
        .context("db: touch runner jobs")?;
    Ok(result.rows_affected)
}

/// Reset a stuck job back to pending, unassigning the runner.
pub async fn reset_stuck_job(db: &DatabaseConnection, job_id: i64) -> Result<()> {
    let now = chrono::Utc::now().naive_utc();
    pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .col_expr(pipeline_job::Column::Status, Expr::value("pending"))
        .col_expr(
            pipeline_job::Column::RunnerId,
            Expr::value(sea_orm::Value::BigInt(None)),
        )
        .col_expr(pipeline_job::Column::UpdatedAt, Expr::value(now))
        .exec(db)
        .await
        .context("db: reset stuck job")?;
    Ok(())
}

/// Hand one still-active job back to the pool, unassigning its runner.
///
/// The single-job counterpart of [`reset_runner_jobs`], for an executor that has
/// a job id but no `runners` row to key on — the embedded runner, which is not
/// registered anywhere and so cannot be swept by runner id (card_34368880dc20).
///
/// Unlike [`reset_stuck_job`] the status is part of the `WHERE`: the watchdog
/// acts on a row nobody has touched for ten minutes, while this one races the
/// job it is interrupting. A cancellation or a finish that landed a moment
/// earlier already settled the row, and reviving it as `pending` would hand a
/// finished job back to be run a second time. Returns whether the row was still
/// active and therefore handed back.
pub async fn hand_back_active_job(db: &impl ConnectionTrait, job_id: i64) -> Result<bool> {
    let now = chrono::Utc::now().naive_utc();
    let result = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .filter(pipeline_job::Column::Status.is_in(["assigned", "running"]))
        .col_expr(pipeline_job::Column::Status, Expr::value("pending"))
        .col_expr(
            pipeline_job::Column::RunnerId,
            Expr::value(sea_orm::Value::BigInt(None)),
        )
        .col_expr(pipeline_job::Column::UpdatedAt, Expr::value(now))
        .exec(db)
        .await
        .context("db: hand active job back to the pool")?;
    Ok(result.rows_affected == 1)
}

/// Find offline runners: online/busy but no heartbeat within threshold.
pub async fn find_offline_runners(
    db: &DatabaseConnection,
    heartbeat_timeout_secs: i64,
) -> Result<Vec<crate::entities::runner::Model>> {
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(heartbeat_timeout_secs);
    use crate::entities::runner;
    runner::Entity::find()
        .filter(runner::Column::Status.is_in(["online", "busy"]))
        .filter(runner::Column::LastSeenAt.lt(cutoff))
        .all(db)
        .await
        .context("db: find offline runners")
}

/// Reset all jobs assigned to a runner back to pending (for deregistration).
///
/// Takes any [`ConnectionTrait`] — a pool *or* a transaction — because
/// deregistration has to reset the runner's jobs and delete its row as one
/// unit: a reset that lands without the delete leaves a live runner with no
/// work, and a delete that lands without the reset leaves jobs pointing at a
/// runner row that no longer exists, which nothing but the watchdog will ever
/// pick up. See `runner_ops::deregister_runner`.
pub async fn reset_runner_jobs(db: &impl ConnectionTrait, runner_id: i64) -> Result<u64> {
    let now = chrono::Utc::now().naive_utc();
    let result = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::RunnerId.eq(Some(runner_id)))
        .filter(pipeline_job::Column::Status.is_in(["assigned", "running"]))
        .col_expr(pipeline_job::Column::Status, Expr::value("pending"))
        .col_expr(
            pipeline_job::Column::RunnerId,
            Expr::value(sea_orm::Value::BigInt(None)),
        )
        .col_expr(pipeline_job::Column::UpdatedAt, Expr::value(now))
        .exec(db)
        .await
        .context("db: reset runner jobs")?;
    Ok(result.rows_affected)
}

/// Claim a CI job for a specific runner. Returns whether the claim landed.
///
/// This is a compare-and-swap, not a write: the `WHERE` names the exact state
/// the caller believed it was acting on — unclaimed work — and the database
/// decides the winner in one statement.
///
/// `poll_job` picks its candidate with `find_pending_job_matching_labels`
/// (`status = 'pending' AND runner_id IS NULL`) and writes afterwards, so
/// everything that can change in between has to be re-asserted here. A filter on
/// "still active work" covered only half of it: a *cancellation* landing in that
/// window was refused, but a competing **runner** was not — `assigned` is itself
/// an active status, so the second poller's write sailed through and overwrote
/// `runner_id`. Both runners had already been answered `200` with the job body;
/// the loser then found its own `/start` and `/finish` answered `404 job not
/// found`, because `assigned_job` matches on the runner the row now names. The
/// job it was told to run was never its own.
///
/// So the condition is the candidate query verbatim. `pending` alone would do in
/// production (nothing leaves a row `pending` with a runner still attached —
/// `reset_stuck_job` and `reset_runner_jobs` both null the column in the same
/// statement), but the claim is a safety property and it states both halves
/// rather than leaning on that invariant holding forever.
///
/// A refused claim is not an error: the row simply belongs to someone else now,
/// or the pipeline was canceled. `poll_job` looks for the next candidate.
pub async fn assign_job(db: &DatabaseConnection, job_id: i64, runner_id: i64) -> Result<bool> {
    let now = chrono::Utc::now().naive_utc();
    let result = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .filter(pipeline_job::Column::Status.eq("pending"))
        .filter(pipeline_job::Column::RunnerId.is_null())
        .col_expr(pipeline_job::Column::Status, Expr::value("assigned"))
        .col_expr(pipeline_job::Column::RunnerId, Expr::value(runner_id))
        .col_expr(pipeline_job::Column::UpdatedAt, Expr::value(now))
        .exec(db)
        .await
        .context("db: assign job")?;
    Ok(result.rows_affected > 0)
}

// ── Concurrency Control ──────────────────────────────────────────

/// Lock one repository-local `concurrency.group` until the surrounding
/// transaction commits or rolls back.
///
/// The row is a durable lock identity, not state that has to be released. An
/// INSERT creates the identity for a new group; a conflicting UPSERT performs a
/// real `touched_at` update. PostgreSQL/MySQL therefore take the unique row's
/// write lock, while SQLite takes its normal transaction writer lock. The caller
/// must acquire this before reading active pipelines and keep the same
/// transaction through cancellation plus graph publication, otherwise the
/// empty-group check remains a check-then-insert race.
pub async fn acquire_pipeline_concurrency_lock(
    db: &impl ConnectionTrait,
    repo_id: i64,
    group: &str,
) -> Result<()> {
    let now = chrono::Utc::now();
    pipeline_concurrency_lock::Entity::insert(pipeline_concurrency_lock::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        group_name: Set(group.to_string()),
        touched_at: Set(now),
    })
    .on_conflict(
        OnConflict::columns([
            pipeline_concurrency_lock::Column::RepoId,
            pipeline_concurrency_lock::Column::GroupName,
        ])
        .update_column(pipeline_concurrency_lock::Column::TouchedAt)
        .to_owned(),
    )
    .exec_without_returning(db)
    .await
    .context("db: acquire pipeline concurrency-group lock")?;
    Ok(())
}

/// Count the jobs a runner is executing right now, across every executor.
///
/// The row is the only place both executors agree: the external-runner
/// `start_job` handler and the embedded runner's `start_job_if_active` write
/// the same `running` status, and every path out of it — finish, settle,
/// watchdog reset — writes something else. Counting it is therefore the one
/// answer that holds on an instance with external runners, on one without, and
/// on one running both; the `ci_jobs_running` gauge used to be hand-summed from
/// the external path alone and could not (card_e309fbb5a3fd).
pub async fn count_running_jobs(db: &DatabaseConnection) -> Result<u64> {
    pipeline_job::Entity::find()
        .filter(pipeline_job::Column::Status.eq("running"))
        .count(db)
        .await
        .context("db: count running jobs")
}

/// Count active (pending + running) pipelines for a repository.
pub async fn count_active_pipelines(db: &DatabaseConnection, repo_id: i64) -> Result<usize> {
    let count = pipeline::Entity::find()
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .filter(pipeline::Column::Status.is_in([
            "pending",
            "running",
            "manual",
            "waiting_approval",
        ]))
        .count(db)
        .await
        .context("db: count active pipelines")? as usize;
    Ok(count)
}

/// Find active pipelines belonging to a `concurrency.group`.
///
/// This is the question `concurrency:` actually asks, and it is not the same
/// question as "what else is running on this branch" (card_4c5214698ae9). A
/// fixed group such as `deploy-production` is shared *across* refs and must
/// serialize across them; two workflows that declare different groups on one ref
/// must not touch each other's pipelines.
///
/// A `NULL` group can never match: a workflow that declared no `concurrency:`
/// block is not part of anyone's group, so it is neither waited for nor
/// cancelled. `Column::ConcurrencyGroup.eq(group)` is already `NULL`-safe in SQL
/// — `NULL = 'x'` is unknown, not true — but the guarantee is stated here
/// because it is the half of the fix that prevents *over*-cancelling.
pub async fn find_active_pipelines_by_group(
    db: &impl ConnectionTrait,
    repo_id: i64,
    group: &str,
) -> Result<Vec<pipeline::Model>> {
    pipeline::Entity::find()
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .filter(pipeline::Column::ConcurrencyGroup.eq(group))
        .filter(pipeline::Column::Status.is_in([
            "pending",
            "running",
            "manual",
            "waiting_approval",
        ]))
        .order_by_asc(pipeline::Column::Id)
        .all(db)
        .await
        .context("db: find active pipelines by concurrency group")
}

/// Find active pipelines on a specific git ref (branch/tag).
pub async fn find_active_pipelines_by_ref(
    db: &DatabaseConnection,
    repo_id: i64,
    ref_name: &str,
) -> Result<Vec<pipeline::Model>> {
    pipeline::Entity::find()
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .filter(pipeline::Column::RefName.eq(ref_name))
        .filter(pipeline::Column::Status.is_in([
            "pending",
            "running",
            "manual",
            "waiting_approval",
        ]))
        .order_by_asc(pipeline::Column::Id)
        .all(db)
        .await
        .context("db: find active pipelines by ref")
}

/// Cancel a pipeline and its active stages/jobs atomically.
///
/// A cancellation acknowledgement is a claim about the entire execution graph,
/// not just its root row. Keep every write in one transaction so a failed child
/// read or update cannot leave an active job beneath a canceled pipeline.
/// Returns whether the pipeline was actually transitioned to "canceled".
///
/// Retried through [`crate::busy_retry`], because this transaction reads the
/// graph before it writes it and every caller is a *compensating* one: the
/// state change that made the run pointless — a PR leaving `open`, a queue
/// attempt losing its ownership race — is already committed by the time the
/// cancel runs, so the callers treat a failure here as best-effort and log it
/// rather than unwind. That is the right call for a cancel that genuinely
/// cannot happen, and the wrong one for a transaction refused because another
/// connection committed a byte while it held a read snapshot: without a retry,
/// ordinary write contention leaves a live pipeline handing real jobs to real
/// runners for a question nobody will read the answer to, and nothing in the
/// system comes back for it (card_dce331265c10).
pub async fn cancel_pipeline_chain(db: &DatabaseConnection, pipeline_id: i64) -> Result<bool> {
    crate::contention::retry_transaction("cancel a pipeline graph", || async {
        let tx = db
            .begin()
            .await
            .context("db: begin pipeline cancellation transaction")?;

        match cancel_pipeline_chain_in_transaction(&tx, pipeline_id).await {
            Ok(canceled) => {
                tx.commit()
                    .await
                    .context("db: commit pipeline cancellation transaction")?;
                Ok(canceled)
            }
            Err(error) => {
                if let Err(rollback_error) = tx.rollback().await {
                    tracing::error!(
                        pipeline_id,
                        error = %format!("{rollback_error:#}"),
                        "pipeline cancellation failed and its transaction could not be rolled back"
                    );
                }
                Err(error)
            }
        }
    })
    .await
}

/// Cancel a pipeline graph through a transaction already owned by the caller.
///
/// This is public for `rg-ci`'s concurrency-group publication transaction: the
/// old graph must be canceled under the same group lock and commit boundary as
/// its replacement. Callers that do not already own a transaction should use
/// [`cancel_pipeline_chain`].
pub async fn cancel_pipeline_chain_in_transaction(
    db: &impl ConnectionTrait,
    pipeline_id: i64,
) -> Result<bool> {
    let pipeline_model = match get_pipeline(db, pipeline_id).await? {
        Some(p) => p,
        None => return Ok(false),
    };

    if !is_active_pipeline_work(&pipeline_model.status) {
        return Ok(false);
    }

    let now = Some(chrono::Utc::now().naive_utc());

    update_pipeline_status(db, pipeline_id, "canceled", None, now).await?;

    let stages = list_stages_by_pipeline(db, pipeline_id).await?;
    for stage in &stages {
        if is_active_pipeline_work(&stage.status) {
            update_stage_status(db, stage.id, "canceled", None, now).await?;
        }

        let jobs = list_jobs_by_stage(db, stage.id).await?;
        for job in &jobs {
            if is_active_pipeline_work(&job.status) {
                update_job_result(db, job.id, "canceled", None, None, None, now).await?;
            }
        }
    }

    Ok(true)
}

/// The statuses a pipeline, stage or job can still leave under its own power.
///
/// Everything else — `success`, `failed`/`failure`/`error`, `skipped`,
/// `canceled` — is terminal. `assigned` belongs here: a job handed to a runner
/// but not yet started is work in flight, and a cancellation that walked past
/// it left a runner about to report a result for a pipeline the server had
/// already answered `canceled` for.
pub const ACTIVE_WORK_STATUSES: [&str; 5] = [
    "pending",
    "assigned",
    "running",
    "manual",
    "waiting_approval",
];

pub fn is_active_pipeline_work(status: &str) -> bool {
    ACTIVE_WORK_STATUSES.contains(&status)
}

/// Whether a row has finished — the definition the roll-up asks for.
///
/// The complement of [`is_active_pipeline_work`], and deliberately *derived*
/// from it rather than spelled out a second time. Four roll-up sites used to
/// carry their own literal list of terminal statuses, and they had already
/// drifted: `check_stage_jobs` omitted `canceled` while its three neighbours
/// included it, so the same status was "finished" to one reader of a row and
/// "still running" to another (card_39bf6a755499). A status added to the column
/// now has exactly one place to be classified.
pub fn is_finished_pipeline_work(status: &str) -> bool {
    !is_active_pipeline_work(status)
}

/// Whether a finished row finished *without* succeeding.
///
/// `canceled` counts. It is not a failure in the sense of "the code is broken",
/// but the roll-up asks a narrower question — may this stage/pipeline be called
/// a success? — and the answer for work somebody stopped is no. Rolling a
/// canceled job's stage up green would release exactly what the cancellation
/// was meant to prevent: the success hooks and auto-merge.
fn is_unsuccessful_outcome(status: &str) -> bool {
    matches!(status, "failure" | "failed" | "error" | "canceled")
}

/// Whether the pipeline still owns its own execution.
///
/// A worker that started before a cancellation has no other way to notice it:
/// its stage/job snapshot predates the cascade.
pub async fn pipeline_is_active(db: &impl ConnectionTrait, pipeline_id: i64) -> Result<bool> {
    Ok(get_pipeline(db, pipeline_id)
        .await?
        .is_some_and(|pipeline| is_active_pipeline_work(&pipeline.status)))
}

// ── Terminal-status guards ───────────────────────────────────────
//
// A cancellation is transactional at the moment it answers, but it does not
// stop a worker that already holds the job. The worker comes back with
// `success`/`failed` from a snapshot taken before the cascade, and an
// unconditional write walks the whole chain back out of `canceled` — a false
// acknowledgement that also releases success hooks and auto-merge.
//
// So the terminal write is a condition, not a command: it lands only while the
// row is still active work, or when it would rewrite the status the row
// already carries (which is what makes a runner's `finish` retry idempotent
// rather than a conflict). One statement each — a read-then-write leaves
// exactly the window the cancellation lands in.

fn still_settleable<C: ColumnTrait>(status_column: C, status: &str) -> Condition {
    Condition::any()
        .add(status_column.is_in(ACTIVE_WORK_STATUSES))
        .add(status_column.eq(status))
}

/// Record a job's result unless the job has already settled as something else.
///
/// Returns `false` when the row was left untouched — the caller is reporting on
/// work the server has disowned and must not cascade it further.
pub async fn settle_job_if_active(
    db: &impl ConnectionTrait,
    id: i64,
    status: &str,
    exit_code: Option<i32>,
    log: Option<&str>,
    finished_at: Option<chrono::NaiveDateTime>,
) -> Result<bool> {
    let now = chrono::Utc::now().naive_utc();
    let mut update = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(id))
        .filter(still_settleable(pipeline_job::Column::Status, status))
        .col_expr(pipeline_job::Column::Status, Expr::value(status))
        .col_expr(pipeline_job::Column::ExitCode, Expr::value(exit_code))
        .col_expr(pipeline_job::Column::UpdatedAt, Expr::value(now));
    if let Some(log) = log {
        update = update.col_expr(pipeline_job::Column::Log, Expr::value(log));
    }
    if let Some(finished_at) = finished_at {
        update = update.col_expr(pipeline_job::Column::FinishedAt, Expr::value(finished_at));
    }
    Ok(update
        .exec(db)
        .await
        .context("db: settle job result")?
        .rows_affected
        > 0)
}

/// Move a job to `running` and stamp its start, but only while it is still
/// active work.
///
/// Returns `false` when the job settled between assignment and start — telling
/// the runner to go ahead would put it to work on a pipeline the server has
/// already answered `canceled` for.
pub async fn start_job_if_active(
    db: &impl ConnectionTrait,
    id: i64,
    started_at: Option<chrono::NaiveDateTime>,
) -> Result<bool> {
    let now = chrono::Utc::now().naive_utc();
    let mut update = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(id))
        .filter(still_settleable(pipeline_job::Column::Status, "running"))
        .col_expr(pipeline_job::Column::Status, Expr::value("running"))
        .col_expr(pipeline_job::Column::UpdatedAt, Expr::value(now));
    if let Some(started_at) = started_at {
        update = update.col_expr(pipeline_job::Column::StartedAt, Expr::value(started_at));
    }
    Ok(update
        .exec(db)
        .await
        .context("db: start job")?
        .rows_affected
        > 0)
}

/// Stage-level twin of [`settle_job_if_active`].
pub async fn settle_stage_if_active(
    db: &impl ConnectionTrait,
    id: i64,
    status: &str,
    started_at: Option<chrono::NaiveDateTime>,
    finished_at: Option<chrono::NaiveDateTime>,
) -> Result<bool> {
    let mut update = pipeline_stage::Entity::update_many()
        .filter(pipeline_stage::Column::Id.eq(id))
        .filter(still_settleable(pipeline_stage::Column::Status, status))
        .col_expr(pipeline_stage::Column::Status, Expr::value(status));
    if let Some(started_at) = started_at {
        update = update.col_expr(pipeline_stage::Column::StartedAt, Expr::value(started_at));
    }
    if let Some(finished_at) = finished_at {
        update = update.col_expr(pipeline_stage::Column::FinishedAt, Expr::value(finished_at));
    }
    Ok(update
        .exec(db)
        .await
        .context("db: settle stage status")?
        .rows_affected
        > 0)
}

/// Pipeline-level twin of [`settle_job_if_active`].
pub async fn settle_pipeline_if_active(
    db: &impl ConnectionTrait,
    id: i64,
    status: &str,
    started_at: Option<chrono::NaiveDateTime>,
    finished_at: Option<chrono::NaiveDateTime>,
) -> Result<bool> {
    let mut update = pipeline::Entity::update_many()
        .filter(pipeline::Column::Id.eq(id))
        .filter(still_settleable(pipeline::Column::Status, status))
        .col_expr(pipeline::Column::Status, Expr::value(status));
    if let Some(started_at) = started_at {
        update = update.col_expr(pipeline::Column::StartedAt, Expr::value(started_at));
    }
    if let Some(finished_at) = finished_at {
        update = update.col_expr(pipeline::Column::FinishedAt, Expr::value(finished_at));
    }
    Ok(update
        .exec(db)
        .await
        .context("db: settle pipeline status")?
        .rows_affected
        > 0)
}

/// Resolve concurrency group template variables.
///
/// Supports `${{ ref }}` / `${{ branch }}` — the `.forgekeep-ci.yml` spelling —
/// plus the two Actions names that mean exactly the same thing. A workflow in
/// `.gitea/workflows/` has already had its full `${{ github.* }}` context
/// expanded by `gitea_actions::expand_concurrency_group`, so this pass is a
/// no-op for it; the aliases are here for the author of a native config who
/// writes the spelling they know from GitHub. Getting a group *wrong* is not a
/// cosmetic miss — an unexpanded template is a literal that every ref of the
/// repository shares — so `trigger_pipeline` refuses a group that still carries
/// an expression after this pass rather than serializing on it.
pub fn resolve_concurrency_group(template: &str, ref_name: &str) -> String {
    let branch = ref_name
        .strip_prefix("refs/heads/")
        .or_else(|| ref_name.strip_prefix("refs/tags/"))
        .unwrap_or(ref_name);
    template
        .replace("${{ ref }}", ref_name)
        .replace("${{ branch }}", branch)
        .replace("${{ github.ref }}", ref_name)
        .replace("${{ github.ref_name }}", branch)
}

/// One column, several readers — and for a while they disagreed about what
/// "finished" meant (card_39bf6a755499). These pin the definition rather than
/// any one caller's use of it.
#[cfg(test)]
mod terminal_status_tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database, Statement};

    async fn setup() -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect to in-memory db");
        crate::run_migrations(&db).await.expect("run migrations");
        for statement in [
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'ci', 'ci@test.com', 'x', 0, 1, '2024-01-01', '2024-01-01')",
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) \
             VALUES(1, 1, 'ci', 0, 'main', 0, 0, '2024-01-01', '2024-01-01')",
        ] {
            db.execute(Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                statement,
            ))
            .await
            .expect("seed row");
        }
        db
    }

    async fn stage_with_job(db: &DatabaseConnection, job_status: &str) -> (i64, i64) {
        let pipeline = create_pipeline(db, 1, "deadbeef", "refs/heads/main", "push", None)
            .await
            .expect("create pipeline");
        let stage = create_stage(db, pipeline.id, "test", 0)
            .await
            .expect("create stage");
        let job = create_job(
            db, stage.id, "test", "echo ok", None, None, None, None, None, None, false, None, None,
            None,
        )
        .await
        .expect("create job");
        update_job_result(db, job.id, job_status, None, None, None, None)
            .await
            .expect("settle job");
        (pipeline.id, stage.id)
    }

    /// `canceled` was terminal to three roll-up readers and unknown to the
    /// fourth, so a stage holding one waited for a job that was never coming
    /// back — and the pipeline, and every required check on the PR behind it.
    #[tokio::test]
    async fn a_stage_holding_a_canceled_job_is_finished_not_still_running() {
        let db = setup().await;
        let (_, stage_id) = stage_with_job(&db, "canceled").await;

        let (all_done, any_failure) = check_stage_jobs(&db, stage_id)
            .await
            .expect("classify stage jobs");

        assert!(all_done, "a canceled job is not work still in flight");
        assert!(
            any_failure,
            "work somebody stopped is not a success: rolling the stage up green \
             would release the auto-merge the cancellation existed to prevent"
        );
    }

    /// The verdict the roll-up actually writes, not just its inputs.
    #[tokio::test]
    async fn a_canceled_job_closes_its_stage_without_calling_it_a_success() {
        let db = setup().await;
        let (_, stage_id) = stage_with_job(&db, "canceled").await;

        let rolled_up = try_update_stage(&db, stage_id)
            .await
            .expect("roll the stage up");

        assert_eq!(rolled_up.as_deref(), Some("failed"));
    }

    /// The complement is exactly the complement — a status the active list does
    /// not name is finished, and nothing is both.
    #[test]
    fn finished_is_the_complement_of_active() {
        for status in ACTIVE_WORK_STATUSES {
            assert!(is_active_pipeline_work(status));
            assert!(!is_finished_pipeline_work(status));
        }
        for status in [
            "success", "failure", "failed", "error", "skipped", "canceled",
        ] {
            assert!(is_finished_pipeline_work(status));
        }
    }
}
