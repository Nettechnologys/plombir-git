//! Database operations for SSH keys.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::ssh_key::{self, ActiveModel, Entity as SshKeyEntity, Model as SshKey};

/// Find an SSH key by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<SshKey>> {
    SshKeyEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find ssh key by id")
}

/// Find an SSH key by its fingerprint.
pub async fn find_by_fingerprint(
    db: &DatabaseConnection,
    fingerprint: &str,
) -> Result<Option<SshKey>> {
    SshKeyEntity::find()
        .filter(ssh_key::Column::Fingerprint.eq(fingerprint))
        .one(db)
        .await
        .context("db: find ssh key by fingerprint")
}

/// List all SSH keys for a user.
pub async fn list_by_user(db: &DatabaseConnection, user_id: i64) -> Result<Vec<SshKey>> {
    SshKeyEntity::find()
        .filter(ssh_key::Column::UserId.eq(user_id))
        .order_by_asc(ssh_key::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list ssh keys by user")
}

/// Create a new SSH key.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<SshKey> {
    model.insert(db).await.context("db: create ssh key")
}

/// Record that an SSH key was successfully used for authentication.
pub async fn touch_last_used(db: &DatabaseConnection, id: i64) -> Result<()> {
    SshKeyEntity::update_many()
        .col_expr(ssh_key::Column::LastUsedAt, Expr::value(chrono::Utc::now()))
        .filter(ssh_key::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update SSH key last used time")?;
    Ok(())
}

/// Delete an SSH key by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    SshKeyEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete ssh key")?;
    Ok(())
}
