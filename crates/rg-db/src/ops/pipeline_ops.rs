//! Database operations for CI/CD pipelines.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::{pipeline, pipeline_job, pipeline_stage};

// ── Pipeline ops ─────────────────────────────────────────────────

/// Create a new pipeline record.
pub async fn create_pipeline(
    db: &DatabaseConnection,
    repo_id: i64,
    commit_sha: &str,
    ref_name: &str,
    trigger_type: &str,
    triggered_by: Option<i64>,
) -> Result<pipeline::Model> {
    let now = chrono::Utc::now().naive_utc();
    let model = pipeline::ActiveModel {
        repo_id: Set(repo_id),
        commit_sha: Set(commit_sha.to_string()),
        ref_name: Set(ref_name.to_string()),
        status: Set("pending".to_string()),
        trigger_type: Set(trigger_type.to_string()),
        triggered_by: Set(triggered_by),
        started_at: Set(None),
        finished_at: Set(None),
        created_at: Set(now),
        ..Default::default()
    };
    let result = model.insert(db).await.context("db: create pipeline")?;
    Ok(result)
}

/// Get a pipeline by ID.
pub async fn get_pipeline(db: &DatabaseConnection, id: i64) -> Result<Option<pipeline::Model>> {
    pipeline::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: get pipeline")
}

/// List pipelines for a repo.
pub async fn list_pipelines_by_repo(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Vec<pipeline::Model>> {
    pipeline::Entity::find()
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .order_by_desc(pipeline::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list pipelines by repo")
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
        .order_by_desc(pipeline::Column::CreatedAt);

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

/// Update pipeline status.
pub async fn update_pipeline_status(
    db: &DatabaseConnection,
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
pub async fn create_stage(
    db: &DatabaseConnection,
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
    db: &DatabaseConnection,
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
    db: &DatabaseConnection,
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

/// Create a pipeline job.
pub async fn create_job(
    db: &DatabaseConnection,
    stage_id: i64,
    name: &str,
    script: &str,
    image: Option<&str>,
    tags: Option<&str>,
    variables: Option<&str>,
    cache_key: Option<&str>,
    cache_paths: Option<&str>,
    allow_failure: bool,
    timeout_seconds: Option<i64>,
    when_condition: Option<&str>,
    if_condition: Option<&str>,
) -> Result<pipeline_job::Model> {
    let when_condition = when_condition.unwrap_or("on_success");
    let model = pipeline_job::ActiveModel {
        stage_id: Set(stage_id),
        name: Set(name.to_string()),
        script: Set(script.to_string()),
        variables: Set(variables.map(str::to_string)),
        cache_key: Set(cache_key.map(str::to_string)),
        cache_paths: Set(cache_paths.map(str::to_string)),
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

/// Update just the log field of a job (lightweight, used by log write queue).
pub async fn update_job_log(db: &DatabaseConnection, id: i64, log: &str) -> Result<()> {
    use sea_orm::ActiveModelTrait;
    let model = pipeline_job::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find job for log update")?
        .ok_or_else(|| anyhow::anyhow!("job {} not found", id))?;

    let mut active: pipeline_job::ActiveModel = model.into();
    active.log = Set(Some(log.to_string()));
    active.update(db).await.context("db: update job log")?;
    Ok(())
}

/// List jobs for a stage.
pub async fn list_jobs_by_stage(
    db: &DatabaseConnection,
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
pub async fn update_job_result(
    db: &DatabaseConnection,
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
    let all_done = jobs.iter().all(|j| {
        matches!(
            j.status.as_str(),
            "success" | "failure" | "failed" | "error" | "skipped"
        )
    });
    let any_failure = jobs.iter().any(|j| {
        (j.status == "failure" || j.status == "error" || j.status == "failed") && !j.allow_failure
    });
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
        .all(|job| {
            matches!(
                job.status.as_str(),
                "success" | "failure" | "failed" | "error" | "skipped" | "canceled"
            )
        });
    let Some(gate_status) = gate_status else {
        return Ok(false);
    };
    if !automatic_done {
        return Ok(false);
    }
    let stage = get_stage_by_id(db, stage_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("stage {} not found", stage_id))?;
    update_stage_status(db, stage_id, gate_status, None, None).await?;
    update_pipeline_status(db, stage.pipeline_id, gate_status, None, None).await?;
    Ok(true)
}

/// After a job finishes, update stage status if all jobs in the stage are done.
/// Returns the new stage status if updated, or None if not all done.
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
    update_stage_status(db, stage_id, new_status, None, now).await?;
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
            if !matches!(
                job.status.as_str(),
                "success" | "failure" | "failed" | "error" | "skipped" | "canceled"
            ) {
                update_job_result(db, job.id, "skipped", None, None, None, now).await?;
            }
        }
        if !matches!(
            stage.status.as_str(),
            "success" | "failure" | "failed" | "error" | "skipped" | "canceled"
        ) {
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
    let all_done = stages.iter().all(|stage| {
        matches!(
            stage.status.as_str(),
            "success" | "failure" | "failed" | "error" | "skipped" | "canceled"
        )
    });
    let any_failure = stages
        .iter()
        .any(|stage| matches!(stage.status.as_str(), "failure" | "failed" | "error"));
    Ok((all_done, any_failure))
}

/// After a stage finishes, update pipeline status if all stages are done.
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
    update_pipeline_status(db, pipeline_id, new_status, None, now).await?;
    Ok(Some(new_status.to_string()))
}

/// Find a pending job (status = "pending" and runner_id is NULL).
/// Returns the oldest pending job (by id).
pub async fn find_pending_job(db: &DatabaseConnection) -> Result<Option<pipeline_job::Model>> {
    let jobs = pipeline_job::Entity::find()
        .filter(pipeline_job::Column::Status.eq("pending"))
        .filter(pipeline_job::Column::RunnerId.is_null())
        .order_by_asc(pipeline_job::Column::Id)
        .all(db)
        .await
        .context("db: find pending job")?;
    for job in jobs {
        if job_is_schedulable(db, &job).await? {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

/// Find a pending job that matches the given runner labels.
///
/// A job matches if:
/// - It has no tags (any runner can pick it up)
/// - OR at least one of its tags matches one of the runner's labels
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

    let labels_lower: Vec<String> = runner_labels.iter().map(|l| l.to_lowercase()).collect();

    for job in all_pending {
        if !job_is_schedulable(db, &job).await? {
            continue;
        }
        if runner_labels.is_empty() {
            return Ok(Some(job));
        }
        let job_tags: Vec<String> = job
            .tags
            .as_ref()
            .and_then(|t| serde_json::from_str(t).ok())
            .unwrap_or_default();

        if job_tags.is_empty() {
            return Ok(Some(job));
        }

        if job_tags
            .iter()
            .any(|t| labels_lower.contains(&t.to_lowercase()))
        {
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

/// Mark a job as timed out (error status).
pub async fn mark_job_timeout(db: &DatabaseConnection, job_id: i64) -> Result<()> {
    let now = chrono::Utc::now().naive_utc();
    pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .col_expr(pipeline_job::Column::Status, Expr::value("error"))
        .col_expr(pipeline_job::Column::ExitCode, Expr::value(-1))
        .col_expr(pipeline_job::Column::FinishedAt, Expr::value(now))
        .col_expr(pipeline_job::Column::UpdatedAt, Expr::value(now))
        .exec(db)
        .await
        .context("db: mark job timeout")?;
    Ok(())
}

/// Reset all jobs assigned to a runner back to pending (for deregistration).
pub async fn reset_runner_jobs(db: &DatabaseConnection, runner_id: i64) -> Result<u64> {
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

/// Assign a CI job to a specific runner.
pub async fn assign_job(db: &DatabaseConnection, job_id: i64, runner_id: i64) -> Result<()> {
    let now = chrono::Utc::now().naive_utc();
    let model = pipeline_job::Entity::find_by_id(job_id)
        .one(db)
        .await
        .context("db: find job for assign")?
        .ok_or_else(|| anyhow::anyhow!("job {} not found", job_id))?;

    let mut active: pipeline_job::ActiveModel = model.into();
    active.status = Set("assigned".to_string());
    active.runner_id = Set(Some(runner_id));
    active.updated_at = Set(Some(now));
    active.update(db).await.context("db: assign job")?;
    Ok(())
}

// ── Concurrency Control ──────────────────────────────────────────

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

/// Find active pipelines on a specific git ref (branch/tag).
/// Used for concurrency control by ref name.
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

/// Cancel a pipeline and all its stages/jobs that are still pending or running.
/// Returns whether the pipeline was actually transitioned to "canceled".
pub async fn cancel_pipeline_chain(db: &DatabaseConnection, pipeline_id: i64) -> Result<bool> {
    let pipeline_model = match get_pipeline(db, pipeline_id).await? {
        Some(p) => p,
        None => return Ok(false),
    };

    // Only cancel if still pending or running
    if pipeline_model.status != "pending"
        && pipeline_model.status != "running"
        && pipeline_model.status != "manual"
        && pipeline_model.status != "waiting_approval"
    {
        return Ok(false);
    }

    let now = Some(chrono::Utc::now().naive_utc());

    // Cancel the pipeline
    update_pipeline_status(db, pipeline_id, "canceled", None, now).await?;

    // Cancel all stages that are not yet finished
    let stages = list_stages_by_pipeline(db, pipeline_id).await?;
    for stage in &stages {
        if stage.status != "success" && stage.status != "failed" && stage.status != "skipped" {
            update_stage_status(db, stage.id, "canceled", None, now).await?;
        }

        // Cancel all jobs in this stage
        let jobs = list_jobs_by_stage(db, stage.id).await?;
        for job in &jobs {
            if job.status != "success" && job.status != "failed" && job.status != "skipped" {
                update_job_result(db, job.id, "canceled", None, None, None, now).await?;
            }
        }
    }

    Ok(true)
}

/// Resolve concurrency group template variables.
/// Supports: ${{ ref }}, ${{ branch }}
pub fn resolve_concurrency_group(template: &str, ref_name: &str) -> String {
    let branch = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
    template
        .replace("${{ ref }}", ref_name)
        .replace("${{ branch }}", branch)
}
