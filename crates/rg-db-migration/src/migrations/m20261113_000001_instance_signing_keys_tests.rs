use rg_db::entities::instance_signing_key as keys;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseConnection, EntityTrait, Statement};

use super::{Migration, NEW, OLD};

/// Recreate the pre-0.4.3 singleton with one row, as an instance that has not
/// run this migration holds it.
async fn seed_singleton(db: &DatabaseConnection, seed: &str, rotated: bool) {
    let backend = db.get_database_backend();
    let manager = SchemaManager::new(db);
    manager
        .create_table(
            Table::create()
                .table(Alias::new(OLD))
                .if_not_exists()
                .col(
                    ColumnDef::new(Alias::new("id"))
                        .big_integer()
                        .not_null()
                        .primary_key(),
                )
                .col(
                    ColumnDef::new(Alias::new("seed_encrypted"))
                        .text()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(Alias::new("created_at"))
                        .timestamp_with_time_zone()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(Alias::new("rotated_at"))
                        .timestamp_with_time_zone()
                        .null(),
                )
                .to_owned(),
        )
        .await
        .unwrap();
    db.execute_unprepared(&format!("DELETE FROM {OLD}"))
        .await
        .unwrap();
    let created = chrono::Utc::now() - chrono::Duration::days(30);
    let rotated_at: Option<chrono::DateTime<chrono::Utc>> = rotated.then(chrono::Utc::now);
    db.execute(
        backend.build(
            Query::insert()
                .into_table(Alias::new(OLD))
                .columns([
                    Alias::new("id"),
                    Alias::new("seed_encrypted"),
                    Alias::new("created_at"),
                    Alias::new("rotated_at"),
                ])
                .values_panic([1i64.into(), seed.into(), created.into(), rotated_at.into()]),
        ),
    )
    .await
    .unwrap();
}

async fn rows(db: &DatabaseConnection) -> Vec<keys::Model> {
    keys::Entity::find().all(db).await.unwrap()
}

/// The whole contract on one connection: carry-over with the exact origin,
/// resumption after an interruption at every step MySQL can stop at, and the
/// single-active-key index. Leaves the key rows as it found them, because on a
/// server database the table is shared with the rest of the job.
async fn exercise(db: &DatabaseConnection) {
    let saved = rows(db).await;
    let manager = SchemaManager::new(db);
    db.execute_unprepared(&format!("DELETE FROM {NEW}"))
        .await
        .unwrap();

    // A key adopted from jwt_secret (never rotated) → legacy_derived.
    seed_singleton(db, "sealed-adopted", false).await;
    Migration.up(&manager).await.unwrap();
    assert!(!manager.has_table(OLD).await.unwrap());
    let carried = rows(db).await;
    assert_eq!(carried.len(), 1);
    let row = &carried[0];
    assert_eq!(row.seed_encrypted.as_deref(), Some("sealed-adopted"));
    assert_eq!(row.origin, keys::ORIGIN_LEGACY_DERIVED);
    assert_eq!(row.status, keys::STATUS_ACTIVE);
    assert_eq!(row.active_slot, Some(keys::ACTIVE_SLOT));
    assert!(row.kid.is_none() && row.public_key.is_none());

    // Recorded-migration lost after the DROP: `up` again is a no-op.
    Migration.up(&manager).await.unwrap();
    assert_eq!(rows(db).await.len(), 1);

    // Crash after the copy but before the DROP: the copy is not repeated.
    seed_singleton(db, "sealed-adopted", false).await;
    Migration.up(&manager).await.unwrap();
    assert_eq!(rows(db).await.len(), 1);
    assert!(!manager.has_table(OLD).await.unwrap());

    // A key replaced by rotate-instance-key → generated.
    db.execute_unprepared(&format!("DELETE FROM {NEW}"))
        .await
        .unwrap();
    seed_singleton(db, "sealed-rotated", true).await;
    Migration.up(&manager).await.unwrap();
    let carried = rows(db).await;
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].origin, keys::ORIGIN_GENERATED);

    // At most one active key, enforced by the database; any number of
    // retired keys.
    let insert = |slot: &str, kid: &str| {
        format!(
            "INSERT INTO {NEW} (kid, origin, status, active_slot, created_at) \
             VALUES ('{kid}', 'generated', 'retired', {slot}, CURRENT_TIMESTAMP)"
        )
    };
    assert!(db
        .execute_unprepared(&insert("1", "aaaaaaaaaaaaaaaa"))
        .await
        .is_err());
    db.execute_unprepared(&insert("NULL", "bbbbbbbbbbbbbbbb"))
        .await
        .unwrap();
    db.execute_unprepared(&insert("NULL", "cccccccccccccccc"))
        .await
        .unwrap();
    assert!(db
        .execute_unprepared(&insert("NULL", "cccccccccccccccc"))
        .await
        .is_err());

    // Down carries the active key back; up brings it forward again.
    Migration.down(&manager).await.unwrap();
    assert!(!manager.has_table(NEW).await.unwrap());
    let back = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            format!("SELECT seed_encrypted FROM {OLD}"),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        back.try_get::<String>("", "seed_encrypted").unwrap(),
        "sealed-rotated"
    );
    Migration.up(&manager).await.unwrap();
    assert_eq!(
        rows(db).await[0].seed_encrypted.as_deref(),
        Some("sealed-rotated")
    );

    db.execute_unprepared(&format!("DELETE FROM {NEW}"))
        .await
        .unwrap();
    if !saved.is_empty() {
        // Back under their own ids, so the job's later steps find the keys
        // they established.
        keys::Entity::insert_many(saved.into_iter().map(keys::ActiveModel::from))
            .exec_without_returning(db)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn sqlite_carries_the_singleton_over_and_resumes_after_an_interruption() {
    let db = rg_db::connect_with_pool("sqlite::memory:", rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .unwrap();
    crate::run_migrations(&db).await.unwrap();
    exercise(&db).await;
}

/// The same on PostgreSQL or MySQL; MySQL is the reason every step must be
/// repeatable, since its DDL commits implicitly.
#[tokio::test]
#[ignore = "needs PLOMBIR_GIT_TEST_DATABASE_URL pointing at a disposable server database"]
async fn server_instance_signing_keys_carry_over_and_resume() {
    let url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    assert!(!url.starts_with("sqlite"), "SQLite is covered above");
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to the disposable server database");
    crate::run_migrations(&db).await.expect("run migrations");
    exercise(&db).await;
}
