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

use rg_db::entities::repository;
use rg_db::sea_orm::{ActiveValue::NotSet, ActiveValue::Set, ConnectionTrait, DatabaseBackend};
use rg_db::sea_orm::{DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;

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

fn repo_row(owner_id: i64, org_id: Option<i64>, name: &str) -> repository::ActiveModel {
    let now = chrono::Utc::now();
    repository::ActiveModel {
        id: NotSet,
        owner_id: Set(owner_id),
        name: Set(name.to_string()),
        description: Set(None),
        is_private: Set(false),
        default_branch: Set("main".to_string()),
        fork_id: Set(None),
        stars_count: Set(0),
        forks_count: Set(0),
        org_id: Set(org_id),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        origin_repo_id: Set(None),
    }
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
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to throwaway database");

    // ── The schema as it was, one step before the rebuild ────────────────────
    let all = rg_db::migrations::Migrator::migrations().len();
    let before_rebuild = u32::try_from(all - 1).expect("migration count fits in u32");
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
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        "nsrebuild",
        "nsrebuild@example.invalid",
        "unused",
        "Namespace Rebuild",
    )
    .await
    .expect("create user");

    let org = rg_db::ops::org_ops::create_org(&db, "nsrebuildcorp", None, None, owner.id, "public")
        .await
        .expect("create org owned by the same account");

    let repo = rg_db::ops::repo_ops::create(&db, repo_row(owner.id, None, "twin"))
        .await
        .expect("create personal repository");

    assert!(
        rg_db::ops::repo_star_ops::toggle_star(&db, owner.id, repo.id)
            .await
            .expect("star the repository"),
        "baseline: a child row exists behind ON DELETE CASCADE"
    );
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT count(*) AS n FROM repos_fts WHERE rowid = {}",
                repo.id
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
                repo.id
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
                repo.id
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
        "the AUTOINCREMENT counter was reset, so the next repository can reuse an id"
    );

    // ── What the change was for: the two namespaces hold the same name ───────
    let org_twin = rg_db::ops::repo_ops::create(&db, repo_row(owner.id, Some(org.id), "twin"))
        .await
        .expect("an organization repository may share a name with its owner's personal one");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT count(*) AS n FROM repos_fts WHERE rowid = {}",
                org_twin.id
            )
        )
        .await,
        1,
        "the recreated FTS trigger did not fire for a repository created after the rebuild"
    );

    // ── And uniqueness is still the database's, not the service layer's ──────
    assert!(
        rg_db::ops::repo_ops::create(&db, repo_row(owner.id, None, "twin"))
            .await
            .is_err(),
        "a second personal repository called `twin` was accepted — inside one namespace \
         the name must still be unique, and enforced here rather than upstream"
    );
    assert!(
        rg_db::ops::repo_ops::create(&db, repo_row(owner.id, Some(org.id), "twin"))
            .await
            .is_err(),
        "a second `twin` in the same organization was accepted"
    );

    // ── A soft-deleted repository holds no name ──────────────────────────────
    rg_db::ops::repo_ops::soft_delete(&db, repo.id)
        .await
        .expect("soft-delete the personal repository");
    let reused = rg_db::ops::repo_ops::create(&db, repo_row(owner.id, None, "twin"))
        .await
        .expect(
            "the name of a soft-deleted repository is still reserved: every lookup filters \
             `deleted_at IS NULL`, so this came back as an anonymous 5xx from the constraint",
        );

    // ── Cascade still cascades, in the direction it is supposed to ───────────
    assert!(
        rg_db::ops::repo_star_ops::toggle_star(&db, owner.id, reused.id)
            .await
            .expect("star the reused repository"),
        "baseline: the repository about to be hard-deleted has a child row"
    );
    rg_db::ops::repo_ops::delete_by_id(&db, reused.id)
        .await
        .expect("hard-delete the repository");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT count(*) AS n FROM repo_stars WHERE repo_id = {}",
                reused.id
            )
        )
        .await,
        0,
        "the rebuilt table lost its incoming foreign keys — deleting a repository no longer \
         takes its child rows with it"
    );
}
