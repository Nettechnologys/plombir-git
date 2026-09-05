//! card_615e00843297: `repositories` held one name per account, not one per
//! namespace, and the constraint saying so was inline in `CREATE TABLE` —
//! removable on SQLite only by rebuilding the table.
//!
//! A table rebuild is the kind of change that either works or quietly destroys
//! data, so this exercises the *upgrade* path rather than a fresh database:
//! migrate to the step before the rebuild, put rows in — a repository, a child
//! row behind an `ON DELETE CASCADE`, an FTS entry — and only then run it.
//!
//! What each assertion is guarding:
//!
//! * **Rows and children survive.** `DROP TABLE` under `PRAGMA foreign_keys =
//!   ON` performs an implicit `DELETE`, so a rebuild that drops the table while
//!   17 other tables still reference it cascades their rows away. The star row
//!   below is the canary.
//! * **The FTS index is neither doubled nor emptied.** The three `repos_fts`
//!   triggers have to be dropped before the copy (or every row is indexed
//!   twice) and recreated after it (or nothing is indexed again).
//! * **Uniqueness is still the database's job.** The point of the change is to
//!   *narrow* the constraint, not to move it into the service layer.

use rg_db::sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::MigratorTrait;

const OWNER_ID: i64 = 1;
const ORG_ID: i64 = 1;
const PERSONAL_REPO_ID: i64 = 1;
const ORG_REPO_ID: i64 = 2;
const REUSED_REPO_ID: i64 = 3;
const HIGH_WATER_REPO_ID: i64 = 50;

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-namespace-rebuild-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        Self { path }
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

/// Run raw SQL against the schema pinned by this upgrade test.
///
/// The fixture must not use `ops::*`: those helpers describe HEAD and can start
/// writing a column that did not exist when the migration under test runs.
async fn execute(db: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        sql.to_string(),
    ))
    .await
    .map(|_| ())
}

async fn insert_user(db: &DatabaseConnection) -> Result<(), DbErr> {
    execute(
        db,
        "INSERT INTO users \
         (id, username, email, password_hash, created_at, updated_at) \
         VALUES (1, 'nsrebuild', 'nsrebuild@example.invalid', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .await
}

async fn insert_org(db: &DatabaseConnection) -> Result<(), DbErr> {
    execute(
        db,
        "INSERT INTO organizations \
         (id, name, owner_id, created_at, updated_at) \
         VALUES (1, 'nsrebuildcorp', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .await
}

async fn insert_repo(
    db: &DatabaseConnection,
    id: i64,
    org_id: Option<i64>,
    name: &str,
) -> Result<(), DbErr> {
    let org_id = org_id.map_or_else(|| "NULL".to_string(), |id| id.to_string());
    execute(
        db,
        &format!(
            "INSERT INTO repositories \
             (id, owner_id, name, org_id, created_at, updated_at) \
             VALUES ({id}, {OWNER_ID}, '{name}', {org_id}, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
        ),
    )
    .await
}

async fn insert_star(db: &DatabaseConnection, repo_id: i64) -> Result<(), DbErr> {
    execute(
        db,
        &format!(
            "INSERT INTO repo_stars (user_id, repo_id, created_at) \
             VALUES ({OWNER_ID}, {repo_id}, CURRENT_TIMESTAMP)"
        ),
    )
    .await
}

/// `CREATE TABLE` text of `repositories` as SQLite currently holds it.
async fn table_sql(db: &DatabaseConnection) -> String {
    db.query_one(Statement::from_string(
        DatabaseBackend::Sqlite,
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'repositories'",
    ))
    .await
    .expect("read schema")
    .expect("repositories exists")
    .try_get::<String>("", "sql")
    .expect("schema text")
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .expect("query")
        .expect("one row")
        .try_get::<i64>("", "n")
        .expect("count column")
}

#[tokio::test]
async fn the_rebuild_narrows_the_constraint_without_losing_rows_children_or_the_fts_index() {
    let temp = TempDb::new();
    // This fixture drives `Migrator` directly, so it does not get the pool-wide
    // schema refresh performed by `run_migrations` after SQLite DDL churn.
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway database");

    // ── The schema as it was, one step before the rebuild ────────────────────
    // Located by name, not by "one before the end": the rebuild stopped being
    // the newest migration the moment another one landed, and a count-based
    // index silently starts testing a different step instead of failing.
    const REBUILD: &str = "m20260730_000001_repositories_namespace_unique";
    let before_rebuild = rg_db::migrations::Migrator::migrations()
        .iter()
        .position(|m| m.name() == REBUILD)
        .unwrap_or_else(|| panic!("{REBUILD} must still be part of the migration list"));
    let before_rebuild = u32::try_from(before_rebuild).expect("migration index fits in u32");
    rg_db::migrations::Migrator::up(&db, Some(before_rebuild))
        .await
        .expect("migrate up to the step before the rebuild");

    assert!(
        table_sql(&db)
            .await
            .contains(r#"UNIQUE ("owner_id", "name")"#),
        "baseline: the account-wide constraint is what this migration is here to replace"
    );

    // ── Data that the rebuild has to carry across ────────────────────────────
    insert_user(&db)
        .await
        .expect("seed a user valid for the pre-rebuild schema");
    insert_org(&db)
        .await
        .expect("seed an organization valid for the pre-rebuild schema");
    insert_repo(&db, PERSONAL_REPO_ID, None, "twin")
        .await
        .expect("seed a personal repository valid for the pre-rebuild schema");

    insert_star(&db, PERSONAL_REPO_ID)
        .await
        .expect("seed a child row behind ON DELETE CASCADE");
    insert_repo(&db, HIGH_WATER_REPO_ID, None, "deleted-high-water")
        .await
        .expect("advance the AUTOINCREMENT counter beyond the surviving rows");
    execute(
        &db,
        &format!("DELETE FROM repositories WHERE id = {HIGH_WATER_REPO_ID}"),
    )
    .await
    .expect("hard-delete the row that established the high-water mark");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT count(*) AS n FROM repos_fts WHERE rowid = {}",
                PERSONAL_REPO_ID
            )
        )
        .await,
        1,
        "baseline: the FTS trigger indexed the repository on insert"
    );

    let sequence_before = scalar(
        &db,
        "SELECT seq AS n FROM sqlite_sequence WHERE name = 'repositories'",
    )
    .await;

    // ── The rebuild ──────────────────────────────────────────────────────────
    rg_db::migrations::Migrator::up(&db, None)
        .await
        .expect("run the namespace-uniqueness migration");

    let sql = table_sql(&db).await;
    assert!(
        !sql.contains(r#"UNIQUE ("owner_id", "name")"#),
        "the account-wide constraint survived the rebuild: {sql}"
    );
    assert!(
        sql.contains("namespace_key"),
        "the rebuilt table has no namespace_key column: {sql}"
    );

    assert_eq!(
        scalar(&db, "SELECT count(*) AS n FROM repositories").await,
        1,
        "the rebuild lost the repository row"
    );
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT count(*) AS n FROM repo_stars WHERE repo_id = {}",
                PERSONAL_REPO_ID
            )
        )
        .await,
        1,
        "dropping the old table cascaded a child row away — foreign keys were live \
         while the table it was referencing went out from under it"
    );
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT count(*) AS n FROM repos_fts WHERE rowid = {}",
                PERSONAL_REPO_ID
            )
        )
        .await,
        1,
        "the FTS index no longer holds exactly one row for the repository — the triggers \
         either fired during the copy or were not put back"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT seq AS n FROM sqlite_sequence WHERE name = 'repositories'",
        )
        .await,
        sequence_before,
        "the AUTOINCREMENT counter was reset below a deleted high-water row, so the next \
         repository can reuse an id"
    );

    // ── What the change was for: the two namespaces hold the same name ───────
    insert_repo(&db, ORG_REPO_ID, Some(ORG_ID), "twin")
        .await
        .expect("an organization repository may share a name with its owner's personal one");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT count(*) AS n FROM repos_fts WHERE rowid = {}",
                ORG_REPO_ID
            )
        )
        .await,
        1,
        "the recreated FTS trigger did not fire for a repository created after the rebuild"
    );

    // ── And uniqueness is still the database's, not the service layer's ──────
    assert!(
        insert_repo(&db, ORG_REPO_ID + 1, None, "twin")
            .await
            .is_err(),
        "a second personal repository called `twin` was accepted — inside one namespace \
         the name must still be unique, and enforced here rather than upstream"
    );
    assert!(
        insert_repo(&db, ORG_REPO_ID + 1, Some(ORG_ID), "twin")
            .await
            .is_err(),
        "a second `twin` in the same organization was accepted"
    );

    // ── A soft-deleted repository holds no name ──────────────────────────────
    execute(
        &db,
        "UPDATE repositories SET deleted_at = CURRENT_TIMESTAMP WHERE id = 1",
    )
    .await
    .expect("soft-delete the personal repository");
    insert_repo(&db, REUSED_REPO_ID, None, "twin").await.expect(
        "the name of a soft-deleted repository is still reserved: every lookup filters \
             `deleted_at IS NULL`, so this came back as an anonymous 5xx from the constraint",
    );

    // ── Cascade still cascades, in the direction it is supposed to ───────────
    insert_star(&db, REUSED_REPO_ID)
        .await
        .expect("star the reused repository");
    execute(&db, "DELETE FROM repositories WHERE id = 3")
        .await
        .expect("hard-delete the repository");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT count(*) AS n FROM repo_stars WHERE repo_id = {}",
                REUSED_REPO_ID
            )
        )
        .await,
        0,
        "the rebuilt table lost its incoming foreign keys — deleting a repository no longer \
         takes its child rows with it"
    );
}
