//! Database operations for `notification_settings`.

use anyhow::{Context, Result};
use sea_orm::sea_query::OnConflict;
use sea_orm::*;

use crate::entities::notification_setting;

/// The account's mail choices — the defaults when it never changed any.
pub async fn get(db: &DatabaseConnection, user_id: i64) -> Result<notification_setting::Model> {
    Ok(notification_setting::Entity::find_by_id(user_id)
        .one(db)
        .await
        .context("db: read notification settings")?
        .unwrap_or_else(|| notification_setting::Model::defaults_for(user_id)))
}

/// The mail choices of every account in `user_ids`, in one query — the
/// defaults for an account that never changed any.
pub async fn get_many(
    db: &DatabaseConnection,
    user_ids: &[i64],
) -> Result<std::collections::HashMap<i64, notification_setting::Model>> {
    let mut settings: std::collections::HashMap<i64, notification_setting::Model> = user_ids
        .iter()
        .map(|&user_id| (user_id, notification_setting::Model::defaults_for(user_id)))
        .collect();
    if user_ids.is_empty() {
        return Ok(settings);
    }
    for row in notification_setting::Entity::find()
        .filter(notification_setting::Column::UserId.is_in(user_ids.iter().copied()))
        .all(db)
        .await
        .context("db: read notification settings")?
    {
        settings.insert(row.user_id, row);
    }
    Ok(settings)
}

/// Store the account's mail choices, whole.
pub async fn put(
    db: &DatabaseConnection,
    settings: notification_setting::Model,
) -> Result<notification_setting::Model> {
    use notification_setting::Column;
    let user_id = settings.user_id;
    let mut active: notification_setting::ActiveModel = settings.into();
    active.updated_at = Set(chrono::Utc::now());
    notification_setting::Entity::insert(active.reset_all())
        .on_conflict(
            OnConflict::column(Column::UserId)
                .update_columns([
                    Column::EmailReviewRequested,
                    Column::EmailMention,
                    Column::EmailAssigned,
                    Column::EmailCiFailed,
                    Column::EmailParticipating,
                    Column::EmailCiTriggered,
                    Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(db)
        .await
        .context("db: write notification settings")?;
    get(db, user_id).await
}
