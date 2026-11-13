//! Instance signing-key operations. The rules that make a rotation safe live
//! here as SQL predicates, so a racing replica loses on the database, not in
//! application code that read a stale row.
use sea_orm::sea_query::Expr;
use sea_orm::*;

pub use crate::entities::instance_signing_key::Entity;
use crate::entities::instance_signing_key::{
    self, ACTIVE_SLOT, STATUS_ACTIVE, STATUS_RETIRED, STATUS_REVOKED,
};

/// Every key this instance has held, oldest first.
pub async fn all(db: &impl ConnectionTrait) -> Result<Vec<instance_signing_key::Model>, DbErr> {
    Entity::find()
        .order_by_asc(instance_signing_key::Column::Id)
        .all(db)
        .await
}

/// The key that signs, if one has been established.
pub async fn find_active(
    db: &impl ConnectionTrait,
) -> Result<Option<instance_signing_key::Model>, DbErr> {
    Entity::find()
        .filter(instance_signing_key::Column::Status.eq(STATUS_ACTIVE))
        .one(db)
        .await
}

pub async fn find_by_kid(
    db: &impl ConnectionTrait,
    kid: &str,
) -> Result<Option<instance_signing_key::Model>, DbErr> {
    Entity::find()
        .filter(instance_signing_key::Column::Kid.eq(kid))
        .one(db)
        .await
}

/// Insert a new active key. Fails on the `active_slot` unique index when
/// another active key exists — the caller re-reads instead of overwriting, so
/// two processes sharing a database end up with one identity.
pub async fn insert_active(
    db: &impl ConnectionTrait,
    kid: &str,
    public_key: &str,
    seed_encrypted: &str,
    origin: &str,
) -> Result<instance_signing_key::Model, DbErr> {
    instance_signing_key::ActiveModel {
        id: NotSet,
        kid: Set(Some(kid.to_owned())),
        public_key: Set(Some(public_key.to_owned())),
        seed_encrypted: Set(Some(seed_encrypted.to_owned())),
        origin: Set(origin.to_owned()),
        status: Set(STATUS_ACTIVE.to_owned()),
        active_slot: Set(Some(ACTIVE_SLOT)),
        created_at: Set(chrono::Utc::now()),
        retired_at: Set(None),
        revoked_at: Set(None),
    }
    .insert(db)
    .await
}

/// Record the public half of a row carried over without one. Compare-and-set
/// on `kid IS NULL`: concurrent fillers compute the same values, and the first
/// write stands.
pub async fn fill_identity(
    db: &impl ConnectionTrait,
    id: i64,
    kid: &str,
    public_key: &str,
) -> Result<u64, DbErr> {
    Ok(Entity::update_many()
        .col_expr(instance_signing_key::Column::Kid, Expr::value(kid))
        .col_expr(
            instance_signing_key::Column::PublicKey,
            Expr::value(public_key),
        )
        .filter(instance_signing_key::Column::Id.eq(id))
        .filter(instance_signing_key::Column::Kid.is_null())
        .exec(db)
        .await?
        .rows_affected)
}

/// Take the active key `id` out of service as `retired` or `revoked`, dropping
/// its sealed seed in the same statement. Compare-and-set on the row still
/// being active: `0` means somebody else already rotated it.
pub async fn retire_active(db: &impl ConnectionTrait, id: i64, revoke: bool) -> Result<u64, DbErr> {
    let now = chrono::Utc::now();
    let mut update = Entity::update_many()
        .col_expr(
            instance_signing_key::Column::ActiveSlot,
            Expr::value(Option::<i16>::None),
        )
        .col_expr(
            instance_signing_key::Column::SeedEncrypted,
            Expr::value(Option::<String>::None),
        )
        .col_expr(instance_signing_key::Column::RetiredAt, Expr::value(now));
    update = if revoke {
        update
            .col_expr(
                instance_signing_key::Column::Status,
                Expr::value(STATUS_REVOKED),
            )
            .col_expr(instance_signing_key::Column::RevokedAt, Expr::value(now))
    } else {
        update.col_expr(
            instance_signing_key::Column::Status,
            Expr::value(STATUS_RETIRED),
        )
    };
    Ok(update
        .filter(instance_signing_key::Column::Id.eq(id))
        .filter(instance_signing_key::Column::Status.eq(STATUS_ACTIVE))
        .exec(db)
        .await?
        .rows_affected)
}

/// Revoke an already retired key. The active key is never revoked here: that
/// would leave the instance without a signer, so that path is a rotation.
pub async fn revoke_retired(db: &impl ConnectionTrait, kid: &str) -> Result<u64, DbErr> {
    Ok(Entity::update_many()
        .col_expr(
            instance_signing_key::Column::Status,
            Expr::value(STATUS_REVOKED),
        )
        .col_expr(
            instance_signing_key::Column::RevokedAt,
            Expr::value(chrono::Utc::now()),
        )
        .col_expr(
            instance_signing_key::Column::SeedEncrypted,
            Expr::value(Option::<String>::None),
        )
        .filter(instance_signing_key::Column::Kid.eq(kid))
        .filter(instance_signing_key::Column::Status.eq(STATUS_RETIRED))
        .exec(db)
        .await?
        .rows_affected)
}
