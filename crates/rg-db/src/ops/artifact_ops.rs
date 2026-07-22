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
    };

    active_model.insert(db).await.context("db: create artifact")
}

/// List artifacts by job ID.
pub async fn list_by_job(db: &DatabaseConnection, job_id: i64) -> Result<Vec<Artifact>> {
    ArtifactEntity::find()
        .filter(Column::JobId.eq(job_id))
        .filter(
            Condition::any()
                .add(Column::ExpiresAt.is_null())
                .add(Column::ExpiresAt.gt(Utc::now())),
        )
        .order_by_desc(Column::CreatedAt)
        .all(db)
        .await
        .context("db: list artifacts by job")
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
