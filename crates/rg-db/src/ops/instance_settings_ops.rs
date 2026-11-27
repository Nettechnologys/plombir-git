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

/// Create the singleton row with the default settings unless it exists, so a
/// change can lock it.
///
/// A row created here says what no row already meant — the defaults — so
/// creating it changes nothing anyone can observe. Two administrators turning
/// on maintenance mode on a never-configured instance both find no row and both
/// insert; one meets the primary key. That loss is the outcome this wanted —
/// the singleton exists — so only a UNIQUE/primary-key violation is treated
/// this way. A broken connection stays an error.
pub async fn ensure_exists(db: &DatabaseConnection) -> Result<(), DbErr> {
    if find(db).await?.is_some() {
        return Ok(());
    }
    let insert = instance_settings::ActiveModel {
        id: Set(SINGLETON_ID),
        maintenance_mode: Set(false),
        banner_message: Set(None),
        banner_type: Set(String::new()),
        updated_at: Set(chrono::Utc::now()),
    }
    .insert(db)
    .await;
    match insert {
        Ok(_) => Ok(()),
        Err(error) if crate::is_unique_violation(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

/// The singleton row, locked until `transaction` ends: `FOR UPDATE` on
/// PostgreSQL and MySQL, the database's writer lock on SQLite.
///
/// card_adbe712869d6: SQLite has no row lock, and the comment here used to
/// say it "serializes the writers instead". It does not for a transaction that
/// reads first. SeaORM's `BEGIN` is `BEGIN DEFERRED`, so the `SELECT` below took
/// a read snapshot and the later `UPDATE` asked to upgrade it; any commit on
/// another connection in between — to any table — made that snapshot stale,
/// and SQLite refused the upgrade at once with `SQLITE_BUSY_SNAPSHOT` (517),
/// which `busy_timeout` does not cover. A settings PATCH answered 500.
///
/// So on SQLite the first statement is a write to the row itself: it takes
/// the writer lock (waiting for it under `busy_timeout`, like any writer), and
/// the read after it can no longer be overtaken. A missing row makes the write
/// a no-op that still holds the lock. See `rg_db::contention` for the class.
pub async fn find_for_update(
    transaction: &DatabaseTransaction,
) -> Result<Option<instance_settings::Model>, DbErr> {
    if transaction.get_database_backend() == DatabaseBackend::Sqlite {
        Entity::update_many()
            .col_expr(
                instance_settings::Column::UpdatedAt,
                sea_orm::sea_query::Expr::col(instance_settings::Column::UpdatedAt).into(),
            )
            .filter(instance_settings::Column::Id.eq(SINGLETON_ID))
            .exec(transaction)
            .await?;
    }
    Entity::find_by_id(SINGLETON_ID)
        .lock_exclusive()
        .one(transaction)
        .await
}

/// Write this call's settings onto the existing singleton row.
pub async fn apply_settings(
    db: &impl ConnectionTrait,
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
