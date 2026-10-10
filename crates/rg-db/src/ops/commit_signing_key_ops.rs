//! Account signing-key registry operations.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::{commit_signing_key, user};

pub async fn find_by_id(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<commit_signing_key::Model>> {
    commit_signing_key::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find commit signing key by id")
}

pub async fn list_by_user(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<commit_signing_key::Model>> {
    commit_signing_key::Entity::find()
        .filter(commit_signing_key::Column::UserId.eq(user_id))
        .order_by_asc(commit_signing_key::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list account commit signing keys")
}

/// A snapshot of currently trusted keys and the account email each may sign.
pub async fn list_active_verified(
    db: &DatabaseConnection,
) -> Result<Vec<(commit_signing_key::Model, String)>> {
    let rows = commit_signing_key::Entity::find()
        .find_also_related(user::Entity)
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .filter(user::Column::EmailVerifiedAt.is_not_null())
        .all(db)
        .await
        .context("db: load trusted commit signing keys")?;
    rows.into_iter()
        .map(|(key, account)| {
            let account = account.context("commit signing key owner is missing")?;
            Ok((key, account.email))
        })
        .collect()
}

pub async fn create(
    db: &DatabaseConnection,
    model: commit_signing_key::ActiveModel,
) -> Result<commit_signing_key::Model> {
    model
        .insert(db)
        .await
        .context("db: create commit signing key")
}

/// Ownership is part of the DELETE, so a concurrent change cannot turn a
/// checked row into a different account's successful deletion.
pub async fn delete_by_user(db: &DatabaseConnection, id: i64, user_id: i64) -> Result<bool> {
    let deleted = commit_signing_key::Entity::delete_many()
        .filter(commit_signing_key::Column::Id.eq(id))
        .filter(commit_signing_key::Column::UserId.eq(user_id))
        .exec(db)
        .await
        .context("db: delete account commit signing key")?;
    Ok(deleted.rows_affected == 1)
}
