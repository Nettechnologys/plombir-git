//! Database operations for repository deploy keys.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::deploy_key::{
    self, ActiveModel, Entity as DeployKeyEntity, Model as DeployKey,
};

pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<DeployKey>> {
    DeployKeyEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find deploy key by id")
}

pub async fn find_by_fingerprint(
    db: &DatabaseConnection,
    fingerprint: &str,
) -> Result<Option<DeployKey>> {
    DeployKeyEntity::find()
        .filter(deploy_key::Column::Fingerprint.eq(fingerprint))
        .one(db)
        .await
        .context("db: find deploy key by fingerprint")
}

pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<DeployKey>> {
    DeployKeyEntity::find()
        .filter(deploy_key::Column::RepoId.eq(repo_id))
        .order_by_asc(deploy_key::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list deploy keys by repository")
}

pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<DeployKey> {
    model.insert(db).await.context("db: create deploy key")
}

/// Record that a deploy key was used, at most once per
/// [`LAST_USED_RESOLUTION`](super::LAST_USED_RESOLUTION).
///
/// `previous` is the `last_used_at` the caller just read with the credential.
/// Within the window this returns without touching the database, so a burst
/// of requests on one credential is reads only.
pub async fn touch_last_used(
    db: &DatabaseConnection,
    id: i64,
    previous: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    let now = chrono::Utc::now();
    if !super::last_used_is_due(previous, now) {
        return Ok(());
    }
    DeployKeyEntity::update_many()
        .col_expr(deploy_key::Column::LastUsedAt, Expr::value(now))
        .filter(deploy_key::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update deploy key last used time")?;
    Ok(())
}

/// Delete a deploy key by id. `Ok(false)` means no such row.
///
/// The caller's lookup and this `DELETE` are two statements, so a concurrent
/// revocation can win in between; reporting `rows_affected` is what lets the
/// route answer 404 instead of confirming a revocation it did not perform.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = DeployKeyEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete deploy key")?;
    Ok(result.rows_affected > 0)
}
