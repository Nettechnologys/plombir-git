use std::future::Future;

use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use sea_orm_migration::prelude::{SchemaManager, SchemaManagerConnection};

/// Execute a SQLite metadata-FTS upgrade under one write transaction.
///
/// SeaORM 1.1 only wraps PostgreSQL migrations in a transaction.  Keeping this
/// boundary here prevents a trigger writer from observing (or writing into) a
/// cleared, half-rebuilt FTS table on SQLite.  `after_statement` is a test seam:
/// production passes a no-op, while concurrency and fault-injection tests pause
/// after the destructive stages without depending on scheduler timing.
pub(crate) async fn sqlite_fts_maintenance(
    manager: &SchemaManager<'_>,
    statements: &[String],
) -> Result<(), DbErr> {
    sqlite_fts_maintenance_with_hook(manager, statements, |_| async { Ok(()) }).await
}

pub(crate) async fn sqlite_fts_maintenance_with_hook<F, Fut>(
    manager: &SchemaManager<'_>,
    statements: &[String],
    after_statement: F,
) -> Result<(), DbErr>
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Result<(), DbErr>>,
{
    let transaction = manager.get_connection().begin().await?;
    let result = async {
        for (index, statement) in statements.iter().enumerate() {
            transaction.execute_unprepared(statement).await?;
            after_statement(index).await?;
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => transaction.commit().await,
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(DbErr::Custom(format!(
                "SQLite FTS migration failed ({error}); rollback also failed ({rollback_error})"
            ))),
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MySqlFtsMaintenancePoint {
    Locked,
    AfterDdl(usize),
    AfterReconcile(usize),
}

/// Repair MySQL FTS triggers and rows while source writers are excluded.
///
/// MySQL trigger DDL implicitly commits, so a normal transaction cannot close
/// the drop/create window.  `LOCK TABLES ... WRITE` is session-scoped and is not
/// released by those implicit DDL commits.  We therefore pin one SQLx pool
/// connection, retain the table locks across the DDL, and commit all row
/// reconciliation statements together before unlocking.  Every replacement
/// plan installs an idempotent guard trigger before dropping the canonical one,
/// so a DDL error still leaves a writer after the maintenance lock is released.
pub(crate) async fn mysql_fts_maintenance(
    manager: &SchemaManager<'_>,
    lock_tables: &str,
    ddl: &[String],
    reconcile: &[String],
) -> Result<(), DbErr> {
    mysql_fts_maintenance_with_hook(manager, lock_tables, ddl, reconcile, |_| async { Ok(()) })
        .await
}

pub(crate) async fn mysql_fts_maintenance_with_hook<F, Fut>(
    manager: &SchemaManager<'_>,
    lock_tables: &str,
    ddl: &[String],
    reconcile: &[String],
    after_step: F,
) -> Result<(), DbErr>
where
    F: Fn(MySqlFtsMaintenancePoint) -> Fut,
    Fut: Future<Output = Result<(), DbErr>>,
{
    let pool = match manager.get_connection() {
        SchemaManagerConnection::Connection(db) => db.get_mysql_connection_pool(),
        SchemaManagerConnection::Transaction(_) => {
            return Err(DbErr::Custom(
                "MySQL FTS maintenance requires a non-transactional migration connection"
                    .to_string(),
            ));
        }
    };
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| mysql_error("acquire a dedicated connection", error))?;

    if let Err(error) = mysql_execute(&mut connection, "SET autocommit = 0").await {
        connection.close_on_drop();
        return Err(error);
    }

    let lock_sql = format!("LOCK TABLES {lock_tables}");
    if let Err(error) = mysql_execute(&mut connection, &lock_sql).await {
        let reset = mysql_execute(&mut connection, "SET autocommit = 1").await;
        if let Err(reset_error) = reset {
            connection.close_on_drop();
            return Err(combine_mysql_errors(error, vec![reset_error]));
        }
        return Err(error);
    }

    let work_result = async {
        after_step(MySqlFtsMaintenancePoint::Locked).await?;
        for (index, statement) in ddl.iter().enumerate() {
            mysql_execute(&mut connection, statement).await?;
            after_step(MySqlFtsMaintenancePoint::AfterDdl(index)).await?;
        }
        for (index, statement) in reconcile.iter().enumerate() {
            mysql_execute(&mut connection, statement).await?;
            after_step(MySqlFtsMaintenancePoint::AfterReconcile(index)).await?;
        }
        mysql_execute(&mut connection, "COMMIT").await
    }
    .await;

    let mut cleanup_errors = Vec::new();
    if work_result.is_err() {
        if let Err(error) = mysql_execute(&mut connection, "ROLLBACK").await {
            cleanup_errors.push(error);
        }
    }
    if let Err(error) = mysql_execute(&mut connection, "UNLOCK TABLES").await {
        cleanup_errors.push(error);
    }
    if let Err(error) = mysql_execute(&mut connection, "SET autocommit = 1").await {
        cleanup_errors.push(error);
    }

    match (work_result, cleanup_errors.is_empty()) {
        (Ok(()), true) => Ok(()),
        (Ok(()), false) => {
            connection.close_on_drop();
            Err(combine_mysql_errors(
                DbErr::Custom("MySQL FTS maintenance cleanup failed".to_string()),
                cleanup_errors,
            ))
        }
        (Err(error), true) => Err(error),
        (Err(error), false) => {
            connection.close_on_drop();
            Err(combine_mysql_errors(error, cleanup_errors))
        }
    }
}

async fn mysql_execute(
    connection: &mut sea_orm::sqlx::pool::PoolConnection<sea_orm::sqlx::MySql>,
    statement: &str,
) -> Result<(), DbErr> {
    use sea_orm::sqlx::Executor as _;

    (&mut **connection)
        .execute(statement)
        .await
        .map(|_| ())
        .map_err(|error| mysql_error("execute maintenance SQL", error))
}

fn mysql_error(action: &str, error: sea_orm::sqlx::Error) -> DbErr {
    DbErr::Custom(format!("MySQL FTS maintenance: {action}: {error}"))
}

fn combine_mysql_errors(primary: DbErr, cleanup: Vec<DbErr>) -> DbErr {
    let cleanup = cleanup
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    DbErr::Custom(format!("{primary}; cleanup also failed: {cleanup}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use sea_orm::{
        ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait,
        TryGetable,
    };
    use sea_orm_migration::SchemaManager;
    use tokio::sync::{oneshot, Notify};

    use super::{
        mysql_fts_maintenance_with_hook, sqlite_fts_maintenance_with_hook, MySqlFtsMaintenancePoint,
    };
    use crate::migrations::{
        m20260508_000005_create_fts5_indexes as create_fts,
        m20260511_000003_fix_fts5_triggers as fix_triggers,
        m20260804_000006_repo_fts_soft_delete as repo_soft_delete,
    };
    use crate::test_support::{assert_writer_stays_blocked, write_while_the_lock_is_held};

    #[derive(Debug, Eq, PartialEq)]
    struct Snapshot {
        repositories: Vec<(i64, String, String)>,
        issues: Vec<(i64, String, String)>,
        wiki_pages: Vec<(i64, String, String)>,
    }

    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new(label: &str) -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "plombir-git-fts-migration-{label}-{}.db",
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
        db.execute_unprepared(
            r#"
            CREATE TABLE repositories (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                deleted_at TEXT
            );
            CREATE TABLE issues (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                body TEXT
            );
            CREATE TABLE wiki_pages (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                content TEXT
            );
            INSERT INTO repositories VALUES
                (1, 'repo-one', 'repo body one', NULL),
                (2, 'repo-two', 'repo body two', NULL),
                (3, 'repo-three', 'repo body three', NULL);
            INSERT INTO issues VALUES
                (1, 'issue-one', 'issue body one'),
                (2, 'issue-two', 'issue body two'),
                (3, 'issue-three', 'issue body three');
            INSERT INTO wiki_pages VALUES
                (1, 'wiki-one', 'wiki body one'),
                (2, 'wiki-two', 'wiki body two'),
                (3, 'wiki-three', 'wiki body three');
            "#,
        )
        .await
        .expect("create source tables");
        (db, temp)
    }

    async fn rows(
        db: &DatabaseConnection,
        sql: &str,
    ) -> Result<Vec<(i64, String, String)>, sea_orm::DbErr> {
        Ok(db
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                sql.to_string(),
            ))
            .await?
            .into_iter()
            .map(|row| {
                (
                    i64::try_get_by_index(&row, 0).expect("row id"),
                    String::try_get_by_index(&row, 1).expect("first text column"),
                    String::try_get_by_index(&row, 2).expect("second text column"),
                )
            })
            .collect())
    }

    async fn fts_snapshot(db: &DatabaseConnection) -> Result<Snapshot, sea_orm::DbErr> {
        Ok(Snapshot {
            repositories: rows(
                db,
                "SELECT rowid, name, description FROM repos_fts ORDER BY rowid",
            )
            .await?,
            issues: rows(
                db,
                "SELECT rowid, title, body FROM issues_fts ORDER BY rowid",
            )
            .await?,
            wiki_pages: rows(
                db,
                "SELECT rowid, title, content FROM wiki_pages_fts ORDER BY rowid",
            )
            .await?,
        })
    }

    async fn source_snapshot(db: &DatabaseConnection) -> Snapshot {
        Snapshot {
            repositories: rows(
                db,
                "SELECT id, name, COALESCE(description, '') FROM repositories \
                 WHERE deleted_at IS NULL ORDER BY id",
            )
            .await
            .expect("read repository sources"),
            issues: rows(
                db,
                "SELECT id, title, COALESCE(body, '') FROM issues ORDER BY id",
            )
            .await
            .expect("read issue sources"),
            wiki_pages: rows(
                db,
                "SELECT id, title, COALESCE(content, '') FROM wiki_pages ORDER BY id",
            )
            .await
            .expect("read wiki sources"),
        }
    }

    async fn seed_delete_candidate(db: &DatabaseConnection, id: i64) {
        db.execute_unprepared(&format!(
            "INSERT INTO repositories VALUES ({id}, 'delete-repo-{id}', 'delete repo', NULL); \
             INSERT INTO issues VALUES ({id}, 'delete-issue-{id}', 'delete issue'); \
             INSERT INTO wiki_pages VALUES ({id}, 'delete-wiki-{id}', 'delete wiki');"
        ))
        .await
        .expect("seed rows that the concurrent writer deletes");
    }

    async fn run_paused_upgrade(
        db: &DatabaseConnection,
        statements: Vec<String>,
        pause_after: usize,
        run: i64,
        fts_existed_before: bool,
    ) {
        let delete_id = 100 + run;
        let insert_id = 200 + run;
        seed_delete_candidate(db, delete_id).await;
        let before = if fts_existed_before {
            Some(
                fts_snapshot(db)
                    .await
                    .expect("read pre-upgrade FTS snapshot"),
            )
        } else {
            None
        };

        let (reached_tx, reached_rx) = oneshot::channel();
        let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));
        let release = Arc::new(Notify::new());
        let migration_db = db.clone();
        let migration_release = release.clone();
        let migration = tokio::spawn(async move {
            let manager = SchemaManager::new(&migration_db);
            sqlite_fts_maintenance_with_hook(&manager, &statements, move |index| {
                let reached_tx = reached_tx.clone();
                let release = migration_release.clone();
                async move {
                    if index == pause_after {
                        let sender = reached_tx.lock().expect("lock pause sender").take();
                        if let Some(sender) = sender {
                            sender.send(()).expect("announce paused migration");
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
            .expect("migration did not reach the destructive stage")
            .expect("migration dropped its pause signal");

        match before {
            Some(before) => assert_eq!(
                fts_snapshot(db)
                    .await
                    .expect("read old committed FTS snapshot"),
                before,
                "a reader observed the migration's uncommitted partial index"
            ),
            None => assert!(
                fts_snapshot(db).await.is_err(),
                "the first FTS migration published its tables before commit"
            ),
        }

        let writer_db = db.clone();
        let source_writes = format!(
            "INSERT INTO repositories VALUES ({insert_id}, 'inserted-repo-{run}', 'inserted repo', NULL); \
             UPDATE repositories SET name = 'updated-repo-{run}' WHERE id = 2; \
             DELETE FROM repositories WHERE id = {delete_id}; \
             INSERT INTO issues VALUES ({insert_id}, 'inserted-issue-{run}', 'inserted issue'); \
             UPDATE issues SET title = 'updated-issue-{run}' WHERE id = 2; \
             DELETE FROM issues WHERE id = {delete_id}; \
             INSERT INTO wiki_pages VALUES ({insert_id}, 'inserted-wiki-{run}', 'inserted wiki'); \
             UPDATE wiki_pages SET title = 'updated-wiki-{run}' WHERE id = 2; \
             DELETE FROM wiki_pages WHERE id = {delete_id};"
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
            "a live source writer crossed the SQLite migration boundary",
        )
        .await;

        release.notify_one();
        migration
            .await
            .expect("migration task panicked")
            .expect("migration failed");
        writer
            .await
            .expect("writer task panicked")
            .expect("source writes failed after migration commit");
        assert_eq!(
            fts_snapshot(db).await.expect("read final FTS snapshot"),
            source_snapshot(db).await,
            "the committed FTS snapshot diverged from its source rows"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn every_sqlite_metadata_fts_upgrade_excludes_live_insert_update_and_delete() {
        let (db, _temp) = fixture("writers").await;

        run_paused_upgrade(&db, create_fts::sqlite_stmts(), 2, 1, false).await;
        run_paused_upgrade(&db, fix_triggers::sqlite_stmts(), 1, 2, true).await;

        db.execute_unprepared(
            "UPDATE repositories SET deleted_at = 'deleted before repair' WHERE id = 1",
        )
        .await
        .expect("soft-delete a repository under the old trigger");
        run_paused_upgrade(&db, repo_soft_delete::sqlite_statements(), 1, 3, true).await;
    }

    #[tokio::test]
    async fn a_mid_rebuild_failure_rolls_back_the_cleared_sqlite_indexes() {
        let (db, _temp) = fixture("rollback").await;
        let manager = SchemaManager::new(&db);
        super::sqlite_fts_maintenance(&manager, &create_fts::sqlite_stmts())
            .await
            .expect("create initial FTS indexes");
        let before = fts_snapshot(&db).await.expect("read complete FTS snapshot");

        let error = sqlite_fts_maintenance_with_hook(
            &manager,
            &fix_triggers::sqlite_stmts(),
            |index| async move {
                if index == 1 {
                    Err(sea_orm::DbErr::Custom(
                        "injected failure after clearing FTS tables".to_string(),
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .await
        .expect_err("injected migration failure must escape");
        assert!(error.to_string().contains("injected failure"));
        assert_eq!(
            fts_snapshot(&db)
                .await
                .expect("read rolled-back FTS snapshot"),
            before
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable MySQL 8.4"]
    async fn mysql_fts_maintenance_holds_an_offline_boundary_and_rolls_back_row_repairs() {
        let database_url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
            .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
        assert!(
            database_url.starts_with("mysql://"),
            "this proof exercises MySQL table-lock and implicit-DDL-commit semantics"
        );
        let db = crate::connect_with_pool(&database_url, crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to disposable MySQL database");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let source = format!("fts_migration_source_{}", &suffix[..12]);
        let index = format!("fts_migration_index_{}", &suffix[..12]);
        let triggers = [
            (
                format!("fts_migration_ai_{}", &suffix[..12]),
                format!("fts_migration_gi_{}", &suffix[..12]),
                format!(
                    "AFTER INSERT ON {source} FOR EACH ROW INSERT INTO {index}(rowid, name) \
                     VALUES (NEW.id, NEW.name) ON DUPLICATE KEY UPDATE name = NEW.name"
                ),
            ),
            (
                format!("fts_migration_au_{}", &suffix[..12]),
                format!("fts_migration_gu_{}", &suffix[..12]),
                format!(
                    "AFTER UPDATE ON {source} FOR EACH ROW INSERT INTO {index}(rowid, name) \
                     VALUES (NEW.id, NEW.name) ON DUPLICATE KEY UPDATE name = NEW.name"
                ),
            ),
            (
                format!("fts_migration_ad_{}", &suffix[..12]),
                format!("fts_migration_gd_{}", &suffix[..12]),
                format!(
                    "AFTER DELETE ON {source} FOR EACH ROW DELETE FROM {index} WHERE rowid = OLD.id"
                ),
            ),
        ];

        for statement in [
            format!(
                "CREATE TABLE {source} (id BIGINT PRIMARY KEY, name VARCHAR(255) NOT NULL) ENGINE=InnoDB"
            ),
            format!(
                "CREATE TABLE {index} (rowid BIGINT PRIMARY KEY, name VARCHAR(255) NOT NULL, FULLTEXT(name)) ENGINE=InnoDB"
            ),
        ] {
            db.execute_unprepared(&statement)
                .await
                .expect("create MySQL maintenance fixture");
        }
        for (canonical, _, definition) in &triggers {
            db.execute_unprepared(&format!("CREATE TRIGGER {canonical} {definition}"))
                .await
                .expect("create MySQL maintenance trigger");
        }
        for statement in [
            format!("INSERT INTO {source} VALUES (1, 'old-one'), (3, 'delete-three')"),
            format!("UPDATE {index} SET name = 'stale-one' WHERE rowid = 1"),
        ] {
            db.execute_unprepared(&statement)
                .await
                .expect("seed MySQL maintenance fixture");
        }

        let ddl = triggers
            .iter()
            .flat_map(|(canonical, guard, definition)| {
                [
                    format!("CREATE TRIGGER IF NOT EXISTS {guard} {definition}"),
                    format!("DROP TRIGGER IF EXISTS {canonical}"),
                    format!("CREATE TRIGGER {canonical} {definition}"),
                    format!("DROP TRIGGER IF EXISTS {guard}"),
                ]
            })
            .collect::<Vec<_>>();
        let reconcile = vec![
            format!(
                "DELETE FROM {index} WHERE NOT EXISTS \
                 (SELECT 1 FROM {source} WHERE {source}.id = {index}.rowid)"
            ),
            format!(
                "INSERT INTO {index}(rowid, name) SELECT id, name FROM {source} \
                 ON DUPLICATE KEY UPDATE name = VALUES(name)"
            ),
        ];
        let locks = format!("{source} WRITE, {index} WRITE");
        let (reached_tx, reached_rx) = oneshot::channel();
        let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));
        let release = Arc::new(Notify::new());
        let maintenance_db = db.clone();
        let maintenance_locks = locks.clone();
        let maintenance_ddl = ddl.clone();
        let maintenance_reconcile = reconcile.clone();
        let maintenance_release = release.clone();
        let maintenance = tokio::spawn(async move {
            let manager = SchemaManager::new(&maintenance_db);
            mysql_fts_maintenance_with_hook(
                &manager,
                &maintenance_locks,
                &maintenance_ddl,
                &maintenance_reconcile,
                move |point| {
                    let reached_tx = reached_tx.clone();
                    let release = maintenance_release.clone();
                    async move {
                        if point == MySqlFtsMaintenancePoint::Locked {
                            let sender = reached_tx.lock().expect("lock pause sender").take();
                            if let Some(sender) = sender {
                                sender.send(()).expect("announce MySQL table lock");
                                release.notified().await;
                            }
                        }
                        Ok(())
                    }
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), reached_rx)
            .await
            .expect("maintenance did not acquire table locks")
            .expect("maintenance dropped its lock signal");

        let writer_db = db.clone();
        let writer_source = source.clone();
        let (attempted_tx, attempted_rx) = oneshot::channel();
        let mut writer = tokio::spawn(async move {
            attempted_tx.send(()).expect("announce MySQL source write");
            for statement in [
                format!("INSERT INTO {writer_source} VALUES (2, 'insert-two')"),
                format!("UPDATE {writer_source} SET name = 'updated-one' WHERE id = 1"),
                format!("DELETE FROM {writer_source} WHERE id = 3"),
            ] {
                writer_db
                    .execute(Statement::from_string(DatabaseBackend::MySql, statement))
                    .await?;
            }
            Ok::<_, sea_orm::DbErr>(())
        });
        attempted_rx
            .await
            .expect("writer dropped its attempt signal");
        assert_writer_stays_blocked(
            &mut writer,
            "a MySQL source writer crossed the exclusive maintenance window",
        )
        .await;

        release.notify_one();
        maintenance
            .await
            .expect("maintenance task panicked")
            .expect("MySQL maintenance failed");
        writer
            .await
            .expect("writer task panicked")
            .expect("writer failed after maintenance unlocked");
        let exact = rows_for_backend(
            &db,
            DatabaseBackend::MySql,
            &format!("SELECT rowid, name, '' FROM {index} ORDER BY rowid"),
        )
        .await
        .expect("read MySQL FTS rows");
        let source_rows = rows_for_backend(
            &db,
            DatabaseBackend::MySql,
            &format!("SELECT id, name, '' FROM {source} ORDER BY id"),
        )
        .await
        .expect("read MySQL source rows");
        assert_eq!(exact, source_rows);

        let before_failure = exact;
        let manager = SchemaManager::new(&db);
        let error = mysql_fts_maintenance_with_hook(
            &manager,
            &locks,
            &[],
            &reconcile,
            |point| async move {
                if point == MySqlFtsMaintenancePoint::AfterReconcile(0) {
                    Err(sea_orm::DbErr::Custom(
                        "injected failure after MySQL FTS delete".to_string(),
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .await
        .expect_err("injected MySQL maintenance failure must escape");
        assert!(error.to_string().contains("injected failure"));
        assert_eq!(
            rows_for_backend(
                &db,
                DatabaseBackend::MySql,
                &format!("SELECT rowid, name, '' FROM {index} ORDER BY rowid"),
            )
            .await
            .expect("read rolled-back MySQL FTS rows"),
            before_failure
        );

        let mut cleanup = triggers
            .iter()
            .flat_map(|(canonical, guard, _)| {
                [
                    format!("DROP TRIGGER IF EXISTS {canonical}"),
                    format!("DROP TRIGGER IF EXISTS {guard}"),
                ]
            })
            .collect::<Vec<_>>();
        cleanup.extend([
            format!("DROP TABLE {index}"),
            format!("DROP TABLE {source}"),
        ]);
        for statement in cleanup {
            db.execute_unprepared(&statement)
                .await
                .expect("drop MySQL maintenance fixture");
        }
    }

    async fn rows_for_backend(
        db: &DatabaseConnection,
        backend: DatabaseBackend,
        sql: &str,
    ) -> Result<Vec<(i64, String, String)>, sea_orm::DbErr> {
        Ok(db
            .query_all(Statement::from_string(backend, sql.to_string()))
            .await?
            .into_iter()
            .map(|row| {
                (
                    i64::try_get_by_index(&row, 0).expect("row id"),
                    String::try_get_by_index(&row, 1).expect("first text column"),
                    String::try_get_by_index(&row, 2).expect("second text column"),
                )
            })
            .collect())
    }
}
