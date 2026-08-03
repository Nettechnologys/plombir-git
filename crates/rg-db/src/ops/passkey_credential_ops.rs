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
    rp_id: &str,
) -> Result<passkey_credential::Model, DbErr> {
    let am = passkey_credential::ActiveModel {
        id: NotSet,
        user_id: Set(user_id),
        credential_id: Set(credential_id.to_string()),
        passkey: Set(passkey_json.to_string()),
        name: Set(name.to_string()),
        rp_id: Set(Some(rp_id.to_string())),
        created_at: Set(chrono::Utc::now()),
        last_used_at: Set(None),
    };
    am.insert(db).await
}

/// Count credentials created before the relying-party id was persisted.
///
/// A NULL value deliberately means "unknown", not an RP id inferred during an
/// upgrade: inventing one would silently make a pre-existing credential belong
/// to the wrong hostname.
pub async fn count_legacy_without_rp_id(db: &DatabaseConnection) -> Result<u64, DbErr> {
    Entity::find()
        .filter(passkey_credential::Column::RpId.is_null())
        .count(db)
        .await
}

/// Count every registered passkey without loading its serialized credential.
pub async fn count_all(db: &DatabaseConnection) -> Result<u64, DbErr> {
    Entity::find().count(db).await
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
/// stamp `last_used_at`. Returns false when no row with that id exists.
///
/// The absent row is a `false`, not a `DbErr`, on purpose: the caller is the
/// login path, and "this credential is no longer registered" and "the write
/// failed" are two different answers there — one ends the ceremony, the other
/// is a retryable outage. Folding the first into an error left the caller
/// unable to tell them apart, so it swallowed both.
///
/// A single `UPDATE ... WHERE id = ?` rather than read-then-write, so the row
/// cannot be deleted between the two statements and turn a concurrent
/// revocation into `RecordNotUpdated`.
pub async fn touch_and_update(
    db: &DatabaseConnection,
    id: i64,
    passkey_json: &str,
) -> Result<bool, DbErr> {
    let res = Entity::update_many()
        .set(passkey_credential::ActiveModel {
            passkey: Set(passkey_json.to_string()),
            last_used_at: Set(Some(chrono::Utc::now())),
            ..Default::default()
        })
        .filter(passkey_credential::Column::Id.eq(id))
        .exec(db)
        .await?;
    Ok(res.rows_affected > 0)
}
