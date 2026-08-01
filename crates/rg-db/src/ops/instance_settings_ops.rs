//! Instance-wide settings operations — read and write the singleton row.
use sea_orm::*;

use crate::entities::instance_settings;
pub use crate::entities::instance_settings::Entity;
pub use crate::entities::instance_settings::SINGLETON_ID;

/// Read the instance settings row, if one has ever been written.
///
/// `None` is not an error: a server that has never had its settings touched
/// has no row, and the caller resolves that to the defaults.
pub async fn find(db: &DatabaseConnection) -> Result<Option<instance_settings::Model>, DbErr> {
    Entity::find_by_id(SINGLETON_ID).one(db).await
}

/// Write the instance settings, creating the row on first use.
///
/// Insert-or-update rather than an upsert statement so the same code path holds
/// on every backend the server supports.
///
/// The read and the insert are two statements, and `id` is the primary key.
/// Two admins turning on maintenance mode on a never-configured instance — the
/// moment the row is most likely to be missing is also the moment several
/// people are reaching for it — both read `None` and both insert; one meets the
/// key. That loss says the singleton this call wanted now exists, so it is
/// resolved by re-reading it and writing this call's settings onto it. Last
/// save wins, whole: the banner text and its type come from one submission,
/// never one field from each.
///
/// Only a UNIQUE/primary-key violation is treated this way — a broken
/// connection stays an error, because reporting success for a maintenance-mode
/// switch that was never written is exactly the silent failure this promises
/// not to be.
pub async fn save(
    db: &DatabaseConnection,
    maintenance_mode: bool,
    banner_message: Option<&str>,
    banner_type: &str,
) -> Result<instance_settings::Model, DbErr> {
    let now = chrono::Utc::now();
    if let Some(existing) = find(db).await? {
        return apply_settings(
            db,
            existing,
            maintenance_mode,
            banner_message,
            banner_type,
            now,
        )
        .await;
    }

    let insert = instance_settings::ActiveModel {
        id: Set(SINGLETON_ID),
        maintenance_mode: Set(maintenance_mode),
        banner_message: Set(banner_message.map(str::to_string)),
        banner_type: Set(banner_type.to_string()),
        updated_at: Set(now),
    }
    .insert(db)
    .await;

    match insert {
        Ok(created) => Ok(created),
        Err(error) if crate::is_unique_violation(&error) => match find(db).await? {
            Some(existing) => {
                apply_settings(
                    db,
                    existing,
                    maintenance_mode,
                    banner_message,
                    banner_type,
                    now,
                )
                .await
            }
            // Not there after all, so the collision was on some other
            // constraint. Report the original failure rather than inventing a
            // reason for it.
            None => Err(error),
        },
        Err(error) => Err(error),
    }
}

/// Write this call's settings onto the existing singleton row.
async fn apply_settings(
    db: &DatabaseConnection,
    existing: instance_settings::Model,
    maintenance_mode: bool,
    banner_message: Option<&str>,
    banner_type: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<instance_settings::Model, DbErr> {
    let mut am: instance_settings::ActiveModel = existing.into();
    am.maintenance_mode = Set(maintenance_mode);
    am.banner_message = Set(banner_message.map(str::to_string));
    am.banner_type = Set(banner_type.to_string());
    am.updated_at = Set(now);
    am.update(db).await
}
