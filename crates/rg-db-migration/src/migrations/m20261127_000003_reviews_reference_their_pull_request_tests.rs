//! card_09d54b1cd124: reviews and review comments reference their pull request
//! (and a comment its review) on every backend, the rows that already point at
//! nothing are settled, and the database refuses the next one.

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TryGetable};
use sea_orm_migration::{MigrationName, MigratorTrait, SchemaManager};

use super::REBUILD;

/// Ids above anything the shared exclusive database already holds: on the
/// server jobs the earlier migration tests seeded it first.
const BASE: i64 = 9_300;
/// The highest id in each table is an orphan, so the rebuild has to carry the
/// `AUTOINCREMENT` high-water mark across a row it removes.
const HIGH_WATER: i64 = BASE + 50;
/// Ids no row carries.
const GONE: i64 = 999_900;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-review-reference-{label}-{}.db",
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

/// Apply every migration before this one that the database does not have yet.
async fn migrate_to_just_before(db: &DatabaseConnection) {
    let name = super::Migration.name().to_string();
    let position = crate::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == name)
        .unwrap_or_else(|| panic!("{name} must be part of the migration list"));
    let applied = crate::migrations::Migrator::get_applied_migrations(db)
        .await
        .expect("read the applied migrations")
        .len();
    assert!(
        applied <= position,
        "the database already has {name}: an upgrade test needs one that does not"
    );
    let steps = u32::try_from(position - applied).expect("migration count fits in u32");
    if steps > 0 {
        crate::migrations::Migrator::up(db, Some(steps))
            .await
            .expect("migrate to the schema immediately before this migration");
    }
}

/// A live pull request with a live review and comment; a review on no pull
/// request; comments on no pull request, on no review, and on the orphaned
/// review; an attachment uploaded into an orphaned comment. Portable SQL.
async fn seed(db: &DatabaseConnection) {
    let user = BASE;
    let repo = BASE;
    let pr = BASE;
    let review = BASE;
    let orphan_review = HIGH_WATER;
    let script = format!(
        r#"
        INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
        VALUES ({user}, 'reviewer{user}', 'reviewer{user}@example.invalid', '',
                CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES ({repo}, {user}, 'reviewed{repo}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO pull_requests (id, repo_id, number, title, author_id, head_branch, base_branch)
        VALUES ({pr}, {repo}, 1, 'A change', {user}, 'feature', 'main');
        INSERT INTO pr_reviews (id, pr_id, repo_id, reviewer_id, action, created_at)
        VALUES ({review}, {pr}, {repo}, {user}, 'comment', CURRENT_TIMESTAMP);
        INSERT INTO pr_reviews (id, pr_id, repo_id, reviewer_id, action, created_at)
        VALUES ({orphan_review}, {GONE}, {repo}, {user}, 'approve', CURRENT_TIMESTAMP);
        INSERT INTO review_comments (id, review_id, pr_id, author_id, path, body)
        VALUES ({live}, {review}, {pr}, {user}, 'a.rs', 'live');
        INSERT INTO review_comments (id, review_id, pr_id, author_id, path, body)
        VALUES ({no_pr}, {review}, {GONE}, {user}, 'a.rs', 'pull request gone');
        INSERT INTO review_comments (id, review_id, pr_id, author_id, path, body)
        VALUES ({no_review}, {GONE}, {pr}, {user}, 'a.rs', 'review gone');
        INSERT INTO review_comments (id, review_id, pr_id, author_id, path, body)
        VALUES ({HIGH_WATER}, {orphan_review}, {GONE}, {user}, 'a.rs', 'on the orphaned review');
        INSERT INTO attachments
            (id, uuid, repo_id, uploader_id, review_comment_id, filename, blob_key,
             content_type, size, download_count, created_at)
        VALUES ({BASE}, 'rc{BASE}', {repo}, {user}, {no_pr}, 'trace.log',
                'attachments/{BASE}/trace.log', 'text/plain', 5, 0, CURRENT_TIMESTAMP);
        "#,
        live = BASE,
        no_pr = BASE + 1,
        no_review = BASE + 2,
    );
    // One statement per call: MySQL's driver runs one at a time.
    for statement in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        db.execute_unprepared(statement)
            .await
            .unwrap_or_else(|error| panic!("seed `{statement}`: {error}"));
    }
}

async fn scalar<T: TryGetable>(db: &DatabaseConnection, sql: &str) -> T {
    let row = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .unwrap_or_else(|error| panic!("`{sql}`: {error}"))
        .unwrap_or_else(|| panic!("`{sql}` returned no row"));
    row.try_get_by_index(0)
        .unwrap_or_else(|error| panic!("`{sql}`: {error}"))
}

async fn ids(db: &DatabaseConnection, table: &str) -> Vec<i64> {
    db.query_all(Statement::from_string(
        db.get_database_backend(),
        format!("SELECT id FROM {table} WHERE id >= {BASE} AND id <= {HIGH_WATER} ORDER BY id"),
    ))
    .await
    .unwrap()
    .iter()
    .map(|row| i64::try_get_by_index(row, 0).unwrap())
    .collect()
}

/// What the migration must have done, on any backend.
async fn exercise(db: &DatabaseConnection) {
    assert_eq!(
        ids(db, "pr_reviews").await,
        [BASE],
        "the orphaned review stays"
    );
    assert_eq!(
        ids(db, "review_comments").await,
        [BASE],
        "an orphaned comment stays"
    );
    assert_eq!(
        scalar::<i64>(
            db,
            &format!(
                "SELECT COUNT(*) FROM attachments WHERE id = {BASE} AND review_comment_id IS NULL"
            )
        )
        .await,
        1,
        "an attachment uploaded into an orphan must be detached, not cascaded away"
    );

    for (what, statement) in [
        (
            "a review on no pull request",
            format!(
                "INSERT INTO pr_reviews (id, pr_id, repo_id, reviewer_id, action, created_at) \
                 VALUES ({}, {GONE}, {BASE}, {BASE}, 'comment', CURRENT_TIMESTAMP)",
                BASE + 10
            ),
        ),
        (
            "a comment on no pull request",
            format!(
                "INSERT INTO review_comments (id, review_id, pr_id, author_id, path, body) \
                 VALUES ({}, {BASE}, {GONE}, {BASE}, 'a.rs', 'x')",
                BASE + 11
            ),
        ),
        (
            "a comment on no review",
            format!(
                "INSERT INTO review_comments (id, review_id, pr_id, author_id, path, body) \
                 VALUES ({}, {GONE}, {BASE}, {BASE}, 'a.rs', 'x')",
                BASE + 12
            ),
        ),
    ] {
        let refused = db
            .execute_unprepared(&statement)
            .await
            .expect_err(&format!("{what} was stored after the migration"));
        assert!(
            refused.to_string().to_lowercase().contains("foreign key"),
            "{what} was refused for some other reason: {refused}"
        );
    }

    // A pull request takes its reviews and their comments with it.
    db.execute_unprepared(&format!("DELETE FROM pull_requests WHERE id = {BASE}"))
        .await
        .expect("delete the pull request");
    assert!(ids(db, "pr_reviews").await.is_empty());
    assert!(ids(db, "review_comments").await.is_empty());
}

async fn references_installed(db: &DatabaseConnection) -> i64 {
    scalar(
        db,
        r#"SELECT
             (SELECT count(*) FROM pragma_foreign_key_list('pr_reviews')
              WHERE "from" = 'pr_id' AND "table" = 'pull_requests' AND "on_delete" = 'CASCADE')
           + (SELECT count(*) FROM pragma_foreign_key_list('review_comments')
              WHERE "from" = 'pr_id' AND "table" = 'pull_requests' AND "on_delete" = 'CASCADE')
           + (SELECT count(*) FROM pragma_foreign_key_list('review_comments')
              WHERE "from" = 'review_id' AND "table" = 'pr_reviews' AND "on_delete" = 'CASCADE')"#,
    )
    .await
}

async fn sqlite_fixture(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway SQLite database");
    migrate_to_just_before(&db).await;
    seed(&db).await;
    (db, temp)
}

#[tokio::test]
async fn the_database_refuses_a_review_on_no_pull_request_and_settles_the_old_ones() {
    let (db, _temp) = sqlite_fixture("up").await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("apply the migration over a database with orphans");

    assert_eq!(references_installed(&db).await, 3);
    for table in ["pr_reviews", "review_comments"] {
        assert_eq!(
            scalar::<i64>(
                &db,
                &format!("SELECT seq FROM sqlite_sequence WHERE name = '{table}'")
            )
            .await,
            HIGH_WATER,
            "the rebuild lowered the AUTOINCREMENT high-water mark of {table}"
        );
    }
    for index in [
        "idx_pr_reviews_pr_id",
        "idx_review_comments_pr_id",
        "idx_review_comments_review_id",
    ] {
        assert_eq!(
            scalar::<i64>(
                &db,
                &format!(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = '{index}'"
                )
            )
            .await,
            1,
            "the rebuild dropped {index}"
        );
    }
    assert_eq!(
        scalar::<i64>(&db, "SELECT count(*) FROM pragma_foreign_key_check").await,
        0
    );
    exercise(&db).await;
}

#[tokio::test]
async fn down_removes_the_references_again() {
    let (db, _temp) = sqlite_fixture("down").await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("install the references");
    REBUILD
        .sqlite(&SchemaManager::new(&db), false)
        .await
        .expect("remove them again");
    assert_eq!(references_installed(&db).await, 0);
    db.execute_unprepared(&format!(
        "INSERT INTO review_comments (review_id, pr_id, author_id, path, body) \
         VALUES ({GONE}, {GONE}, {BASE}, 'a.rs', 'unchecked')"
    ))
    .await
    .expect("without the references the database accepts any pr_id again");
}

/// The same contract on the server database `PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL`
/// names, where the constraints are added in place. The CI jobs hand it over
/// after the GitLab head repair test, which leaves it just before this
/// migration.
#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL: a PostgreSQL or MySQL database without this migration"]
async fn the_references_install_over_orphans_on_a_server_database() {
    let url = std::env::var("PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL must be set");
    assert!(!url.starts_with("sqlite"), "SQLite is covered above");
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to the exclusive test database");
    migrate_to_just_before(&db).await;
    seed(&db).await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("apply the migration over a database with orphans");
    exercise(&db).await;

    crate::migrations::Migrator::down(&db, Some(1))
        .await
        .expect("reverse the migration");
    db.execute_unprepared(&format!(
        "INSERT INTO review_comments (id, review_id, pr_id, author_id, path, body) \
         VALUES ({}, {GONE}, {GONE}, {BASE}, 'a.rs', 'unchecked')",
        BASE + 20
    ))
    .await
    .expect("without the references the database accepts any pr_id again");
}
