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

pub async fn touch_last_used(db: &DatabaseConnection, id: i64) -> Result<()> {
    DeployKeyEntity::update_many()
        .col_expr(
            deploy_key::Column::LastUsedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(deploy_key::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update deploy key last used time")?;
    Ok(())
}

pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    DeployKeyEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete deploy key")?;
    Ok(())
}
