//! Database operations for CI artifacts.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::*;

use crate::entities::artifact::{ActiveModel, Column, Entity as ArtifactEntity, Model as Artifact};

/// Create a new artifact record.
pub async fn create_artifact(
    db: &DatabaseConnection,
    job_id: i64,
    name: &str,
    file_path: &str,
    size: i64,
    sha256: Option<String>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Artifact> {
    let now = Utc::now();
    let active_model = ActiveModel {
        id: NotSet,
        job_id: Set(job_id),
        name: Set(name.to_string()),
        file_path: Set(file_path.to_string()),
        size: Set(size),
        created_at: Set(now),
        expires_at: Set(expires_at),
        sha256: Set(sha256),
    };

    active_model.insert(db).await.context("db: create artifact")
}

/// Check whether durable metadata owns an exact artifact blob key.
///
/// Startup recovery is the only caller. Artifact publication writes a
/// request-private UUID key before inserting its row, so recovery must prove
/// that exact row absent before it may delete an interrupted publication.
pub async fn exists_by_file_path(db: &DatabaseConnection, file_path: &str) -> Result<bool> {
    Ok(ArtifactEntity::find()
        .select_only()
        .column(Column::Id)
        .filter(Column::FilePath.eq(file_path))
        .into_tuple::<i64>()
        .one(db)
        .await
        .context("db: find artifact by file path")?
        .is_some())
}

/// List artifacts by pipeline ID.
/// Fetches all jobs belonging to the pipeline's stages, then queries artifacts.
pub async fn list_by_pipeline(db: &DatabaseConnection, pipeline_id: i64) -> Result<Vec<Artifact>> {
    let stages = crate::ops::pipeline_ops::list_stages_by_pipeline(db, pipeline_id).await?;
    let mut job_ids = Vec::new();
    for stage in &stages {
        let jobs = crate::ops::pipeline_ops::list_jobs_by_stage(db, stage.id).await?;
        for job in &jobs {
            job_ids.push(job.id);
        }
    }

    if job_ids.is_empty() {
        return Ok(vec![]);
    }

    ArtifactEntity::find()
        .filter(Column::JobId.is_in(job_ids))
        .filter(
            Condition::any()
                .add(Column::ExpiresAt.is_null())
                .add(Column::ExpiresAt.gt(Utc::now())),
        )
        .order_by_desc(Column::CreatedAt)
        .all(db)
        .await
        .context("db: list artifacts by pipeline")
}

/// How many artifacts `repo_id`'s pipelines published, and how many bytes they
/// declare.
///
/// An artifact names its repository only through its job, stage and pipeline,
/// so the filter walks that chain as nested subqueries — one statement, no
/// per-pipeline round trips. Expired rows count: their archive occupies the
/// volume until the retention sweep retires it.
pub async fn repo_usage(db: &DatabaseConnection, repo_id: i64) -> Result<(u64, i64)> {
    use crate::entities::{pipeline, pipeline_job, pipeline_stage};

    let pipelines = pipeline::Entity::find()
        .select_only()
        .column(pipeline::Column::Id)
        .filter(pipeline::Column::RepoId.eq(repo_id))
        .into_query();
    let stages = pipeline_stage::Entity::find()
        .select_only()
        .column(pipeline_stage::Column::Id)
        .filter(pipeline_stage::Column::PipelineId.in_subquery(pipelines))
        .into_query();
    let jobs = pipeline_job::Entity::find()
        .select_only()
        .column(pipeline_job::Column::Id)
        .filter(pipeline_job::Column::StageId.in_subquery(stages))
        .into_query();
    super::aggregate::count_and_sum(
        db,
        ArtifactEntity::find().filter(Column::JobId.in_subquery(jobs)),
        Column::Id,
        Column::Size,
        "db: sum the CI artifacts of a repository",
    )
    .await
}

/// Get a single artifact by ID.
pub async fn get_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Artifact>> {
    ArtifactEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: get artifact by id")
}

/// Delete an artifact by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = ArtifactEntity::delete_by_id(id).exec(db).await?;
    Ok(result.rows_affected > 0)
}

pub async fn list_expired(db: &DatabaseConnection) -> Result<Vec<Artifact>> {
    ArtifactEntity::find()
        .filter(Column::ExpiresAt.is_not_null())
        .filter(Column::ExpiresAt.lte(Utc::now()))
        .all(db)
        .await
        .context("db: list expired artifacts")
}
