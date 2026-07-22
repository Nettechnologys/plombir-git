//! Database operations for import tasks.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::*;

use crate::entities::import_task::{self, ActiveModel, Entity as ImportTaskEntity, Model};

/// Create a new import task.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.insert(db).await.context("db: create import task")
}

/// Find an import task by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>> {
    ImportTaskEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find import task by id")
}

/// Find the latest import task for a user.
pub async fn find_by_user(db: &DatabaseConnection, user_id: i64, limit: u64) -> Result<Vec<Model>> {
    ImportTaskEntity::find()
        .filter(import_task::Column::UserId.eq(user_id))
        .order_by_desc(import_task::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .context("db: find import tasks by user")
}

/// Find an import task by target owner/name (latest first).
pub async fn find_by_target(
    db: &DatabaseConnection,
    owner: &str,
    name: &str,
) -> Result<Option<Model>> {
    ImportTaskEntity::find()
        .filter(import_task::Column::TargetOwner.eq(owner))
        .filter(import_task::Column::TargetName.eq(name))
        .order_by_desc(import_task::Column::CreatedAt)
        .one(db)
        .await
        .context("db: find import task by target")
}

/// Find import tasks by repo_id.
pub async fn find_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Option<Model>> {
    ImportTaskEntity::find()
        .filter(import_task::Column::RepoId.eq(repo_id))
        .order_by_desc(import_task::Column::CreatedAt)
        .one(db)
        .await
        .context("db: find import task by repo")
}

/// Update an import task.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.update(db).await.context("db: update import task")
}

/// Update status and progress atomically.
pub async fn update_progress(
    db: &DatabaseConnection,
    id: i64,
    status: &str,
    progress: i32,
    stage: Option<&str>,
) -> Result<Model> {
    let task = find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found: {id}"))?;

    let mut model: ActiveModel = task.into();
    model.status = Set(status.to_string());
    model.progress = Set(progress);
    model.stage = Set(stage.map(|s| s.to_string()));
    model.updated_at = Set(Utc::now());

    update(db, model).await
}

/// Mark import as failed with error message.
pub async fn mark_failed(db: &DatabaseConnection, id: i64, error: &str) -> Result<Model> {
    let task = find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found: {id}"))?;

    let mut model: ActiveModel = task.into();
    model.status = Set("failed".to_string());
    model.error = Set(Some(error.to_string()));
    model.updated_at = Set(Utc::now());

    update(db, model).await
}

/// Mark import as completed with final stats.
pub async fn mark_completed(db: &DatabaseConnection, id: i64, stats_json: &str) -> Result<Model> {
    let task = find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found: {id}"))?;

    let mut model: ActiveModel = task.into();
    model.status = Set("completed".to_string());
    model.progress = Set(100);
    model.stage = Set(Some("Import completed".to_string()));
    model.stats = Set(Some(stats_json.to_string()));
    model.updated_at = Set(Utc::now());

    update(db, model).await
}

/// Set repo_id after repository is created.
pub async fn set_repo_id(db: &DatabaseConnection, id: i64, repo_id: i64) -> Result<Model> {
    let task = find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found: {id}"))?;

    let mut model: ActiveModel = task.into();
    model.repo_id = Set(Some(repo_id));
    model.updated_at = Set(Utc::now());

    update(db, model).await
}

/// Delete an import task by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    ImportTaskEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete import task")?;
    Ok(())
}

/// List all active (non-completed, non-failed) import tasks.
pub async fn list_active(db: &DatabaseConnection, limit: u64) -> Result<Vec<Model>> {
    ImportTaskEntity::find()
        .filter(import_task::Column::Status.is_in(["pending", "cloning", "importing"]))
        .order_by_asc(import_task::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .context("db: list active import tasks")
}

/// List all import tasks (admin use).
pub async fn list_all(db: &DatabaseConnection, limit: u64) -> Result<Vec<Model>> {
    ImportTaskEntity::find()
        .order_by_desc(import_task::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .context("db: list all import tasks")
}
