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
