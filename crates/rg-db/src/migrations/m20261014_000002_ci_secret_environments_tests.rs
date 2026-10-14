use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use super::super::ghost_author::SqliteRebuildPoint;
use super::{Migration, NAME, SQLITE_UP, STAGING_TABLE};

/// A secret hard-deleted before the rebuild, so the `AUTOINCREMENT` high-water
/// mark sits above the highest surviving id.
const HIGH_WATER_SECRET_ID: i64 = 40;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-ci-secret-scope-{label}-{}.db",
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

/// A database at the schema immediately before this migration, with three
/// repository-wide secrets and two environments to scope new ones to.
async fn fixture(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    // One pooled connection: a rebuild swaps the table out from under every
    // other connection of the pool (card_a28a7004b108).
    let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway SQLite database");

    let before = crate::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == NAME)
        .unwrap_or_else(|| panic!("{NAME} must still be part of the migration list"));
    let before = u32::try_from(before).expect("migration index fits in u32");
    crate::migrations::Migrator::up(&db, Some(before))
        .await
        .expect("migrate to the schema immediately before scoping secrets");

    db.execute_unprepared(&format!(
        r#"
        INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
        VALUES (1, 'scope-owner', 'scope-owner@example.invalid', '',
                CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES (1, 1, 'scoped', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO ci_environments
            (id, repo_id, name, protected, required_approvals, created_at, updated_at)
        VALUES
            (10, 1, 'staging', 0, 0, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            (11, 1, 'production', 1, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO ci_secrets
            (id, repo_id, name, encrypted_value, created_by_id, created_at, updated_at)
        VALUES
            (3, 1, 'DEPLOY_TOKEN', 'cipher-deploy', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            (4, 1, 'NPM_TOKEN', 'cipher-npm', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            (5, 1, 'GHOSTED', 'cipher-ghost', NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            ({HIGH_WATER_SECRET_ID}, 1, 'MOMENTARY', 'v', 1,
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        DELETE FROM ci_secrets WHERE id = {HIGH_WATER_SECRET_ID};
        "#
    ))
    .await
    .expect("seed the pre-migration secrets");
    (db, temp)
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .expect("run scalar query")
        .expect("scalar query returns a row")
        .try_get_by_index::<i64>(0)
        .expect("decode scalar")
}

async fn has_table(db: &DatabaseConnection, table: &str) -> bool {
    scalar(
        db,
        &format!("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '{table}'"),
    )
    .await
        > 0
}

/// Every pre-migration secret, by id, name and ciphertext.
async fn secrets(db: &DatabaseConnection) -> Vec<(i64, String, String)> {
    db.query_all(Statement::from_string(
        DatabaseBackend::Sqlite,
        "SELECT id, name, encrypted_value FROM ci_secrets ORDER BY id",
    ))
    .await
    .expect("read secrets")
    .into_iter()
    .map(|row| {
        (
            row.try_get_by_index(0).expect("id"),
            row.try_get_by_index(1).expect("name"),
            row.try_get_by_index(2).expect("encrypted_value"),
        )
    })
    .collect()
}

fn seeded() -> Vec<(i64, String, String)> {
    [
        (3, "DEPLOY_TOKEN", "cipher-deploy"),
        (4, "NPM_TOKEN", "cipher-npm"),
        (5, "GHOSTED", "cipher-ghost"),
    ]
    .into_iter()
    .map(|(id, name, value)| (id, name.to_string(), value.to_string()))
    .collect()
}

async fn insert(
    db: &DatabaseConnection,
    environment: &str,
    name: &str,
) -> Result<(), sea_orm::DbErr> {
    db.execute_unprepared(&format!(
        "INSERT INTO ci_secrets
             (repo_id, environment_id, name, encrypted_value, created_at, updated_at)
         VALUES (1, {environment}, '{name}', 'v', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
    ))
    .await
    .map(|_| ())
}

#[tokio::test]
async fn existing_secrets_keep_their_identity_in_the_repository_scope() {
    let (db, _temp) = fixture("identity").await;
    Migration
        .up(&SchemaManager::new(&db))
        .await
        .expect("scope the secrets");

    assert_eq!(secrets(&db).await, seeded());
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM ci_secrets WHERE environment_id IS NULL"
        )
        .await,
        3
    );
    assert!(!has_table(&db, STAGING_TABLE).await);

    // The repository-wide scope still holds one secret per name...
    assert!(insert(&db, "NULL", "DEPLOY_TOKEN").await.is_err());
    // ... while the same name is a different secret in each environment.
    insert(&db, "10", "DEPLOY_TOKEN")
        .await
        .expect("staging scope");
    insert(&db, "11", "DEPLOY_TOKEN")
        .await
        .expect("production scope");
    assert!(insert(&db, "11", "DEPLOY_TOKEN").await.is_err());
    // A secret cannot name an environment that does not exist.
    assert!(insert(&db, "12", "ORPHAN").await.is_err());
}

#[tokio::test]
async fn the_rebuild_keeps_the_indexes_and_the_sequence_high_water() {
    let (db, _temp) = fixture("indexes").await;
    Migration
        .up(&SchemaManager::new(&db))
        .await
        .expect("scope the secrets");

    for index in [
        "idx_ci_secrets_created_by",
        "idx_ci_secrets_environment",
        "uq_ci_secrets_repo_environment_name",
    ] {
        assert_eq!(
            scalar(
                &db,
                &format!(
                    "SELECT count(*) FROM sqlite_master \
                     WHERE type = 'index' AND tbl_name = 'ci_secrets' AND name = '{index}'"
                ),
            )
            .await,
            1,
            "the rebuild lost `{index}`"
        );
    }
    insert(&db, "NULL", "NEXT")
        .await
        .expect("insert after the rebuild");
    assert!(
        scalar(&db, "SELECT id FROM ci_secrets WHERE name = 'NEXT'").await > HIGH_WATER_SECRET_ID,
        "the next secret reused an id that was handed out before the rebuild"
    );
}

#[tokio::test]
async fn an_environment_secret_cascades_with_its_environment() {
    let (db, _temp) = fixture("cascade").await;
    Migration
        .up(&SchemaManager::new(&db))
        .await
        .expect("scope the secrets");
    db.execute_unprepared("PRAGMA foreign_keys = ON")
        .await
        .expect("enforce foreign keys");
    insert(&db, "11", "PROD_KEY")
        .await
        .expect("production secret");

    db.execute_unprepared("DELETE FROM ci_environments WHERE id = 11")
        .await
        .expect("delete the environment");
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM ci_secrets WHERE name = 'PROD_KEY'"
        )
        .await,
        0
    );
    assert_eq!(secrets(&db).await, seeded());
}

/// The acceptance case of the review: a rebuild that fails after the table was
/// renamed and the replacement filled leaves every secret where it was, and
/// the migration has not happened — so the same run can simply be retried.
#[tokio::test]
async fn failure_mid_rebuild_keeps_every_secret_and_the_rebuild_can_retry() {
    let (db, _temp) = fixture("rollback").await;
    let manager = SchemaManager::new(&db);

    let error = SQLITE_UP
        .sqlite_with_hook(&manager, |point| async move {
            if point == SqliteRebuildPoint::TableRebuilt("ci_secrets") {
                Err(sea_orm::DbErr::Custom(
                    "injected failure after rebuilding ci_secrets".to_string(),
                ))
            } else {
                Ok(())
            }
        })
        .await
        .expect_err("the injected failure must escape");
    assert!(error.to_string().contains("injected failure"), "{error}");

    assert_eq!(
        secrets(&db).await,
        seeded(),
        "the failed rebuild lost secrets"
    );
    assert!(
        !has_table(&db, STAGING_TABLE).await,
        "the failed rebuild left its staging table behind"
    );
    assert!(
        !manager
            .has_column("ci_secrets", "environment_id")
            .await
            .expect("read columns"),
        "the failed rebuild left the scoped shape behind, so a re-run would skip it"
    );

    Migration
        .up(&manager)
        .await
        .expect("the same migration can be retried after a rollback");
    assert_eq!(secrets(&db).await, seeded());
}

/// A staging table left by something other than this migration may hold the
/// only copy of the secrets: `up` refuses rather than recording itself applied.
#[tokio::test]
async fn up_refuses_while_a_staging_table_exists() {
    let (db, _temp) = fixture("staging").await;
    db.execute_unprepared(&format!(
        "CREATE TABLE \"{STAGING_TABLE}\" AS SELECT * FROM ci_secrets"
    ))
    .await
    .expect("leave a staging table behind");

    let error = Migration
        .up(&SchemaManager::new(&db))
        .await
        .expect_err("a staging table must stop the migration");
    assert!(error.to_string().contains(STAGING_TABLE), "{error}");
    assert!(
        !SchemaManager::new(&db)
            .has_column("ci_secrets", "environment_id")
            .await
            .expect("read columns"),
        "the refusal must come before the rebuild"
    );
}

/// `down` refuses while an environment secret exists rather than widening it
/// to every branch, and is a real reversal once none do.
#[tokio::test]
async fn down_refuses_environment_secrets_and_restores_the_old_constraint() {
    let (db, _temp) = fixture("down").await;
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.expect("scope the secrets");
    insert(&db, "11", "DEPLOY_TOKEN")
        .await
        .expect("production secret");

    let error = Migration
        .down(&manager)
        .await
        .expect_err("an environment secret must stop the rollback");
    assert!(error.to_string().contains("environment-scoped"), "{error}");
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM ci_secrets WHERE environment_id = 11"
        )
        .await,
        1,
        "the refused rollback touched the environment secret"
    );

    db.execute_unprepared("DELETE FROM ci_secrets WHERE environment_id IS NOT NULL")
        .await
        .expect("the operator deletes the environment secrets");
    Migration.down(&manager).await.expect("roll back");

    assert_eq!(secrets(&db).await, seeded());
    assert!(!manager
        .has_column("ci_secrets", "environment_id")
        .await
        .expect("read columns"));
    assert!(
        db.execute_unprepared(
            "INSERT INTO ci_secrets (repo_id, name, encrypted_value, created_at, updated_at)
             VALUES (1, 'DEPLOY_TOKEN', 'v', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
        )
        .await
        .is_err(),
        "the rollback did not restore `uq_ci_secrets_repo_name`"
    );
    Migration.up(&manager).await.expect("migrate up again");
    assert_eq!(secrets(&db).await, seeded());
}

/// MySQL commits every DDL statement on its own, so a crash can stop `up`
/// after any step. The re-run has to finish the job (foreign key, generated
/// column, both indexes, the old constraint gone) rather than skip it because
/// `environment_id` already exists, and `down` has to reverse it in an order
/// InnoDB accepts. PostgreSQL runs the same steps inside its transaction.
#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at a disposable PostgreSQL or MySQL database"]
async fn server_up_converges_after_an_interrupted_run_and_down_reverses_it() {
    let url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    assert!(
        !url.starts_with("sqlite"),
        "SQLite is covered by the rebuild tests above"
    );
    let db = crate::connect_with_pool(&url, crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to the disposable server database");
    crate::run_migrations(&db).await.expect("run migrations");
    let backend = db.get_database_backend();
    let manager = SchemaManager::new(&db);
    let q = |sql: &str| {
        if backend == DatabaseBackend::MySql {
            sql.replace('"', "`")
        } else {
            sql.to_string()
        }
    };
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let user = crate::ops::user_ops::create_user(
        &db,
        &format!("scope{suffix}"),
        &format!("scope{suffix}@example.invalid"),
        "unused",
        "Scope",
    )
    .await
    .expect("create user");
    let now = chrono::Utc::now();
    let repo = crate::ops::repo_ops::create(
        &db,
        crate::entities::repository::ActiveModel {
            id: sea_orm::NotSet,
            owner_id: sea_orm::Set(user.id),
            name: sea_orm::Set(format!("scope{suffix}")),
            description: sea_orm::Set(None),
            is_private: sea_orm::Set(true),
            default_branch: sea_orm::Set("main".into()),
            fork_id: sea_orm::Set(None),
            stars_count: sea_orm::Set(0),
            forks_count: sea_orm::Set(0),
            org_id: sea_orm::Set(None),
            created_at: sea_orm::Set(now),
            updated_at: sea_orm::Set(now),
            deleted_at: sea_orm::Set(None),
            origin_repo_id: sea_orm::Set(None),
        },
    )
    .await
    .expect("create repository");
    let environment = sea_orm::ActiveModelTrait::insert(
        crate::entities::ci_environment::ActiveModel {
            repo_id: sea_orm::Set(repo.id),
            name: sea_orm::Set("production".into()),
            protected: sea_orm::Set(true),
            required_approvals: sea_orm::Set(1),
            allowed_approver_ids: sea_orm::Set(None),
            created_at: sea_orm::Set(now),
            updated_at: sea_orm::Set(now),
            ..Default::default()
        },
        &db,
    )
    .await
    .expect("create environment");
    let store = |environment_id: Option<i64>, name: &'static str| {
        let db = &db;
        async move {
            crate::ops::ci_secret_ops::upsert(db, repo.id, environment_id, name, "v", user.id).await
        }
    };
    let raw_insert = |environment: String, name: &'static str| {
        let db = &db;
        let sql = q(&format!(
            "INSERT INTO \"ci_secrets\" (\"repo_id\", \"environment_id\", \"name\", \
             \"encrypted_value\", \"created_at\", \"updated_at\") \
             VALUES ({}, {environment}, '{name}', 'v', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            repo.id
        ));
        async move { db.execute_unprepared(&sql).await.map(|_| ()) }
    };

    // Rolled back: the old one-scope constraint is back.
    Migration
        .down(&manager)
        .await
        .expect("roll back an unscoped table");
    assert!(!manager
        .has_column("ci_secrets", "environment_id")
        .await
        .unwrap());
    raw_insert_old(&db, &q, repo.id, "TWICE")
        .await
        .expect("first row");
    assert!(
        raw_insert_old(&db, &q, repo.id, "TWICE").await.is_err(),
        "the rollback did not restore `uq_ci_secrets_repo_name`"
    );

    // A run that stopped after its first statement.
    db.execute_unprepared(&q(
        "ALTER TABLE \"ci_secrets\" ADD COLUMN \"environment_id\" BIGINT NULL",
    ))
    .await
    .expect("simulate the first step of an interrupted run");
    Migration
        .up(&manager)
        .await
        .expect("converge the interrupted run");
    Migration
        .up(&manager)
        .await
        .expect("a converged schema is a no-op");

    store(None, "DEPLOY_TOKEN")
        .await
        .expect("repository-wide secret");
    store(Some(environment.id), "DEPLOY_TOKEN")
        .await
        .expect("the same name in an environment, once the old constraint is gone");
    assert!(
        raw_insert("NULL".into(), "DEPLOY_TOKEN").await.is_err(),
        "the repository-wide scope lost its uniqueness"
    );
    assert!(
        raw_insert(environment.id.to_string(), "DEPLOY_TOKEN")
            .await
            .is_err(),
        "an environment scope lost its uniqueness"
    );
    assert!(
        raw_insert("999999999".into(), "ORPHAN").await.is_err(),
        "the converged run did not add the environment foreign key"
    );
    let resolved = crate::ops::ci_secret_ops::list_for_job(&db, repo.id, Some(environment.id))
        .await
        .expect("list for a production job");
    assert_eq!(
        resolved
            .iter()
            .map(|secret| (secret.name.as_str(), secret.environment_id))
            .collect::<Vec<_>>(),
        [("DEPLOY_TOKEN", Some(environment.id)), ("TWICE", None)]
    );

    let refusal = Migration
        .down(&manager)
        .await
        .expect_err("an environment secret must stop the rollback");
    assert!(
        refusal.to_string().contains("environment-scoped"),
        "{refusal}"
    );
    assert!(manager
        .has_column("ci_secrets", "environment_id")
        .await
        .unwrap());

    db.execute_unprepared(&q(&format!(
        "DELETE FROM \"repositories\" WHERE \"id\" = {}",
        repo.id
    )))
    .await
    .expect("clean up the repository and its secrets");
}

async fn raw_insert_old(
    db: &DatabaseConnection,
    q: &impl Fn(&str) -> String,
    repo_id: i64,
    name: &str,
) -> Result<(), sea_orm::DbErr> {
    db.execute_unprepared(&q(&format!(
        "INSERT INTO \"ci_secrets\" (\"repo_id\", \"name\", \"encrypted_value\", \
         \"created_at\", \"updated_at\") \
         VALUES ({repo_id}, '{name}', 'v', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
    )))
    .await
    .map(|_| ())
}
