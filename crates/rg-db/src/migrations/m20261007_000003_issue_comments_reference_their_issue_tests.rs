use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TryGetable};
use sea_orm_migration::{MigratorTrait, SchemaManager};

use super::REBUILD;

const HIGH_WATER_ID: i64 = 50;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-issue-comment-reference-{label}-{}.db",
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

/// A throwaway SQLite database at the schema immediately before this
/// migration, holding the rows [`seed`] writes.
async fn fixture(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway SQLite database");
    migrate_to_just_before(&db).await;
    seed(&db).await;
    (db, temp)
}

async fn migrate_to_just_before(db: &DatabaseConnection) {
    const NAME: &str = "m20261007_000003_issue_comments_reference_their_issue";
    let before = crate::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == NAME)
        .unwrap_or_else(|| panic!("{NAME} must be part of the migration list"));
    let before = u32::try_from(before).expect("migration index fits in u32");
    crate::migrations::Migrator::up(db, Some(before))
        .await
        .expect("migrate to the schema immediately before this migration");
}

/// A live comment, an orphan whose issue is simply gone, an orphan carrying a
/// pull request's id (what the import used to write), and an attachment
/// uploaded into an orphan. Portable SQL: the server-database test seeds the
/// same rows.
async fn seed(db: &DatabaseConnection) {
    let script = format!(
        r#"
        INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
        VALUES (1, 'host', 'host@example.invalid', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES (1, 1, 'shared', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO issues (id, repo_id, number, title, author_id, created_at, updated_at)
        VALUES (1, 1, 1, 'A bug', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO pull_requests
            (id, repo_id, number, title, author_id, head_branch, base_branch)
        VALUES (7, 1, 2, 'A change', 1, 'feature', 'main');
        INSERT INTO issue_comments (id, issue_id, author_id, body, created_at, updated_at)
        VALUES
            (1, 1, 1, 'live', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            (2, 999, 1, 'issue gone', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            ({HIGH_WATER_ID}, 7, 1, 'imported PR conversation', CURRENT_TIMESTAMP,
             CURRENT_TIMESTAMP);
        INSERT INTO attachments
            (id, uuid, repo_id, uploader_id, issue_comment_id, filename, blob_key,
             content_type, size, download_count, created_at)
        VALUES (1, 'a1b2c3', 1, 1, 2, 'trace.log', 'attachments/1/a1b2c3/trace.log',
                'text/plain', 5, 0, CURRENT_TIMESTAMP);
        "#
    );
    // One statement per call: MySQL's driver runs one at a time.
    for statement in script.split(';').filter(|sql| !sql.trim().is_empty()) {
        db.execute_unprepared(statement)
            .await
            .unwrap_or_else(|error| panic!("seed `{}`: {error}", statement.trim()));
    }
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    let row = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("read scalar")
        .expect("scalar row exists");
    i64::try_get_by_index(&row, 0).expect("decode scalar")
}

async fn reference_installed(db: &DatabaseConnection) -> bool {
    scalar(
        db,
        r#"SELECT count(*) FROM pragma_foreign_key_list('issue_comments')
           WHERE "from" = 'issue_id' AND "table" = 'issues' AND "to" = 'id'
             AND "on_delete" = 'CASCADE'"#,
    )
    .await
        == 1
}

#[tokio::test]
async fn the_database_refuses_a_comment_on_no_issue_and_settles_the_old_ones() {
    let (db, _temp) = fixture("up").await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("apply the migration over a database with orphans");

    assert!(reference_installed(&db).await);
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM issue_comments").await,
        1,
        "the live comment must stay and both orphans must go"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM attachments WHERE id = 1 AND issue_comment_id IS NULL"
        )
        .await,
        1,
        "an attachment uploaded into an orphan must be detached, not cascaded away"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT seq FROM sqlite_sequence WHERE name = 'issue_comments'"
        )
        .await,
        HIGH_WATER_ID,
        "the rebuild lowered the AUTOINCREMENT high-water mark"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_issue_comments_issue_id'",
        )
        .await,
        1,
        "the rebuild dropped the issue_id index"
    );
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM pragma_foreign_key_check").await,
        0
    );

    // A pull request's id is not an issue's.
    let refused = db
        .execute_unprepared(
            "INSERT INTO issue_comments (id, issue_id, author_id, body) \
             VALUES (1000, 7, 1, 'misfiled')",
        )
        .await;
    assert!(
        refused.is_err(),
        "a comment naming no issue was stored after the migration"
    );

    // An issue takes its comments with it.
    db.execute_unprepared("DELETE FROM issues WHERE id = 1")
        .await
        .expect("delete the issue");
    assert_eq!(scalar(&db, "SELECT count(*) FROM issue_comments").await, 0);
}

#[tokio::test]
async fn down_removes_the_reference_again() {
    let (db, _temp) = fixture("down").await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("install the reference");
    let manager = SchemaManager::new(&db);
    REBUILD
        .sqlite(&manager, false)
        .await
        .expect("remove it again");
    assert!(!reference_installed(&db).await);
    db.execute_unprepared(
        "INSERT INTO issue_comments (issue_id, author_id, body) VALUES (999, 1, 'unchecked')",
    )
    .await
    .expect("without the reference the database accepts any issue_id again");
}

/// The same contract on PostgreSQL and MySQL, where the constraint is added in
/// place rather than by a rebuild.
///
/// It needs a database of its own — it migrates an empty one to the schema
/// before this migration, seeds orphans and then applies it — so it reads
/// `PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL`, not the URL the shared
/// `multi_backend_smoke` tests migrate concurrently.
#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL: an empty PostgreSQL or MySQL database for this test alone"]
async fn the_reference_installs_over_orphans_on_a_server_database() {
    let url = std::env::var("PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL must be set");
    let db = crate::connect_with_pool(&url, crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to the exclusive test database");
    migrate_to_just_before(&db).await;
    seed(&db).await;

    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("apply the migration over a database with orphans");

    assert_eq!(scalar(&db, "SELECT COUNT(*) FROM issue_comments").await, 1);
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM attachments WHERE id = 1 AND issue_comment_id IS NULL"
        )
        .await,
        1
    );
    let refused = db
        .execute_unprepared(
            "INSERT INTO issue_comments (id, issue_id, author_id, body) \
             VALUES (1000, 7, 1, 'misfiled')",
        )
        .await
        .expect_err("a comment naming no issue was stored after the migration");
    assert!(
        refused.to_string().to_lowercase().contains("foreign key"),
        "refused for some other reason: {refused}"
    );
    db.execute_unprepared("DELETE FROM issues WHERE id = 1")
        .await
        .expect("delete the issue");
    assert_eq!(scalar(&db, "SELECT COUNT(*) FROM issue_comments").await, 0);

    crate::migrations::Migrator::down(&db, Some(1))
        .await
        .expect("reverse the migration");
    db.execute_unprepared(
        "INSERT INTO issue_comments (id, issue_id, author_id, body) \
         VALUES (1001, 999, 1, 'unchecked')",
    )
    .await
    .expect("without the reference the database accepts any issue_id again");
}
