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
pub async fn save(
    db: &DatabaseConnection,
    maintenance_mode: bool,
    banner_message: Option<&str>,
    banner_type: &str,
) -> Result<instance_settings::Model, DbErr> {
    let now = chrono::Utc::now();
    match Entity::find_by_id(SINGLETON_ID).one(db).await? {
        Some(existing) => {
            let mut am: instance_settings::ActiveModel = existing.into();
            am.maintenance_mode = Set(maintenance_mode);
            am.banner_message = Set(banner_message.map(str::to_string));
            am.banner_type = Set(banner_type.to_string());
            am.updated_at = Set(now);
            am.update(db).await
        }
        None => {
            instance_settings::ActiveModel {
                id: Set(SINGLETON_ID),
                maintenance_mode: Set(maintenance_mode),
                banner_message: Set(banner_message.map(str::to_string)),
                banner_type: Set(banner_type.to_string()),
                updated_at: Set(now),
            }
            .insert(db)
            .await
        }
    }
}
