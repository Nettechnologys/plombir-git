//! The picture an account uploaded. See [`crate::entities::user_avatar`].
//!
//! `users.avatar_url` is written in the same transaction as the bytes, so the
//! URL every page renders never names a picture that is not there — nor keeps
//! pointing at one that was removed.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::*;

use crate::entities::{user, user_avatar};

/// The stored picture of `user_id`, if it has one.
pub async fn find(db: &DatabaseConnection, user_id: i64) -> Result<Option<user_avatar::Model>> {
    user_avatar::Entity::find_by_id(user_id)
        .one(db)
        .await
        .context("db: read an avatar")
}

/// Store `bytes` as the picture of `user_id` and point its `avatar_url` at
/// `url`. `false` when the account is gone or retiring.
pub async fn replace(
    db: &DatabaseConnection,
    user_id: i64,
    content_type: &str,
    sha256: &str,
    bytes: Vec<u8>,
    url: &str,
) -> Result<bool> {
    let transaction = db.begin().await.context("db: begin avatar upload")?;
    if !point_at(&transaction, user_id, Some(url)).await? {
        transaction
            .rollback()
            .await
            .context("db: roll back avatar upload")?;
        return Ok(false);
    }
    user_avatar::Entity::insert(user_avatar::ActiveModel {
        user_id: Set(user_id),
        content_type: Set(content_type.to_string()),
        sha256: Set(sha256.to_string()),
        bytes: Set(bytes),
        updated_at: Set(Utc::now()),
    })
    .on_conflict(
        OnConflict::column(user_avatar::Column::UserId)
            .update_columns([
                user_avatar::Column::ContentType,
                user_avatar::Column::Sha256,
                user_avatar::Column::Bytes,
                user_avatar::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec_without_returning(&transaction)
    .await
    .context("db: store an avatar")?;
    transaction
        .commit()
        .await
        .context("db: commit avatar upload")?;
    Ok(true)
}

/// Remove the picture of `user_id` and clear its `avatar_url`. `false` when
/// the account is gone or retiring.
pub async fn remove(db: &DatabaseConnection, user_id: i64) -> Result<bool> {
    let transaction = db.begin().await.context("db: begin avatar removal")?;
    if !point_at(&transaction, user_id, None).await? {
        transaction
            .rollback()
            .await
            .context("db: roll back avatar removal")?;
        return Ok(false);
    }
    user_avatar::Entity::delete_by_id(user_id)
        .exec(&transaction)
        .await
        .context("db: remove an avatar")?;
    transaction
        .commit()
        .await
        .context("db: commit avatar removal")?;
    Ok(true)
}

async fn point_at<C: ConnectionTrait>(db: &C, user_id: i64, url: Option<&str>) -> Result<bool> {
    let updated = user::Entity::update_many()
        .col_expr(
            user::Column::AvatarUrl,
            Expr::value(url.map(str::to_string)),
        )
        .col_expr(user::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(user::Column::Id.eq(user_id))
        .filter(user::Column::DeletedAt.is_null())
        .exec(db)
        .await
        .context("db: point avatar_url at the stored picture")?;
    Ok(updated.rows_affected == 1)
}
