//! Database operations for LFS objects.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::lfs_object::{self, ActiveModel, Entity as LfsEntity, Model as LfsObject};

/// Find an LFS object by (repo_id, oid).
pub async fn find_by_repo_and_oid(
    db: &DatabaseConnection,
    repo_id: i64,
    oid: &str,
) -> Result<Option<LfsObject>> {
    LfsEntity::find()
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::Oid.eq(oid))
        .one(db)
        .await
        .context("db: find LFS object by repo and oid")
}

/// List LFS objects for a repo.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<LfsObject>> {
    LfsEntity::find()
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .order_by_desc(lfs_object::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list LFS objects by repo")
}

/// Create a new LFS object record.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<LfsObject> {
    model.insert(db).await.context("db: create LFS object")
}

/// Mark an LFS object as uploaded.
pub async fn mark_uploaded(db: &DatabaseConnection, id: i64) -> Result<()> {
    let obj = LfsEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find LFS object for mark_uploaded")?
        .ok_or_else(|| anyhow::anyhow!("LFS object {} not found", id))?;

    let mut model: ActiveModel = obj.into();
    model.uploaded = sea_orm::Set(true);
    model
        .update(db)
        .await
        .context("db: mark LFS object as uploaded")?;
    Ok(())
}

/// Delete an LFS object by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    LfsEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete LFS object")?;
    Ok(())
}

/// Update compression info for an LFS object.
pub async fn update_compression(
    db: &DatabaseConnection,
    id: i64,
    compression: &str,
    compressed_size: i64,
) -> Result<()> {
    let obj = LfsEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find LFS object for update_compression")?
        .ok_or_else(|| anyhow::anyhow!("LFS object {} not found", id))?;

    let mut model: ActiveModel = obj.into();
    model.compression = sea_orm::Set(Some(compression.to_string()));
    model.compressed_size = sea_orm::Set(Some(compressed_size));
    model
        .update(db)
        .await
        .context("db: update LFS object compression")?;
    Ok(())
}

/// List uncompressed LFS objects (for lazy compression).
pub async fn list_uncompressed(
    db: &DatabaseConnection,
    repo_id: i64,
    limit: u64,
) -> Result<Vec<LfsObject>> {
    LfsEntity::find()
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::Compression.is_null())
        .filter(lfs_object::Column::Uploaded.eq(true))
        .limit(limit)
        .all(db)
        .await
        .context("db: list uncompressed LFS objects")
}
