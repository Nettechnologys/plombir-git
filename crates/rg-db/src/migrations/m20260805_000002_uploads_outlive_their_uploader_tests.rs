use sea_orm::sqlx::Executor as _;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TryGetable};
use sea_orm_migration::{MigratorTrait, SchemaManager};

use super::super::ghost_author::{Shape, SqliteRebuildPoint};
use super::REBUILD;

/// A release asset that is hard-deleted before the rebuild, so the table's
/// `AUTOINCREMENT` high-water mark sits above its highest surviving row.
const HIGH_WATER_ASSET_ID: i64 = 50;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-ghost-uploader-{label}-{}.db",
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

/// A database at the schema immediately before this migration, holding a
/// release with two assets and an issue attachment — the shapes the rebuild has
/// to carry across, including the child rows that `DROP TABLE "releases"` would
/// cascade away if the pragmas failed.
async fn fixture(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = fixture_at(&temp).await;
    (db, temp)
}

async fn fixture_at(temp: &TempDb) -> DatabaseConnection {
    let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway SQLite database");
    execute_on_every_pool_connection(&db, "SELECT name FROM sqlite_master").await;

    const NAME: &str = "m20260805_000002_uploads_outlive_their_uploader";
    let before_rebuild = crate::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == NAME)
        .unwrap_or_else(|| panic!("{NAME} must still be part of the migration list"));
    let before_rebuild = u32::try_from(before_rebuild).expect("migration index fits in u32");
    crate::migrations::Migrator::up(&db, Some(before_rebuild))
        .await
        .expect("migrate to the schema immediately before the rebuild");

    db.execute_unprepared(&format!(
        r#"
        INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
        VALUES
            (1, 'ghost-host', 'ghost-host@example.invalid', '',
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            (2, 'ghost-guest', 'ghost-guest@example.invalid', '',
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES (1, 1, 'shared', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO releases
            (id, repo_id, tag_name, target_commitish, title, author_id, created_at, updated_at)
        VALUES (1, 1, 'v1.0.0', 'main', 'First', 2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO release_assets
            (id, release_id, filename, size, content_type, download_count, uploader_id, created_at)
        VALUES
            (1, 1, 'binary.tar.gz', 3, 'application/gzip', 0, 2, CURRENT_TIMESTAMP),
            (2, 1, 'checksums.txt', 4, 'text/plain', 0, 1, CURRENT_TIMESTAMP),
            ({HIGH_WATER_ASSET_ID}, 1, 'gone.bin', 1, 'application/octet-stream', 0, 2,
             CURRENT_TIMESTAMP);
        INSERT INTO issues (id, repo_id, number, title, author_id, created_at, updated_at)
        VALUES (1, 1, 1, 'A bug', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO attachments
            (id, uuid, repo_id, uploader_id, issue_id, filename, blob_key, content_type, size,
             download_count, created_at)
        VALUES (1, 'a1b2c3', 1, 2, 1, 'trace.log', 'attachments/1/a1b2c3/trace.log',
                'text/plain', 5, 0, CURRENT_TIMESTAMP);
        DELETE FROM release_assets WHERE id = {HIGH_WATER_ASSET_ID};
        "#
    ))
    .await
    .expect("seed the release, its assets and an attachment");

    assert_eq!(
        scalar(
            &db,
            "SELECT seq FROM sqlite_sequence WHERE name = 'release_assets'",
        )
        .await,
        HIGH_WATER_ASSET_ID
    );
    db
}

/// Hold every physical SQLite connection at once so the pool cannot hand the
/// same one back repeatedly, then run `sql` on each of them.
async fn execute_on_every_pool_connection(db: &DatabaseConnection, sql: &str) {
    let pool = db.get_sqlite_connection_pool();
    let max_connections = pool.options().get_max_connections();
    let mut connections = Vec::with_capacity(max_connections as usize);
    for _ in 0..max_connections {
        connections.push(
            pool.acquire()
                .await
                .expect("acquire every SQLite pool connection"),
        );
    }
    for connection in &mut connections {
        (&mut **connection)
            .execute(sql)
            .await
            .expect("statement uses the current SQLite schema");
    }
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

async fn table_sql(db: &DatabaseConnection, table: &str) -> String {
    db.query_one(Statement::from_string(
        DatabaseBackend::Sqlite,
        format!("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = '{table}'"),
    ))
    .await
    .expect("read table schema")
    .unwrap_or_else(|| panic!("{table} exists"))
    .try_get::<String>("", "sql")
    .expect("decode table schema")
}

/// Everything the rebuild must not lose, whichever shape it produced.
async fn assert_rebuild_invariants(db: &DatabaseConnection) {
    assert_eq!(
        scalar(db, "SELECT count(*) FROM releases WHERE id = 1").await,
        1,
        "the rebuild lost the release row"
    );
    assert_eq!(
        scalar(db, "SELECT count(*) FROM release_assets").await,
        2,
        "the rebuild lost release assets — a `DROP TABLE releases` cascaded into its children"
    );
    assert_eq!(
        scalar(db, "SELECT count(*) FROM attachments WHERE id = 1").await,
        1,
        "the rebuild lost the attachment row"
    );
    assert_eq!(
        scalar(
            db,
            "SELECT seq FROM sqlite_sequence WHERE name = 'release_assets'",
        )
        .await,
        HIGH_WATER_ASSET_ID,
        "the rebuild lowered the AUTOINCREMENT high-water mark, so a new asset would reuse an \
         id that is part of a previous asset's blob key"
    );
    assert_eq!(
        scalar(
            db,
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN \
             ('idx_releases_repo_id', 'idx_release_assets_release_id', \
              'idx_attachments_blob_key', 'idx_attachments_repo', 'idx_attachments_issue', \
              'idx_attachments_pr', 'idx_attachments_issue_comment', \
              'idx_attachments_review_comment')",
        )
        .await,
        8,
        "the rebuild did not restore every named index"
    );
    assert_eq!(
        scalar(db, "SELECT count(*) FROM pragma_foreign_key_check").await,
        0,
        "the rebuild left a dangling foreign key reference behind"
    );
}

/// The rule the migration exists to install, read back off the live schema.
async fn assert_shape(db: &DatabaseConnection, shape: Shape) {
    let (expected_action, expected_notnull) = match shape {
        Shape::Ghost => ("SET NULL", 0),
        Shape::Owned => ("CASCADE", 1),
    };
    for &(table, column) in REBUILD.columns {
        let sql = table_sql(db, table).await;
        let normalized = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let needle = format!(r#"FOREIGN KEY ("{column}") REFERENCES "users" ("id") ON DELETE "#);
        let rest = normalized
            .split(&needle)
            .nth(1)
            .unwrap_or_else(|| panic!("{table} has no users foreign key on {column}: {sql}"));
        assert!(
            rest.starts_with(expected_action),
            "{table}.{column} carries the wrong ON DELETE action: {sql}"
        );
        assert_eq!(
            scalar(
                db,
                &format!(
                    "SELECT \"notnull\" FROM pragma_table_info('{table}') WHERE name = '{column}'"
                ),
            )
            .await,
            expected_notnull,
            "{table}.{column} carries the wrong nullability"
        );
    }
}

/// The whole point: with the account gone, the file it left in somebody else's
/// repository is still there, and the row that names its bytes is still
/// readable.
#[tokio::test]
async fn deleting_the_uploader_ghosts_the_row_instead_of_destroying_it() {
    let (db, _temp) = fixture("ghosted-uploader").await;
    let manager = SchemaManager::new(&db);
    REBUILD
        .sqlite(&manager, Shape::Ghost)
        .await
        .expect("rebuild the upload tables");
    assert_shape(&db, Shape::Ghost).await;
    assert_rebuild_invariants(&db).await;

    db.execute_unprepared("DELETE FROM users WHERE id = 2")
        .await
        .expect("delete the account that uploaded into somebody else's repository");

    assert_eq!(
        scalar(&db, "SELECT count(*) FROM releases WHERE id = 1").await,
        1,
        "the release of a live repository died with the account that published it"
    );
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM release_assets").await,
        2,
        "release assets died with the account that published their release"
    );
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM attachments WHERE id = 1").await,
        1,
        "the attachment died with the account that uploaded it"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM releases WHERE id = 1 AND author_id IS NULL",
        )
        .await,
        1,
        "the departed author was not ghosted"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM release_assets WHERE id = 1 AND uploader_id IS NULL",
        )
        .await,
        1,
        "the departed uploader was not ghosted"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM release_assets WHERE id = 2 AND uploader_id = 1",
        )
        .await,
        1,
        "an asset uploaded by somebody else lost its uploader"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM attachments WHERE id = 1 AND uploader_id IS NULL",
        )
        .await,
        1,
        "the attachment's departed uploader was not ghosted"
    );
}

/// A table rebuild invalidates SQLite's per-connection schema state. Every
/// already-open pool connection must be usable on its first command after the
/// replacement tables commit; a retry only hides the false `no such table`.
#[tokio::test]
async fn every_prepared_pool_connection_uses_the_rebuilt_schema_immediately() {
    let (db, _temp) = fixture("pool-schema-refresh").await;
    db.execute_unprepared(
        "WITH RECURSIVE ids(id) AS (SELECT 100 UNION ALL SELECT id + 1 FROM ids WHERE id < 119) \
         INSERT INTO users (id, username, email, password_hash, created_at, updated_at) \
         SELECT id, 'pool-user-' || id, 'pool-user-' || id || '@example.invalid', '', \
                CURRENT_TIMESTAMP, CURRENT_TIMESTAMP FROM ids",
    )
    .await
    .expect("seed twenty users whose deletes exercise foreign-key schema lookup");
    execute_on_every_pool_connection(&db, "SELECT count(*) FROM releases").await;

    REBUILD
        .sqlite(&SchemaManager::new(&db), Shape::Ghost)
        .await
        .expect("rebuild the upload tables");

    let pool = db.get_sqlite_connection_pool();
    let max_connections = pool.options().get_max_connections();
    let mut connections = Vec::with_capacity(max_connections as usize);
    for _ in 0..max_connections {
        connections.push(
            pool.acquire()
                .await
                .expect("acquire every post-rebuild SQLite pool connection"),
        );
    }
    for (connection_index, connection) in connections.iter_mut().enumerate() {
        for offset in 0..5 {
            let id = 100 + connection_index * 5 + offset;
            (&mut **connection)
                .execute(format!("DELETE FROM users WHERE id = {id}").as_str())
                .await
                .expect("every pooled connection uses the rebuilt schema immediately");
        }
    }
}

/// A different process cannot refresh the server's already-open pool, so the
/// supported file-backed SQLite protocol is deliberately offline. The old
/// four-connection pool excludes the migrator; after it stops, a one-connection
/// migration pool rebuilds the tables and a freshly-started four-connection
/// server performs twenty real deletes without a stale-schema retry.
#[tokio::test]
async fn offline_process_lease_keeps_a_separate_migration_out_of_a_live_server_pool() {
    let temp = TempDb::new("offline-process-lease");
    let server_guard = crate::sqlite_process_guard::acquire_server(&temp.url())
        .unwrap()
        .unwrap();
    let server_db = fixture_at(&temp).await;
    server_db
        .execute_unprepared(
            "WITH RECURSIVE ids(id) AS (SELECT 100 UNION ALL SELECT id + 1 FROM ids WHERE id < 119) \
             INSERT INTO users (id, username, email, password_hash, created_at, updated_at) \
             SELECT id, 'offline-user-' || id, 'offline-user-' || id || '@example.invalid', '', \
                    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP FROM ids",
        )
        .await
        .expect("seed twenty users before the offline rebuild");
    execute_on_every_pool_connection(&server_db, "SELECT count(*) FROM releases").await;

    let refusal = format!(
        "{:#}",
        crate::sqlite_process_guard::acquire_migration(&temp.url())
            .expect_err("a separate migration must not cross the live-server lease")
    );
    assert!(refusal.contains("server to be stopped"), "{refusal}");

    server_db.close().await.expect("close the old server pool");
    drop(server_guard);

    let migration_guard = crate::sqlite_process_guard::acquire_migration(&temp.url())
        .unwrap()
        .unwrap();
    let migration_db =
        crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
            .await
            .expect("open the standalone migration pool");
    REBUILD
        .sqlite(&SchemaManager::new(&migration_db), Shape::Ghost)
        .await
        .expect("rebuild through the standalone migration pool");
    migration_db
        .close()
        .await
        .expect("close the standalone migration pool");
    drop(migration_guard);

    let _restarted_server_guard = crate::sqlite_process_guard::acquire_server(&temp.url())
        .unwrap()
        .unwrap();
    let restarted = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("open a fresh server pool after the migration");
    execute_on_every_pool_connection(&restarted, "SELECT count(*) FROM releases").await;

    let pool = restarted.get_sqlite_connection_pool();
    let mut connections = Vec::with_capacity(4);
    for _ in 0..4 {
        connections.push(
            pool.acquire()
                .await
                .expect("acquire each restarted connection"),
        );
    }
    for (connection_index, connection) in connections.iter_mut().enumerate() {
        for offset in 0..5 {
            let id = 100 + connection_index * 5 + offset;
            (&mut **connection)
                .execute(format!("DELETE FROM users WHERE id = {id}").as_str())
                .await
                .expect("the restarted server pool must use the rebuilt schema immediately");
        }
    }
}

/// A failure part-way through leaves all three tables, their rows and their
/// sequences exactly as they were — and the same migration can simply be run
/// again.
#[tokio::test]
async fn failure_mid_rebuild_rolls_back_and_the_same_rebuild_can_retry() {
    let (db, _temp) = fixture("rollback-retry").await;
    let old_releases = table_sql(&db, "releases").await;
    let manager = SchemaManager::new(&db);

    let error = REBUILD
        .sqlite_with_hook(&manager, Shape::Ghost, |point| async move {
            if point == SqliteRebuildPoint::TableRebuilt("releases") {
                Err(sea_orm::DbErr::Custom(
                    "injected failure after rebuilding releases".to_string(),
                ))
            } else {
                Ok(())
            }
        })
        .await
        .expect_err("injected rebuild failure must escape");
    assert!(error.to_string().contains("injected failure"));

    assert_eq!(
        table_sql(&db, "releases").await,
        old_releases,
        "the rolled-back rebuild left the replacement `releases` table behind"
    );
    assert_shape(&db, Shape::Owned).await;
    assert_rebuild_invariants(&db).await;

    db.execute_unprepared(
        "INSERT INTO release_assets
             (release_id, filename, size, content_type, download_count, uploader_id, created_at)
         VALUES (1, 'after-failure.bin', 1, 'application/octet-stream', 0, 2, CURRENT_TIMESTAMP)",
    )
    .await
    .expect("the rolled-back tables remain writable");

    REBUILD
        .sqlite(&manager, Shape::Ghost)
        .await
        .expect("the same migration can be retried after a rollback");
    assert_shape(&db, Shape::Ghost).await;
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM release_assets").await,
        3,
        "the retried rebuild lost the row written between the two attempts"
    );
}

/// `down` is a real reversal, and it refuses rather than deleting other
/// people's files to make `NOT NULL` fit again.
#[tokio::test]
async fn down_restores_the_cascade_and_refuses_once_a_row_is_ghosted() {
    let (db, _temp) = fixture("reversal").await;
    let manager = SchemaManager::new(&db);
    REBUILD
        .sqlite(&manager, Shape::Ghost)
        .await
        .expect("rebuild the upload tables");

    REBUILD
        .sqlite(&manager, Shape::Owned)
        .await
        .expect("reverse a rebuild nothing has ghosted yet");
    assert_shape(&db, Shape::Owned).await;
    assert_rebuild_invariants(&db).await;

    REBUILD
        .sqlite(&manager, Shape::Ghost)
        .await
        .expect("rebuild the upload tables again");
    db.execute_unprepared("DELETE FROM users WHERE id = 2")
        .await
        .expect("ghost the uploader");

    let error = REBUILD
        .sqlite(&manager, Shape::Owned)
        .await
        .expect_err("a ghosted row has no uploader to restore, so the reversal must fail");
    assert!(
        error.to_string().contains("NOT NULL"),
        "the reversal failed for the wrong reason: {error}"
    );
    assert_shape(&db, Shape::Ghost).await;
    assert_rebuild_invariants(&db).await;
}
