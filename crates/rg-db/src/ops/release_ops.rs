//! Database operations for releases and release assets.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
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
    let base = page_query(repo_id);

    let total = base.clone().count(db).await.context("db: count releases")? as i64;

    let releases = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list releases")?;

    Ok((releases, total))
}

/// The ordered selection [`list_by_repo`] cuts a page from, kept apart so
/// `query_plan_tests` explains the statement the server sends.
pub(crate) fn page_query(repo_id: i64) -> Select<ReleaseEntity> {
    ReleaseEntity::find()
        .filter(release::Column::RepoId.eq(repo_id))
        .order_by_desc(release::Column::CreatedAt)
        .order_by_desc(release::Column::Id)
}

/// Create a new release.
pub async fn create(db: &DatabaseConnection, model: ReleaseActiveModel) -> Result<ReleaseModel> {
    model.insert(db).await.context("db: create release")
}

/// Update a release in one conditional statement.
///
/// The caller's lookup is a separate statement, so a concurrent delete can
/// win before this write. `None` keeps that ordinary absence out of SeaORM's
/// backend-shaped `RecordNotUpdated` error.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    db: &DatabaseConnection,
    id: i64,
    title: Option<String>,
    body: Option<String>,
    is_draft: Option<bool>,
    is_prerelease: Option<bool>,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<ReleaseModel>> {
    let mut update =
        ReleaseEntity::update_many().col_expr(release::Column::UpdatedAt, Expr::value(updated_at));
    if let Some(title) = title {
        update = update.col_expr(release::Column::Title, Expr::value(title));
    }
    if let Some(body) = body {
        update = update.col_expr(release::Column::Body, Expr::value(Some(body)));
    }
    if let Some(is_draft) = is_draft {
        update = update.col_expr(release::Column::IsDraft, Expr::value(is_draft));
    }
    if let Some(is_prerelease) = is_prerelease {
        update = update.col_expr(release::Column::IsPrerelease, Expr::value(is_prerelease));
    }

    let result = update
        .filter(release::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update release")?;
    match result.rows_affected {
        // MySQL may report zero for a no-op update. The identity re-read
        // distinguishes that from a delete without depending on backend
        // affected-row settings.
        0 | 1 => find_by_id(db, id).await,
        rows => anyhow::bail!("db: release update affected {rows} rows for id {id}"),
    }
}

/// Delete a release by ID. `Ok(false)` means no such row.
///
/// The caller's lookup and this `DELETE` are two statements: reporting
/// `rows_affected` is what stops a route from confirming a deletion that a
/// concurrent request had already performed.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = ReleaseEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete release")?;
    Ok(result.rows_affected > 0)
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

/// Delete an asset by ID. `Ok(false)` means no such row.
///
/// The caller's lookup and this `DELETE` are two statements: reporting
/// `rows_affected` is what stops a route from confirming a deletion that a
/// concurrent request had already performed.
pub async fn delete_asset_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = AssetEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete asset")?;
    Ok(result.rows_affected > 0)
}

/// Store (or clear) the detached attestation envelope JSON for an asset.
///
/// The caller resolves the asset in a repository-scoped read before reaching
/// this write. A concurrent DELETE can therefore win in between; `None` keeps
/// that ordinary absence out of SeaORM's backend-shaped `RecordNotUpdated`.
pub async fn set_asset_attestation(
    db: &DatabaseConnection,
    id: i64,
    attestation: Option<String>,
) -> Result<Option<AssetModel>> {
    let result = AssetEntity::update_many()
        .col_expr(release_asset::Column::Attestation, Expr::value(attestation))
        .filter(release_asset::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update asset attestation")?;
    match result.rows_affected {
        // MySQL may report zero for an unchanged envelope. Re-read the stable
        // identity so zero means absence only when the row is actually gone.
        0 | 1 => find_asset_by_id(db, id).await,
        rows => anyhow::bail!("db: asset attestation update affected {rows} rows for id {id}"),
    }
}

/// Increment an asset's download count atomically.
///
/// `Ok(false)` means a concurrent DELETE won before the write. Keeping the
/// addition inside the statement also prevents parallel downloads from
/// overwriting one another with the same stale count.
pub async fn increment_download_count(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = AssetEntity::update_many()
        .col_expr(
            release_asset::Column::DownloadCount,
            Expr::col(release_asset::Column::DownloadCount).add(1),
        )
        .filter(release_asset::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: increment asset download count")?;
    match result.rows_affected {
        0 => Ok(false),
        1 => Ok(true),
        rows => anyhow::bail!("db: asset download increment affected {rows} rows for id {id}"),
    }
}

/// How many assets `repo_id` has published, and how many bytes they declare.
///
/// The join is the only way from a release to its repository: `release_assets`
/// carries `release_id`, and the storage budget is per repository, so the
/// filter belongs on the parent row.
pub async fn repo_asset_usage(db: &DatabaseConnection, repo_id: i64) -> Result<(u64, i64)> {
    #[derive(Debug, FromQueryResult)]
    struct Usage {
        count: i64,
        bytes: Option<i64>,
    }
    let usage = AssetEntity::find()
        .join(JoinType::InnerJoin, release_asset::Relation::Release.def())
        .filter(release::Column::RepoId.eq(repo_id))
        .select_only()
        .column_as(release_asset::Column::Id.count(), "count")
        .column_as(release_asset::Column::Size.sum(), "bytes")
        .into_model::<Usage>()
        .one(db)
        .await
        .context("db: sum the release assets of a repository")?;
    Ok(usage.map_or((0, 0), |usage| {
        (usage.count.max(0) as u64, usage.bytes.unwrap_or(0))
    }))
}

/// How many assets one release holds, for the per-release entry ceiling.
pub async fn count_assets(db: &DatabaseConnection, release_id: i64) -> Result<u64> {
    let count = AssetEntity::find()
        .filter(release_asset::Column::ReleaseId.eq(release_id))
        .count(db)
        .await
        .context("db: count the release assets of a release")?;
    Ok(count)
}
