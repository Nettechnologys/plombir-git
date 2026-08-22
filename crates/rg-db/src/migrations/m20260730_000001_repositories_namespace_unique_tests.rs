use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait, TryGetable,
};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use tokio::sync::{oneshot, Notify};

use super::{sqlite_rebuild, sqlite_rebuild_with_hook, Shape, SqliteRebuildPoint};
use crate::test_support::{assert_writer_stays_blocked, write_while_the_lock_is_held};

const SURVIVING_REPO_ID: i64 = 1;
const DELETED_REPO_ID: i64 = 2;
const HIGH_WATER_REPO_ID: i64 = 50;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-namespace-rebuild-{label}-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

async fn fixture(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway SQLite database");

    const REBUILD: &str = "m20260730_000001_repositories_namespace_unique";
    let before_rebuild = crate::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == REBUILD)
        .unwrap_or_else(|| panic!("{REBUILD} must still be part of the migration list"));
    let before_rebuild = u32::try_from(before_rebuild).expect("migration index fits in u32");
    crate::migrations::Migrator::up(&db, Some(before_rebuild))
        .await
        .expect("migrate to the schema immediately before the rebuild");

    db.execute_unprepared(&format!(
        r#"
        INSERT INTO users
            (id, username, email, password_hash, created_at, updated_at)
        VALUES
            (1, 'namespace-safety', 'namespace-safety@example.invalid', '',
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories
            (id, owner_id, name, description, created_at, updated_at)
        VALUES
            ({SURVIVING_REPO_ID}, 1, 'surviving', 'before rebuild',
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            ({DELETED_REPO_ID}, 1, 'delete-me', 'delete during rebuild',
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            ({HIGH_WATER_REPO_ID}, 1, 'high-water', 'deleted before rebuild',
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repo_stars (user_id, repo_id, created_at)
        VALUES (1, {SURVIVING_REPO_ID}, CURRENT_TIMESTAMP);
        DELETE FROM repositories WHERE id = {HIGH_WATER_REPO_ID};
        "#
    ))
    .await
    .expect("seed source, child, FTS and sequence state");

    assert_eq!(
        scalar(
            &db,
            "SELECT seq FROM sqlite_sequence WHERE name = 'repositories'",
        )
        .await,
        HIGH_WATER_REPO_ID
    );
    assert_source_matches_fts(&db).await;
    (db, temp)
}

async fn rows(db: &DatabaseConnection, sql: &str) -> Vec<(i64, String, String)> {
    db.query_all(Statement::from_string(
        DatabaseBackend::Sqlite,
        sql.to_string(),
    ))
    .await
    .expect("read rows")
    .into_iter()
    .map(|row| {
        (
            row.try_get::<i64>("", "id").expect("row id"),
            row.try_get::<String>("", "name").expect("row name"),
            row.try_get::<String>("", "description")
                .expect("row description"),
        )
    })
    .collect()
}

async fn source_rows(db: &DatabaseConnection) -> Vec<(i64, String, String)> {
    rows(
        db,
        "SELECT id, name, COALESCE(description, '') AS description \
         FROM repositories WHERE deleted_at IS NULL ORDER BY id",
    )
    .await
}

async fn fts_rows(db: &DatabaseConnection) -> Vec<(i64, String, String)> {
    rows(
        db,
        "SELECT rowid AS id, name, description FROM repos_fts ORDER BY rowid",
    )
    .await
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            sql.to_string(),
        ))
        .await
        .expect("read scalar")
        .expect("scalar row exists");
    i64::try_get_by_index(&row, 0).expect("decode scalar")
}

async fn table_sql(db: &DatabaseConnection) -> String {
    db.query_one(Statement::from_string(
        DatabaseBackend::Sqlite,
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'repositories'",
    ))
    .await
    .expect("read repositories schema")
    .expect("repositories table exists")
    .try_get::<String>("", "sql")
    .expect("decode repositories schema")
}

async fn assert_source_matches_fts(db: &DatabaseConnection) {
    assert_eq!(
        fts_rows(db).await,
        source_rows(db).await,
        "repos_fts diverged from its live repository rows"
    );
}

async fn assert_rebuild_invariants(db: &DatabaseConnection) {
    assert_source_matches_fts(db).await;
    assert_eq!(
        scalar(
            db,
            "SELECT count(*) FROM sqlite_master \
             WHERE type = 'trigger' AND name IN \
             ('repos_fts_insert', 'repos_fts_update', 'repos_fts_delete')",
        )
        .await,
        3,
        "the rebuild did not leave all three FTS writers installed"
    );
    assert_eq!(
        scalar(db, "SELECT count(*) FROM repo_stars WHERE repo_id = 1").await,
        1,
        "the rebuild lost an incoming child row"
    );
    assert_eq!(
        scalar(
            db,
            "SELECT seq FROM sqlite_sequence WHERE name = 'repositories'",
        )
        .await,
        HIGH_WATER_REPO_ID,
        "the rebuild lowered the AUTOINCREMENT high-water mark"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_insert_update_and_delete_wait_for_the_namespace_rebuild() {
    let (db, _temp) = fixture("concurrent-writers").await;
    let old_schema = table_sql(&db).await;
    let old_fts = fts_rows(&db).await;

    let (reached_tx, reached_rx) = oneshot::channel();
    let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));
    let release = Arc::new(Notify::new());
    let migration_db = db.clone();
    let migration_release = release.clone();
    let migration = tokio::spawn(async move {
        let manager = SchemaManager::new(&migration_db);
        sqlite_rebuild_with_hook(&manager, Shape::NamespaceKey, move |point| {
            let reached_tx = reached_tx.clone();
            let release = migration_release.clone();
            async move {
                if point == SqliteRebuildPoint::TriggersDropped {
                    let sender = reached_tx.lock().expect("lock pause sender").take();
                    if let Some(sender) = sender {
                        sender.send(()).expect("announce paused rebuild");
                        release.notified().await;
                    }
                }
                Ok(())
            }
        })
        .await
    });

    tokio::time::timeout(Duration::from_secs(10), reached_rx)
        .await
        .expect("rebuild did not reach the dropped-trigger stage")
        .expect("rebuild dropped its pause signal");

    assert_eq!(
        table_sql(&db).await,
        old_schema,
        "a reader observed the uncommitted replacement table"
    );
    assert_eq!(
        fts_rows(&db).await,
        old_fts,
        "a reader observed an uncommitted trigger/index state"
    );

    let writer_db = db.clone();
    let source_writes = format!(
        r#"
        INSERT INTO repositories
            (id, owner_id, name, description, created_at, updated_at)
        VALUES
            (3, 1, 'inserted', 'after rebuild', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        UPDATE repositories
            SET name = 'updated', description = 'after rebuild'
            WHERE id = {SURVIVING_REPO_ID};
        DELETE FROM repositories WHERE id = {DELETED_REPO_ID};
        "#
    );
    let (attempted_tx, attempted_rx) = oneshot::channel();
    let mut writer = tokio::spawn(async move {
        attempted_tx.send(()).expect("announce source write");
        write_while_the_lock_is_held(|| {
            let writer_db = writer_db.clone();
            let source_writes = source_writes.clone();
            async move {
                let transaction = writer_db.begin().await?;
                transaction.execute_unprepared(&source_writes).await?;
                transaction.commit().await
            }
        })
        .await
    });
    attempted_rx
        .await
        .expect("writer dropped its attempt signal");
    assert_writer_stays_blocked(
        &mut writer,
        "a source writer crossed the namespace rebuild boundary after its triggers were dropped",
    )
    .await;

    release.notify_one();
    migration
        .await
        .expect("rebuild task panicked")
        .expect("namespace rebuild failed");
    writer
        .await
        .expect("writer task panicked")
        .expect("source writes failed after rebuild commit");

    assert!(table_sql(&db).await.contains("namespace_key"));
    assert_rebuild_invariants(&db).await;
}

#[tokio::test]
async fn failure_after_trigger_removal_rolls_back_and_the_same_rebuild_can_retry() {
    let (db, _temp) = fixture("rollback-retry").await;
    let old_schema = table_sql(&db).await;
    let old_fts = fts_rows(&db).await;

    let manager = SchemaManager::new(&db);
    let error = sqlite_rebuild_with_hook(&manager, Shape::NamespaceKey, |point| async move {
        if point == SqliteRebuildPoint::TriggersDropped {
            Err(sea_orm::DbErr::Custom(
                "injected failure after dropping repos_fts triggers".to_string(),
            ))
        } else {
            Ok(())
        }
    })
    .await
    .expect_err("injected rebuild failure must escape");
    assert!(error.to_string().contains("injected failure"));

    assert_eq!(table_sql(&db).await, old_schema);
    assert_eq!(fts_rows(&db).await, old_fts);
    assert_rebuild_invariants(&db).await;

    db.execute_unprepared(&format!(
        r#"
        INSERT INTO repositories
            (id, owner_id, name, description, created_at, updated_at)
        VALUES (3, 1, 'after-failure', 'inserted', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        UPDATE repositories SET name = 'still-writing' WHERE id = {SURVIVING_REPO_ID};
        DELETE FROM repositories WHERE id = {DELETED_REPO_ID};
        "#
    ))
    .await
    .expect("the rolled-back FTS triggers remain usable");
    assert_source_matches_fts(&db).await;

    sqlite_rebuild(&manager, Shape::NamespaceKey)
        .await
        .expect("the same migration can be retried after rollback");
    assert!(table_sql(&db).await.contains("namespace_key"));
    assert_rebuild_invariants(&db).await;
}
