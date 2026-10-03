use std::future::Future;

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

const TABLE: &str = "code_fts";
#[cfg(test)]
const SEARCH_INDEX: &str = "code_fts_tsv_idx";
const REPO_INDEX: &str = "code_fts_repo_id_idx";

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260512_000001_create_code_fts"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_code_fts_with_hook(manager, |_| async { Ok(()) }).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS code_fts")
            .await?;
        Ok(())
    }
}

/// A recoverable stage of the in-place `code_fts` create/repair plan.
///
/// The hook is a test seam. Production never pauses here; tests fail after each
/// stage and prove that a retry finishes the schema without deleting rows that
/// a live `CodeIndexer` wrote before or between attempts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodeFtsCreatePoint {
    Table,
    SearchIndex,
    RepoIndex,
}

async fn ensure_code_fts_with_hook<F, Fut>(
    manager: &SchemaManager<'_>,
    after_stage: F,
) -> Result<(), DbErr>
where
    F: Fn(CodeFtsCreatePoint) -> Fut,
    Fut: Future<Output = Result<(), DbErr>>,
{
    let backend = manager.get_database_backend();
    if !manager.has_table(TABLE).await? {
        manager
            .get_connection()
            .execute_unprepared(create_table_statement(backend))
            .await?;
    }
    after_stage(CodeFtsCreatePoint::Table).await?;

    assert_compatible_table(manager).await?;
    match backend {
        DatabaseBackend::Sqlite => assert_sqlite_fts5(manager).await?,
        DatabaseBackend::Postgres => {
            manager
                .get_connection()
                .execute_unprepared(
                    "CREATE INDEX IF NOT EXISTS code_fts_tsv_idx \
                     ON code_fts USING GIN(tsv)",
                )
                .await?;
        }
        DatabaseBackend::MySql => {
            if !mysql_has_compatible_fulltext_index(manager).await? {
                manager
                    .get_connection()
                    .execute_unprepared(
                        "CREATE FULLTEXT INDEX code_fts_fulltext_idx \
                         ON code_fts(content, file_path, file_name, language)",
                    )
                    .await?;
            }
        }
    }
    after_stage(CodeFtsCreatePoint::SearchIndex).await?;

    if backend != DatabaseBackend::Sqlite && !manager.has_index(TABLE, REPO_INDEX).await? {
        manager
            .get_connection()
            .execute_unprepared("CREATE INDEX code_fts_repo_id_idx ON code_fts(repo_id)")
            .await?;
    }
    after_stage(CodeFtsCreatePoint::RepoIndex).await?;

    Ok(())
}

/// This migration creates the index for the first time. If a compatible table
/// is already present (a retry, a restored database, or a deployment that had
/// the old migration), it is the only complete copy of data sourced from Git
/// objects outside the database. Keep it in place and only finish missing
/// indexes; never clear or replace it from a migration that cannot backfill it.
fn create_table_statement(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Sqlite => {
            "CREATE VIRTUAL TABLE IF NOT EXISTS code_fts USING fts5(\
                repo_id, file_path, file_name, content, language\
            )"
        }
        DatabaseBackend::Postgres => {
            "CREATE TABLE IF NOT EXISTS code_fts (\
                id BIGSERIAL PRIMARY KEY, \
                repo_id BIGINT NOT NULL, \
                file_path TEXT, \
                file_name TEXT, \
                content TEXT, \
                language TEXT, \
                tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', \
                    coalesce(content,'') || ' ' || coalesce(file_path,'') || ' ' || \
                    coalesce(file_name,'') || ' ' || coalesce(language,''))) STORED\
            )"
        }
        DatabaseBackend::MySql => {
            "CREATE TABLE IF NOT EXISTS code_fts (\
                id BIGINT AUTO_INCREMENT PRIMARY KEY, \
                repo_id BIGINT NOT NULL, \
                file_path TEXT, \
                file_name TEXT, \
                content LONGTEXT, \
                language TEXT, \
                FULLTEXT INDEX code_fts_fulltext_idx(\
                    content, file_path, file_name, language\
                )\
            ) ENGINE=InnoDB"
        }
    }
}

async fn assert_compatible_table(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    let mut required = vec!["repo_id", "file_path", "file_name", "content", "language"];
    if backend != DatabaseBackend::Sqlite {
        required.push("id");
    }
    if backend == DatabaseBackend::Postgres {
        required.push("tsv");
    }

    for column in required {
        if !manager.has_column(TABLE, column).await? {
            return Err(DbErr::Custom(format!(
                "refusing to replace incompatible code_fts: required column `{column}` is missing; \
                 the existing index is preserved"
            )));
        }
    }
    Ok(())
}

async fn assert_sqlite_fts5(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let row = manager
        .get_connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'code_fts'".to_string(),
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("code_fts disappeared while validating it".to_string()))?;
    let sql: String = row.try_get("", "sql")?;
    if !sql.to_ascii_lowercase().contains("using fts5") {
        return Err(DbErr::Custom(
            "refusing to replace incompatible code_fts: the existing SQLite table is not FTS5; \
             the existing index is preserved"
                .to_string(),
        ));
    }
    Ok(())
}

async fn mysql_has_compatible_fulltext_index(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    Ok(manager
        .get_connection()
        .query_one(Statement::from_string(
            DatabaseBackend::MySql,
            "SELECT s.INDEX_NAME AS idx \
             FROM information_schema.STATISTICS s \
             WHERE s.TABLE_SCHEMA = DATABASE() \
               AND s.TABLE_NAME = 'code_fts' \
               AND s.INDEX_TYPE = 'FULLTEXT' \
             GROUP BY s.INDEX_NAME \
             HAVING GROUP_CONCAT(s.COLUMN_NAME ORDER BY s.SEQ_IN_INDEX SEPARATOR ',') = \
                    'content,file_path,file_name,language' \
             LIMIT 1"
                .to_string(),
        ))
        .await?
        .is_some())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use sea_orm::{DatabaseConnection, TryGetable, Value};
    use tokio::sync::{oneshot, Notify};

    use super::*;

    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "plombir-git-code-fts-create-{}.db",
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

    #[test]
    fn every_backend_create_plan_is_non_destructive_and_retryable() {
        for backend in [
            DatabaseBackend::Sqlite,
            DatabaseBackend::Postgres,
            DatabaseBackend::MySql,
        ] {
            let statement = create_table_statement(backend).to_ascii_uppercase();
            assert!(statement.contains("IF NOT EXISTS"));
            assert!(!statement.contains("DROP"));
            assert!(!statement.contains("DELETE"));
        }
    }

    #[tokio::test]
    async fn incompatible_existing_table_fails_closed_without_losing_its_rows() {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect to in-memory SQLite");
        db.execute_unprepared(
            "CREATE TABLE code_fts (\
                 repo_id INTEGER, file_path TEXT, file_name TEXT, content TEXT, language TEXT\
             );\
             INSERT INTO code_fts VALUES (1, 'kept.rs', 'kept.rs', 'old complete row', 'Rust')",
        )
        .await
        .expect("create incompatible live table");

        let error = Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect_err("a regular SQLite table must not be accepted as an FTS5 index");
        assert!(error.to_string().contains("not FTS5"));
        assert_eq!(rows(&db).await, vec![(1, "kept.rs".into())]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sqlite_create_preserves_live_rows_and_recovers_after_every_stage() {
        let temp = TempDb::new();
        let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to throwaway SQLite database");

        exercise_live_backend(&db).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
    async fn server_create_preserves_live_rows_and_recovers_after_every_stage() {
        let database_url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
            .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
        assert!(
            database_url.starts_with("postgres://") || database_url.starts_with("mysql://"),
            "this proof exercises PostgreSQL or MySQL create/index DDL"
        );
        let db = crate::connect_with_pool(&database_url, crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to disposable server database");

        exercise_live_backend(&db).await;
        db.execute_unprepared("DROP TABLE IF EXISTS code_fts")
            .await
            .expect("remove live code_fts fixture");
    }

    async fn exercise_live_backend(db: &DatabaseConnection) {
        db.execute_unprepared("DROP TABLE IF EXISTS code_fts")
            .await
            .expect("reset code_fts fixture");
        let manager = SchemaManager::new(db);
        ensure_code_fts_with_hook(&manager, |_| async { Ok(()) })
            .await
            .expect("create complete code_fts fixture");
        insert_row(db, 1, "old.rs").await;

        // A compatible live table is never replaced. Pause the migration after
        // it observes the table and prove that its independent writer can
        // commit while the migration is still in progress.
        let (reached_tx, reached_rx) = oneshot::channel();
        let reached_tx = Arc::new(Mutex::new(Some(reached_tx)));
        let release = Arc::new(Notify::new());
        let migration_db = db.clone();
        let migration_release = release.clone();
        let migration = tokio::spawn(async move {
            let manager = SchemaManager::new(&migration_db);
            ensure_code_fts_with_hook(&manager, move |point| {
                let reached_tx = reached_tx.clone();
                let release = migration_release.clone();
                async move {
                    if point == CodeFtsCreatePoint::Table {
                        let sender = reached_tx.lock().expect("lock pause sender").take();
                        if let Some(sender) = sender {
                            sender.send(()).expect("announce table validation boundary");
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
            .expect("migration did not reach table validation")
            .expect("migration dropped its table validation signal");
        tokio::time::timeout(Duration::from_secs(2), insert_row(db, 2, "concurrent.rs"))
            .await
            .expect("a live index writer was blocked by a non-destructive retry");
        release.notify_one();
        migration
            .await
            .expect("migration task panicked")
            .expect("migration failed");
        assert_eq!(
            rows(db).await,
            vec![(1, "old.rs".into()), (2, "concurrent.rs".into())]
        );

        // Starting from no table, inject failure after every construction
        // stage. A writer that reaches the newly-created table between attempts
        // must survive the retry; this is the state the old DROP destroyed.
        for (index, fail_at) in [
            CodeFtsCreatePoint::Table,
            CodeFtsCreatePoint::SearchIndex,
            CodeFtsCreatePoint::RepoIndex,
        ]
        .into_iter()
        .enumerate()
        {
            db.execute_unprepared("DROP TABLE IF EXISTS code_fts")
                .await
                .expect("reset fault-injection fixture");
            let manager = SchemaManager::new(db);
            let error = ensure_code_fts_with_hook(&manager, move |point| async move {
                if point == fail_at {
                    Err(DbErr::Custom(format!("injected failure after {fail_at:?}")))
                } else {
                    Ok(())
                }
            })
            .await
            .expect_err("injected create failure must escape");
            assert!(error.to_string().contains("injected failure"));

            let repo_id = <i64 as std::convert::TryFrom<usize>>::try_from(index + 10)
                .expect("small fixture id");
            let path = format!("between-{index}.rs");
            insert_row(db, repo_id, &path).await;
            ensure_code_fts_with_hook(&manager, |_| async { Ok(()) })
                .await
                .expect("retry must finish the code_fts schema");
            assert_eq!(rows(db).await, vec![(repo_id, path)]);
            assert_schema_complete(&manager).await;
        }
    }

    async fn insert_row(db: &DatabaseConnection, repo_id: i64, file_path: &str) {
        let backend = db.get_database_backend();
        let sql = crate::prepare_sql(
            backend,
            "INSERT INTO code_fts(repo_id, file_path, file_name, content, language) \
             VALUES (?, ?, ?, ?, ?)",
        );
        db.execute(Statement::from_sql_and_values(
            backend,
            &sql,
            [
                Value::from(repo_id),
                Value::from(file_path.to_string()),
                Value::from(file_path.to_string()),
                Value::from(format!("fn fixture_{repo_id}() {{}}")),
                Value::from("Rust".to_string()),
            ],
        ))
        .await
        .expect("write code_fts fixture row");
    }

    async fn rows(db: &DatabaseConnection) -> Vec<(i64, String)> {
        let backend = db.get_database_backend();
        db.query_all(Statement::from_string(
            backend,
            "SELECT repo_id, file_path FROM code_fts ORDER BY repo_id".to_string(),
        ))
        .await
        .expect("read code_fts rows")
        .into_iter()
        .map(|row| {
            (
                i64::try_get_by_index(&row, 0).expect("repo id"),
                String::try_get_by_index(&row, 1).expect("file path"),
            )
        })
        .collect()
    }

    async fn assert_schema_complete(manager: &SchemaManager<'_>) {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => assert_sqlite_fts5(manager)
                .await
                .expect("SQLite code_fts must remain FTS5"),
            DatabaseBackend::Postgres => {
                assert!(manager
                    .has_index(TABLE, SEARCH_INDEX)
                    .await
                    .expect("inspect PostgreSQL GIN index"));
                assert!(manager
                    .has_index(TABLE, REPO_INDEX)
                    .await
                    .expect("inspect PostgreSQL repo index"));
            }
            DatabaseBackend::MySql => {
                assert!(mysql_has_compatible_fulltext_index(manager)
                    .await
                    .expect("inspect MySQL FULLTEXT index"));
                assert!(manager
                    .has_index(TABLE, REPO_INDEX)
                    .await
                    .expect("inspect MySQL repo index"));
            }
        }
    }
}
