//! Database operations for repository mirrors.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::*;

use crate::entities::mirror::{self, ActiveModel, Entity as MirrorEntity, Model};

/// Create a mirror record.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.insert(db).await.context("db: create mirror")
}

/// Find a mirror by its ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>> {
    MirrorEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find mirror by id")
}

/// Find a mirror by repository ID.
pub async fn find_by_repo_id(db: &DatabaseConnection, repo_id: i64) -> Result<Option<Model>> {
    MirrorEntity::find()
        .filter(mirror::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: find mirror by repo_id")
}

/// Update a mirror record.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.update(db).await.context("db: update mirror")
}

/// Delete a mirror by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    MirrorEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete mirror")?;
    Ok(())
}

/// List mirrors that are due for sync.
pub async fn list_due_sync(db: &DatabaseConnection, limit: u64) -> Result<Vec<Model>> {
    let now = Utc::now();
    MirrorEntity::find()
        .filter(mirror::Column::Status.eq("active"))
        .filter(
            mirror::Column::NextSyncAt
                .is_null()
                .or(mirror::Column::NextSyncAt.lte(now)),
        )
        .limit(limit)
        .all(db)
        .await
        .context("db: list due sync mirrors")
}

/// List all mirrors (admin).
pub async fn list_all(db: &DatabaseConnection) -> Result<Vec<Model>> {
    MirrorEntity::find()
        .all(db)
        .await
        .context("db: list all mirrors")
}
