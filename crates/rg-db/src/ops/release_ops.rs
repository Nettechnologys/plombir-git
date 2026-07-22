//! Database operations for releases and release assets.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::release::{
    self, ActiveModel as ReleaseActiveModel, Entity as ReleaseEntity, Model as ReleaseModel,
};
use crate::entities::release_asset::{
    self, ActiveModel as AssetActiveModel, Entity as AssetEntity, Model as AssetModel,
};

/// Find a release by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<ReleaseModel>> {
    ReleaseEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find release by id")
}

/// Find a release by repo_id and tag_name.
pub async fn find_by_repo_and_tag(
    db: &DatabaseConnection,
    repo_id: i64,
    tag_name: &str,
) -> Result<Option<ReleaseModel>> {
    ReleaseEntity::find()
        .filter(release::Column::RepoId.eq(repo_id))
        .filter(release::Column::TagName.eq(tag_name))
        .one(db)
        .await
        .context("db: find release by repo and tag")
}

/// List releases for a repo with pagination.
pub async fn list_by_repo(
    db: &DatabaseConnection,
    repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<ReleaseModel>, i64)> {
    let base = ReleaseEntity::find()
        .filter(release::Column::RepoId.eq(repo_id))
        .order_by_desc(release::Column::CreatedAt);

    let total = base.clone().count(db).await.context("db: count releases")? as i64;

    let releases = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list releases")?;

    Ok((releases, total))
}

/// Create a new release.
pub async fn create(db: &DatabaseConnection, model: ReleaseActiveModel) -> Result<ReleaseModel> {
    model.insert(db).await.context("db: create release")
}

/// Update a release.
pub async fn update(db: &DatabaseConnection, model: ReleaseActiveModel) -> Result<ReleaseModel> {
    model.update(db).await.context("db: update release")
}

/// Delete a release by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    ReleaseEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete release")?;
    Ok(())
}

/// Create a release asset.
pub async fn create_asset(db: &DatabaseConnection, model: AssetActiveModel) -> Result<AssetModel> {
    model.insert(db).await.context("db: create asset")
}

/// List assets for a release.
pub async fn list_assets(db: &DatabaseConnection, release_id: i64) -> Result<Vec<AssetModel>> {
    AssetEntity::find()
        .filter(release_asset::Column::ReleaseId.eq(release_id))
        .order_by_asc(release_asset::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list assets")
}

/// Find an asset by ID.
pub async fn find_asset_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<AssetModel>> {
    AssetEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find asset by id")
}

/// Delete an asset by ID.
pub async fn delete_asset_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    AssetEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete asset")?;
    Ok(())
}

/// Increment download count for an asset.
pub async fn increment_download_count(db: &DatabaseConnection, id: i64) -> Result<()> {
    let asset = AssetEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find asset for increment")?
        .ok_or_else(|| anyhow::anyhow!("asset not found"))?;

    let new_count = asset.download_count + 1;
    let mut model: AssetActiveModel = asset.into();
    model.download_count = Set(new_count);
    model
        .update(db)
        .await
        .context("db: update download count")?;

    Ok(())
}
