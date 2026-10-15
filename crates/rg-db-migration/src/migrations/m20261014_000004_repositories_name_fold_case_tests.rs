//! The upgrade over the pool the server opens: several connections, one of
//! which adds `name_case_exempt` while another may hold an older schema.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::{MigrationName, MigratorTrait};

use super::Migration;
use crate::migrations::Migrator;

/// The upgrade added the mark on one connection and read it back on another: a
/// connection whose cached schema predates the `ALTER TABLE` prepared
/// `"name_case_exempt"` as a string literal (SQLite falls back to one for a
/// double-quoted name it cannot resolve), so the rows it returned had no such
/// column and every upgrade of a database holding a repository failed with
/// `no column found for name: name_case_exempt`. The tests of this migration
/// beside this file run on a pool of one, where it cannot happen.
#[tokio::test]
async fn the_upgrade_reads_the_mark_it_added_on_a_pool_of_several_connections() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("plombir.db").display()
    );
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway SQLite database");
    let before = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == Migration.name())
        .expect("the migration is part of the list");
    Migrator::up(&db, Some(before.try_into().unwrap()))
        .await
        .expect("migrate to the schema right before this migration");

    db.execute_unprepared(
        "INSERT INTO users (id, username, email, password_hash, is_admin, is_active, \
           auth_provider, mfa_enabled, login_attempts, session_version, created_at, updated_at) \
         VALUES (1, 'alice', 'alice@example.invalid', '', 0, 1, 'local', 0, 0, 0, \
           '2026-10-01 00:00:00+00:00', '2026-10-01 00:00:00+00:00'); \
         INSERT INTO repositories (id, owner_id, name, is_private, default_branch, \
           stars_count, forks_count, created_at, updated_at) VALUES \
           (1, 1, 'Payments', 0, 'main', 0, 0, '2026-10-01', '2026-10-01'), \
           (2, 1, 'payments', 0, 'main', 0, 0, '2026-10-02', '2026-10-02');",
    )
    .await
    .expect("seed an account with two repositories that differ only in case");

    // Open every connection the pool may have: the one that runs the
    // `ALTER TABLE` must not be the only one that exists.
    let pool = db.get_sqlite_connection_pool();
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(pool.acquire().await.expect("open a pooled connection"));
    }
    assert!(pool.size() > 1, "a pool of one would prove nothing here");
    drop(held);

    Migrator::up(&db, None)
        .await
        .expect("the upgrade over a populated database");

    let keys: Vec<(i64, Option<String>)> = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT id, name_key FROM repositories ORDER BY id".to_string(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get::<i64>("", "id").unwrap(),
                row.try_get::<Option<String>>("", "name_key").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        keys,
        vec![(1, Some("payments".to_string())), (2, None)],
        "the older twin keeps the key, the younger is marked"
    );
}
