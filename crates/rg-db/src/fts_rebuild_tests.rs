use std::path::PathBuf;
use std::sync::Arc;

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait};
use tokio::sync::{oneshot, Notify};

use super::test_support::{assert_writer_stays_blocked, write_while_the_lock_is_held};
use super::{rebuild_fts_indexes, rebuild_sqlite_fts_indexes, SqliteFtsTable};

#[derive(Debug, PartialEq, Eq)]
struct FtsSnapshot {
    repositories: Vec<(i64, String, String)>,
    issues: Vec<(i64, String, String)>,
    wiki_pages: Vec<(i64, String, String)>,
}

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-fts-rebuild-{}.db",
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

async fn text_rows<C>(db: &C, sql: &str) -> Vec<(i64, String, String)>
where
    C: ConnectionTrait,
{
    db.query_all(Statement::from_string(
        DatabaseBackend::Sqlite,
        sql.to_string(),
    ))
    .await
    .unwrap_or_else(|error| panic!("query `{sql}`: {error}"))
    .into_iter()
    .map(|row| {
        (
            row.try_get_by_index(0).expect("row id"),
            row.try_get_by_index(1).expect("first text column"),
            row.try_get_by_index(2).expect("second text column"),
        )
    })
    .collect()
}

async fn fts_snapshot<C>(db: &C) -> FtsSnapshot
where
    C: ConnectionTrait,
{
    FtsSnapshot {
        repositories: text_rows(
            db,
            "SELECT rowid, name, description FROM repos_fts ORDER BY rowid",
        )
        .await,
        issues: text_rows(
            db,
            "SELECT rowid, title, body FROM issues_fts ORDER BY rowid",
        )
        .await,
        wiki_pages: text_rows(
            db,
            "SELECT rowid, title, content FROM wiki_pages_fts ORDER BY rowid",
        )
        .await,
    }
}

async fn source_snapshot<C>(db: &C) -> FtsSnapshot
where
    C: ConnectionTrait,
{
    FtsSnapshot {
        repositories: text_rows(
            db,
            "SELECT id, name, COALESCE(description, '') FROM repositories \
             WHERE deleted_at IS NULL ORDER BY id",
        )
        .await,
        issues: text_rows(
            db,
            "SELECT id, title, COALESCE(body, '') FROM issues ORDER BY id",
        )
        .await,
        wiki_pages: text_rows(db, "SELECT id, title, content FROM wiki_pages ORDER BY id").await,
    }
}

async fn fixture() -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new();
    let db = super::connect_with_pool(&temp.url(), super::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");

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
            content TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE repos_fts USING fts5(name, description);
        CREATE VIRTUAL TABLE issues_fts USING fts5(title, body);
        CREATE VIRTUAL TABLE wiki_pages_fts USING fts5(title, content);

        CREATE TRIGGER repos_fts_insert AFTER INSERT ON repositories
        WHEN NEW.deleted_at IS NULL
        BEGIN
          INSERT INTO repos_fts(rowid, name, description)
          VALUES (NEW.id, NEW.name, COALESCE(NEW.description, ''));
        END;
        CREATE TRIGGER repos_fts_update AFTER UPDATE ON repositories
        BEGIN
          DELETE FROM repos_fts WHERE rowid = OLD.id;
          INSERT INTO repos_fts(rowid, name, description)
          SELECT NEW.id, NEW.name, COALESCE(NEW.description, '')
          WHERE NEW.deleted_at IS NULL;
        END;
        CREATE TRIGGER repos_fts_delete AFTER DELETE ON repositories
        BEGIN
          DELETE FROM repos_fts WHERE rowid = OLD.id;
        END;

        CREATE TRIGGER issues_fts_insert AFTER INSERT ON issues
        BEGIN
          INSERT INTO issues_fts(rowid, title, body)
          VALUES (NEW.id, NEW.title, COALESCE(NEW.body, ''));
        END;
        CREATE TRIGGER issues_fts_update AFTER UPDATE ON issues
        BEGIN
          DELETE FROM issues_fts WHERE rowid = OLD.id;
          INSERT INTO issues_fts(rowid, title, body)
          VALUES (NEW.id, NEW.title, COALESCE(NEW.body, ''));
        END;
        CREATE TRIGGER issues_fts_delete AFTER DELETE ON issues
        BEGIN
          DELETE FROM issues_fts WHERE rowid = OLD.id;
        END;

        CREATE TRIGGER wiki_pages_fts_insert AFTER INSERT ON wiki_pages
        BEGIN
          INSERT INTO wiki_pages_fts(rowid, title, content)
          VALUES (NEW.id, NEW.title, NEW.content);
        END;
        CREATE TRIGGER wiki_pages_fts_update AFTER UPDATE ON wiki_pages
        BEGIN
          DELETE FROM wiki_pages_fts WHERE rowid = OLD.id;
          INSERT INTO wiki_pages_fts(rowid, title, content)
          VALUES (NEW.id, NEW.title, NEW.content);
        END;
        CREATE TRIGGER wiki_pages_fts_delete AFTER DELETE ON wiki_pages
        BEGIN
          DELETE FROM wiki_pages_fts WHERE rowid = OLD.id;
        END;

        INSERT INTO repositories(id, name, description, deleted_at)
        VALUES (1, 'old repo', 'old repo body', NULL),
               (2, 'delete repo', 'delete repo body', NULL);
        INSERT INTO issues(id, title, body)
        VALUES (1, 'old issue', 'old issue body');
        INSERT INTO wiki_pages(id, title, content)
        VALUES (1, 'old wiki', 'old wiki body'),
               (2, 'delete wiki', 'delete wiki body');
        "#,
    )
    .await
    .expect("create FTS fixture");

    (db, temp)
}

#[tokio::test]
async fn public_rebuild_reconciles_all_sqlite_indexes() {
    let (db, _temp) = fixture().await;
    db.execute_unprepared(
        r#"
        DELETE FROM repos_fts;
        DELETE FROM issues_fts;
        DELETE FROM wiki_pages_fts;
        "#,
    )
    .await
    .expect("clear the derived indexes");

    rebuild_fts_indexes(&db)
        .await
        .expect("public rebuild entry point succeeds");

    assert_eq!(fts_snapshot(&db).await, source_snapshot(&db).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_source_writes_wait_while_readers_see_the_old_complete_snapshot() {
    let (db, _temp) = fixture().await;
    let old_snapshot = fts_snapshot(&db).await;
    assert_eq!(old_snapshot, source_snapshot(&db).await);

    let rebuild_paused = Arc::new(Notify::new());
    let resume_rebuild = Arc::new(Notify::new());
    let rebuild_db = db.clone();
    let rebuild = {
        let rebuild_paused = Arc::clone(&rebuild_paused);
        let resume_rebuild = Arc::clone(&resume_rebuild);
        tokio::spawn(async move {
            rebuild_sqlite_fts_indexes(&rebuild_db, move |table| {
                let rebuild_paused = Arc::clone(&rebuild_paused);
                let resume_rebuild = Arc::clone(&resume_rebuild);
                async move {
                    if table == SqliteFtsTable::WikiPages {
                        rebuild_paused.notify_one();
                        resume_rebuild.notified().await;
                    }
                    Ok(())
                }
            })
            .await
        })
    };

    rebuild_paused.notified().await;
    assert_eq!(
        fts_snapshot(&db).await,
        old_snapshot,
        "a reader must not see the first two rebuilt indexes and an empty third one"
    );

    let writer_db = db.clone();
    let (writer_started_tx, writer_started_rx) = oneshot::channel();
    let mut writer = tokio::spawn(async move {
        writer_started_tx
            .send(())
            .expect("test must still wait for the writer to start");

        write_while_the_lock_is_held(|| {
            let writer_db = writer_db.clone();
            async move {
                let transaction = writer_db.begin().await?;
                transaction
                    .execute_unprepared(
                        "INSERT INTO issues(id, title, body) \
                         VALUES (2, 'new issue', 'new issue body')",
                    )
                    .await?;
                transaction
                    .execute_unprepared(
                        "UPDATE repositories \
                         SET name = 'new repo', description = 'new repo body' WHERE id = 1",
                    )
                    .await?;
                transaction
                    .execute_unprepared("DELETE FROM repositories WHERE id = 2")
                    .await?;
                transaction
                    .execute_unprepared(
                        "UPDATE wiki_pages \
                         SET title = 'new wiki', content = 'new wiki body' WHERE id = 1",
                    )
                    .await?;
                transaction
                    .execute_unprepared("DELETE FROM wiki_pages WHERE id = 2")
                    .await?;
                transaction.commit().await
            }
        })
        .await
    });

    writer_started_rx.await.expect("writer task started");
    assert_writer_stays_blocked(
        &mut writer,
        "the source writer must be held behind the rebuild transaction",
    )
    .await;
    assert_eq!(
        fts_snapshot(&db).await,
        old_snapshot,
        "search during the blocked writer must still see the old full snapshot"
    );

    resume_rebuild.notify_one();
    rebuild
        .await
        .expect("rebuild task did not panic")
        .expect("atomic rebuild succeeds");
    writer
        .await
        .expect("writer task did not panic")
        .expect("the source writer commits once the rebuild releases the write lock");

    let source = source_snapshot(&db).await;
    assert_ne!(source, old_snapshot, "the writer must change the fixture");
    assert_eq!(
        fts_snapshot(&db).await,
        source,
        "INSERT, UPDATE and DELETE triggers must leave every index equal to its source"
    );
}

#[tokio::test]
async fn failure_while_clearing_the_second_index_rolls_back_the_first_rebuild() {
    let (db, _temp) = fixture().await;
    db.execute_unprepared(
        r#"
        DELETE FROM repos_fts;
        INSERT INTO repos_fts(rowid, name, description)
        VALUES (1, 'stale repo', 'stale repo body'),
               (2, 'stale delete repo', 'stale delete repo body');
        DELETE FROM issues_fts;
        INSERT INTO issues_fts(rowid, title, body)
        VALUES (1, 'stale issue', 'stale issue body');
        "#,
    )
    .await
    .expect("seed deliberate index drift");

    let before = fts_snapshot(&db).await;
    assert_ne!(before, source_snapshot(&db).await);

    let error = rebuild_sqlite_fts_indexes(&db, |table| async move {
        if table == SqliteFtsTable::Issues {
            anyhow::bail!("injected failure after clearing issues_fts");
        }
        Ok(())
    })
    .await
    .expect_err("fault injection must abort the rebuild");
    assert!(error
        .to_string()
        .contains("injected failure after clearing issues_fts"));
    assert_eq!(
        fts_snapshot(&db).await,
        before,
        "repos_fts must not commit its rebuild and issues_fts must not stay empty"
    );
}
