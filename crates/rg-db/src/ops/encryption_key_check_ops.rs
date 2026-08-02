//! Read and replace the singleton encryption-key check marker.
use sea_orm::*;

use crate::entities::encryption_key_check;
pub use crate::entities::encryption_key_check::Entity;
pub use crate::entities::encryption_key_check::SINGLETON_ID;

pub async fn find<C>(db: &C) -> Result<Option<encryption_key_check::Model>, DbErr>
where
    C: ConnectionTrait,
{
    Entity::find_by_id(SINGLETON_ID).one(db).await
}

pub async fn insert<C>(db: &C, value_encrypted: &str) -> Result<encryption_key_check::Model, DbErr>
where
    C: ConnectionTrait,
{
    encryption_key_check::ActiveModel {
        id: Set(SINGLETON_ID),
        value_encrypted: Set(value_encrypted.to_owned()),
    }
    .insert(db)
    .await
}

/// Store the marker, creating it if the database predates key markers.
pub async fn replace<C>(db: &C, value_encrypted: &str) -> Result<encryption_key_check::Model, DbErr>
where
    C: ConnectionTrait,
{
    match find(db).await? {
        Some(existing) => {
            let mut active: encryption_key_check::ActiveModel = existing.into();
            active.value_encrypted = Set(value_encrypted.to_owned());
            active.update(db).await
        }
        None => insert(db, value_encrypted).await,
    }
}
