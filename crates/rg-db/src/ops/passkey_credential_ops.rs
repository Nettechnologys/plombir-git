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

/// What became of an attempt to store an advanced credential.
///
/// Three outcomes rather than a `bool`, because the login path has to answer
/// each of them differently and a `DbErr` is a fourth thing again — an outage,
/// not a verdict on the ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterWrite {
    /// The advanced credential is what the row now holds.
    Stored,
    /// No such row: the credential was revoked while the ceremony was in
    /// flight. The assertion verified, but against something this account can
    /// no longer be signed in with.
    Missing,
    /// Another assertion advanced the same credential first, so the state this
    /// call verified against is no longer the stored state and the value it
    /// would write is derived from a snapshot the database has moved past.
    Conflict,
}

/// Store an advanced passkey over the exact snapshot it was derived from, and
/// stamp `last_used_at`.
///
/// A compare-and-swap, not a write. `expected_passkey_json` is the serialized
/// credential the caller loaded and verified the assertion against, and it is
/// part of the `WHERE`: the statement lands only if the row still holds it.
/// Filtering on the id alone made this a lost update on ceremony material —
/// two concurrent logins both verify against one stored state, and whichever
/// `UPDATE` runs second wins, so an assertion carrying the *lower* signature
/// counter can overwrite the higher one and both logins still report success.
/// The counter is exactly what the next assertion is checked against to spot a
/// cloned or replayed credential, so rolling it back quietly disarms that
/// check.
///
/// `rows_affected == 0` is not by itself a refusal: MySQL counts *changed*
/// rows, so a re-store of a byte-identical credential within the same
/// `last_used_at` resolution reports zero while having lost nothing. The row is
/// re-read to tell the three cases apart, and a row that already holds the
/// value this call wanted to write is a [`CounterWrite::Stored`] — whether this
/// call put it there or an identical concurrent one did, the state the caller
/// needs kept is kept.
pub async fn touch_and_update(
    db: &DatabaseConnection,
    id: i64,
    expected_passkey_json: &str,
    passkey_json: &str,
) -> Result<CounterWrite, DbErr> {
    let res = Entity::update_many()
        .set(passkey_credential::ActiveModel {
            passkey: Set(passkey_json.to_string()),
            last_used_at: Set(Some(chrono::Utc::now())),
            ..Default::default()
        })
        .filter(passkey_credential::Column::Id.eq(id))
        .filter(passkey_credential::Column::Passkey.eq(expected_passkey_json))
        .exec(db)
        .await?;
    if res.rows_affected > 0 {
        return Ok(CounterWrite::Stored);
    }

    match Entity::find_by_id(id).one(db).await? {
        None => Ok(CounterWrite::Missing),
        Some(row) if row.passkey == passkey_json => Ok(CounterWrite::Stored),
        Some(_) => Ok(CounterWrite::Conflict),
    }
}
