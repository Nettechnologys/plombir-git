//! Passkey (WebAuthn credential) operations.
use sea_orm::*;

use crate::entities::passkey_credential;
pub use crate::entities::passkey_credential::Entity;

/// Store a newly registered passkey for a user.
pub async fn create(
    db: &DatabaseConnection,
    user_id: i64,
    credential_id: &str,
    passkey_json: &str,
    name: &str,
) -> Result<passkey_credential::Model, DbErr> {
    let am = passkey_credential::ActiveModel {
        id: NotSet,
        user_id: Set(user_id),
        credential_id: Set(credential_id.to_string()),
        passkey: Set(passkey_json.to_string()),
        name: Set(name.to_string()),
        created_at: Set(chrono::Utc::now()),
        last_used_at: Set(None),
    };
    am.insert(db).await
}

/// List all passkeys registered by a user (newest first).
pub async fn list_by_user(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<passkey_credential::Model>, DbErr> {
    Entity::find()
        .filter(passkey_credential::Column::UserId.eq(user_id))
        .order_by_desc(passkey_credential::Column::CreatedAt)
        .all(db)
        .await
}

/// Delete a passkey by id, scoped to its owner. Returns true if a row was removed.
pub async fn delete(db: &DatabaseConnection, user_id: i64, id: i64) -> Result<bool, DbErr> {
    let res = Entity::delete_many()
        .filter(passkey_credential::Column::Id.eq(id))
        .filter(passkey_credential::Column::UserId.eq(user_id))
        .exec(db)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Persist an updated passkey (e.g. after the signature counter advanced) and
/// stamp `last_used_at`.
pub async fn touch_and_update(
    db: &DatabaseConnection,
    id: i64,
    passkey_json: &str,
) -> Result<(), DbErr> {
    let existing = Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("passkey {id}")))?;
    let mut am: passkey_credential::ActiveModel = existing.into();
    am.passkey = Set(passkey_json.to_string());
    am.last_used_at = Set(Some(chrono::Utc::now()));
    am.update(db).await?;
    Ok(())
}
