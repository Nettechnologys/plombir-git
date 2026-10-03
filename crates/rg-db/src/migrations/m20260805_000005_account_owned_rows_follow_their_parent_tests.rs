use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TryGetable};
use sea_orm_migration::{MigratorTrait, SchemaManager};

use super::super::ghost_author::SqliteRebuildPoint;
use super::REBUILD;

const HIGH_WATER_ID: i64 = 50;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-required-account-reference-{label}-{}.db",
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

/// The pre-migration schema with both meaningful rows and historical orphans.
/// The latter are not artificial: they are exactly what the missing references
/// allowed an earlier account/repository deletion to leave behind.
async fn fixture(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway SQLite database");

    const NAME: &str = "m20260805_000005_account_owned_rows_follow_their_parent";
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
            (1, 'host', 'host@example.invalid', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            (2, 'guest', 'guest@example.invalid', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES (1, 1, 'shared', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);

        INSERT INTO organizations
            (id, name, owner_id, visibility, created_at, updated_at)
        VALUES (1, 'host-org', 1, 'private', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO teams
            (id, org_id, name, permission, created_at, updated_at)
        VALUES (1, 1, 'developers', 'write', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);

        INSERT INTO repo_collaborators (id, repo_id, user_id, permission, created_at)
        VALUES
            (1, 1, 2, 'write', CURRENT_TIMESTAMP),
            (49, 1, 999, 'read', CURRENT_TIMESTAMP),
            ({HIGH_WATER_ID}, 999, 2, 'read', CURRENT_TIMESTAMP);
        INSERT INTO organization_members (id, org_id, user_id, role, created_at)
        VALUES
            (1, 1, 2, 'member', CURRENT_TIMESTAMP),
            ({HIGH_WATER_ID}, 1, 999, 'member', CURRENT_TIMESTAMP);
        INSERT INTO team_members (id, team_id, user_id, role, created_at)
        VALUES
            (1, 1, 2, 'member', CURRENT_TIMESTAMP),
            ({HIGH_WATER_ID}, 1, 999, 'member', CURRENT_TIMESTAMP);
        INSERT INTO notifications
            (id, user_id, event_type, title, body, repo_id, is_read, created_at)
        VALUES
            (1, 2, 'issue', 'mentioned', NULL, 1, FALSE, CURRENT_TIMESTAMP),
            ({HIGH_WATER_ID}, 999, 'issue', 'orphan', NULL, 1, FALSE, CURRENT_TIMESTAMP);
        "#
    ))
    .await
    .expect("seed live account rows and historical orphans");

    (db, temp)
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

async fn assert_reference_shape(db: &DatabaseConnection, installed: bool) {
    for reference in REBUILD.references {
        let count = scalar(
            db,
            &format!(
                r#"SELECT count(*) FROM pragma_foreign_key_list('{}')
                   WHERE "from" = '{}' AND "table" = '{}' AND "to" = '{}'
                     AND "on_delete" = 'CASCADE'"#,
                reference.table, reference.column, reference.parent_table, reference.parent_column
            ),
        )
        .await;
        assert_eq!(
            count,
            i64::from(installed),
            "{}.{} has the wrong required-reference shape",
            reference.table,
            reference.column
        );
    }

    // The migration owns only the missing account/repository references. The
    // pre-existing organization/team ownership links must survive both up and
    // down.
    for (table, column, parent) in [
        ("organization_members", "org_id", "organizations"),
        ("team_members", "team_id", "teams"),
    ] {
        assert_eq!(
            scalar(
                db,
                &format!(
                    r#"SELECT count(*) FROM pragma_foreign_key_list('{table}')
                       WHERE "from" = '{column}' AND "table" = '{parent}'
                         AND "on_delete" = 'CASCADE'"#
                ),
            )
            .await,
            1,
            "the rebuild lost the existing {table}.{column} reference"
        );
    }
}

async fn assert_rebuild_invariants(db: &DatabaseConnection) {
    for table in [
        "repo_collaborators",
        "organization_members",
        "team_members",
        "notifications",
    ] {
        assert_eq!(
            scalar(db, &format!("SELECT count(*) FROM {table}")).await,
            1,
            "the migration failed to keep the live row and remove historical orphans from {table}"
        );
        assert_eq!(
            scalar(
                db,
                &format!("SELECT seq FROM sqlite_sequence WHERE name = '{table}'"),
            )
            .await,
            HIGH_WATER_ID,
            "the rebuild lowered {table}'s AUTOINCREMENT high-water mark"
        );
    }
    assert_eq!(
        scalar(
            db,
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN \
             ('idx_repo_collaborators_repo_user', 'idx_notifications_user_id_is_read', \
              'idx_notifications_repo_id')",
        )
        .await,
        3,
        "the rebuild did not restore every named index"
    );
    assert_eq!(
        scalar(db, "SELECT count(*) FROM pragma_foreign_key_check").await,
        0,
        "the rebuild left a dangling foreign key reference behind"
    );
}

#[tokio::test]
async fn account_and_repository_deletion_cascade_the_rows_they_own() {
    let (db, _temp) = fixture("cascade").await;
    let manager = SchemaManager::new(&db);
    REBUILD
        .sqlite(&manager, true)
        .await
        .expect("install the required references");
    assert_reference_shape(&db, true).await;
    assert_rebuild_invariants(&db).await;

    // A raw `DELETE`, the way `down_removes_only_the_references_this_migration_added`
    // below already does it, and not `user_ops::delete_by_id`. The claim under
    // test is a database one — the reference this migration installs carries the
    // owned rows away — while `fixture` deliberately pins the schema to the point
    // *before* this migration. An ops function issues `SELECT *` through the
    // current `users` entity, so calling one here couples a historical schema to
    // today's entity and every column added to `users` afterwards fails this test
    // for a reason it is not about (`totp_last_step`, card_9585caf5692d, was the
    // one that did).
    db.execute_unprepared("DELETE FROM users WHERE id = 2")
        .await
        .expect("delete the guest account");
    for table in [
        "repo_collaborators",
        "organization_members",
        "team_members",
        "notifications",
    ] {
        assert_eq!(
            scalar(&db, &format!("SELECT count(*) FROM {table}")).await,
            0,
            "deleting the account left its row in {table}"
        );
    }

    db.execute_unprepared(
        "INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
         VALUES (3, 'second-guest', 'second-guest@example.invalid', '',
                 CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
         INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
         VALUES (2, 1, 'doomed', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
         INSERT INTO repo_collaborators (repo_id, user_id, permission, created_at)
         VALUES (2, 3, 'read', CURRENT_TIMESTAMP);",
    )
    .await
    .expect("seed a collaborator on the repository that will be deleted");
    crate::ops::repo_ops::delete_by_id(&db, 2)
        .await
        .expect("delete the repository");
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM repo_collaborators WHERE repo_id = 2",
        )
        .await,
        0,
        "deleting the repository left its collaborator grant"
    );

    let error = db
        .execute_unprepared(
            "INSERT INTO notifications
                 (user_id, event_type, title, is_read, created_at)
             VALUES (999, 'issue', 'dangling', FALSE, CURRENT_TIMESTAMP)",
        )
        .await
        .expect_err("the installed constraint must reject a new orphan");
    assert!(
        error.to_string().contains("FOREIGN KEY"),
        "the orphan insert failed for the wrong reason: {error}"
    );
}

#[tokio::test]
async fn failure_mid_rebuild_rolls_every_table_back_and_can_retry() {
    let (db, _temp) = fixture("rollback-retry").await;
    let old_collaborators = table_sql(&db, "repo_collaborators").await;
    let old_members = table_sql(&db, "organization_members").await;
    let manager = SchemaManager::new(&db);

    let error = REBUILD
        .sqlite_with_hook(&manager, true, |point| async move {
            if point == SqliteRebuildPoint::TableRebuilt("repo_collaborators") {
                Err(sea_orm::DbErr::Custom(
                    "injected failure after rebuilding repo_collaborators".to_string(),
                ))
            } else {
                Ok(())
            }
        })
        .await
        .expect_err("the injected failure must escape");
    assert!(error.to_string().contains("injected failure"));
    assert_eq!(
        table_sql(&db, "repo_collaborators").await,
        old_collaborators
    );
    assert_eq!(table_sql(&db, "organization_members").await, old_members);
    assert_reference_shape(&db, false).await;
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM repo_collaborators").await,
        3,
        "the rolled-back cleanup lost historical rows"
    );

    REBUILD
        .sqlite(&manager, true)
        .await
        .expect("retry the same rebuild");
    assert_reference_shape(&db, true).await;
    assert_rebuild_invariants(&db).await;
}

#[tokio::test]
async fn down_removes_only_the_references_this_migration_added() {
    let (db, _temp) = fixture("down").await;
    let manager = SchemaManager::new(&db);
    REBUILD
        .sqlite(&manager, true)
        .await
        .expect("install the required references");
    REBUILD
        .sqlite(&manager, false)
        .await
        .expect("remove the required references again");
    assert_reference_shape(&db, false).await;
    assert_rebuild_invariants(&db).await;

    db.execute_unprepared("DELETE FROM users WHERE id = 2")
        .await
        .expect("delete the guest after reversing the migration");
    for table in [
        "repo_collaborators",
        "organization_members",
        "team_members",
        "notifications",
    ] {
        assert_eq!(
            scalar(&db, &format!("SELECT count(*) FROM {table}")).await,
            1,
            "down left an account cascade on {table}"
        );
    }
}
