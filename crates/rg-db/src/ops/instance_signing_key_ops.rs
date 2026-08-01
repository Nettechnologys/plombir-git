//! Instance signing-key operations — read and write the singleton row.
use sea_orm::*;

use crate::entities::instance_signing_key;
pub use crate::entities::instance_signing_key::Entity;
pub use crate::entities::instance_signing_key::SINGLETON_ID;

/// Read the stored key row, if this instance has ever established one.
///
/// `None` is not an error: it means "no identity yet", which the caller
/// resolves by adopting one.
pub async fn find(db: &DatabaseConnection) -> Result<Option<instance_signing_key::Model>, DbErr> {
    Entity::find_by_id(SINGLETON_ID).one(db).await
}

/// Insert the singleton row. Fails if it already exists — the caller must treat
/// that as "another process got there first" and re-read, never as a reason to
/// overwrite: two servers sharing a database must end up with the same key.
pub async fn insert(
    db: &DatabaseConnection,
    seed_encrypted: &str,
) -> Result<instance_signing_key::Model, DbErr> {
    instance_signing_key::ActiveModel {
        id: Set(SINGLETON_ID),
        seed_encrypted: Set(seed_encrypted.to_string()),
        created_at: Set(chrono::Utc::now()),
        rotated_at: Set(None),
    }
    .insert(db)
    .await
}

/// Replace the stored key material — the deliberate rotation path.
///
/// `created_at` is left alone (it dates the instance's first identity, not this
/// key) and `rotated_at` records when the replacement happened.
pub async fn replace(
    db: &DatabaseConnection,
    seed_encrypted: &str,
) -> Result<instance_signing_key::Model, DbErr> {
    let now = chrono::Utc::now();
    match Entity::find_by_id(SINGLETON_ID).one(db).await? {
        Some(existing) => {
            let mut am: instance_signing_key::ActiveModel = existing.into();
            am.seed_encrypted = Set(seed_encrypted.to_string());
            am.rotated_at = Set(Some(now));
            am.update(db).await
        }
        None => {
            instance_signing_key::ActiveModel {
                id: Set(SINGLETON_ID),
                seed_encrypted: Set(seed_encrypted.to_string()),
                created_at: Set(now),
                rotated_at: Set(Some(now)),
            }
            .insert(db)
            .await
        }
    }
}
