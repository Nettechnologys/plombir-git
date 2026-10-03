//! card_b70de2169bd6: upgrading a populated instance drops four columns and
//! nothing else.
//!
//! Four columns had a writer and no reader — `oci_manifest.push_by`,
//! `users.mfa_type`, `users.ldap_dn`, `users.backup_codes` — and the migrations
//! that remove them run, unattended, against databases that already hold
//! people's accounts and repositories. A unit test per migration proves the
//! column goes; this file proves the *upgrade* is safe, which is a different
//! claim and the one an operator is actually betting on.
//!
//! The old schema is not hand-written. `Migrator::down` puts the four columns
//! back through the very `down` implementations that ship, the fixture fills
//! them the way a pre-upgrade instance would have, and `Migrator::up` performs
//! the real upgrade over the real data. What is asserted afterwards is what an
//! operator would check: every account, repository, MFA enrolment and manifest
//! still there, the historical OCI attribution moved into the journal with its
//! original date, and the four columns gone from the schema the migrations
//! actually produced.
//!
//! The two preflights get their own run: a database holding the state each one
//! refuses must fail the upgrade with that state named, and must still have its
//! column — a refusal that half-applied would be worse than no refusal.

use rg_db::sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;

/// The first of the four column-removal migrations this upgrade test owns.
/// Locate it by name so newer migrations appended to the list cannot silently
/// move the fixture onto the wrong schema.
const FIRST_COLUMN_DROP: &str = "m20260823_000001_oci_manifest_push_audit";

fn upgrade_steps() -> u32 {
    let migrations = rg_db::migrations::Migrator::migrations();
    let first_drop = migrations
        .iter()
        .position(|migration| migration.name() == FIRST_COLUMN_DROP)
        .unwrap_or_else(|| panic!("{FIRST_COLUMN_DROP} must still be part of the migration list"));
    u32::try_from(migrations.len() - first_drop).expect("migration count fits in u32")
}

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-legacy-column-upgrade-{label}-{}.db",
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

/// A database on the schema this instance had *before* the four columns went,
/// built by reverting the shipped migrations rather than by transcribing them.
async fn instance_on_the_old_schema(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    // This fixture drives `Migrator` directly, so it does not get the pool-wide
    // schema refresh performed by `run_migrations` after SQLite DDL churn.
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    rg_db::migrations::Migrator::down(&db, Some(upgrade_steps()))
        .await
        .expect("revert to the pre-upgrade schema");

    for column in ["mfa_type", "ldap_dn", "backup_codes"] {
        assert_eq!(
            column_count(&db, "users", &[column]).await,
            1,
            "the fixture did not rebuild `users.{column}`, so this test would prove nothing"
        );
    }
    assert_eq!(
        column_count(&db, "oci_manifest", &["push_by"]).await,
        1,
        "the fixture did not rebuild `oci_manifest.push_by`"
    );

    (db, temp)
}

async fn column_count(db: &DatabaseConnection, table: &str, columns: &[&str]) -> i64 {
    let names = columns
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    scalar(
        db,
        &format!("SELECT COUNT(*) AS n FROM pragma_table_info('{table}') WHERE name IN ({names})"),
    )
    .await
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(
        DatabaseBackend::Sqlite,
        sql.to_string(),
    ))
    .await
    .unwrap_or_else(|error| panic!("query failed: {sql}: {error}"))
    .expect("a COUNT always answers")
    .try_get::<i64>("", "n")
    .expect("read the count")
}

/// What a populated instance looks like on the way in: two accounts, one of
/// them a directory account, a repository, a TOTP enrolment with a modern
/// recovery set, and two published images — one of them attributed to an
/// account that has since been deleted.
const POPULATED: &str = "\
    INSERT INTO users \
        (id, username, email, password_hash, is_admin, is_active, auth_provider, ldap_dn, \
         ldap_uid, ldap_provider_id, mfa_enabled, mfa_type, backup_codes, login_attempts, \
         session_version, created_at, updated_at) \
    VALUES \
        (1, 'alice', 'alice@example.invalid', 'argon2', 1, 1, 'local', NULL, NULL, NULL, \
         1, 'totp', '[\"legacy-hash\"]', 0, 0, '2026-05-01T00:00:00Z', '2026-05-01T00:00:00Z'),\
        (2, 'bob', 'bob@example.invalid', '', 0, 1, 'ldap', \
         'uid=bob,ou=people,dc=example,dc=invalid', 'bob', 1, 0, NULL, NULL, 0, 0, \
         '2026-05-02T00:00:00Z', '2026-05-02T00:00:00Z');\
    INSERT INTO mfa_backup_codes (user_id, code_hash, used, created_at) \
        VALUES (1, 'modern-hash', 0, '2026-05-01T00:00:00Z');\
    INSERT INTO repositories \
        (id, owner_id, name, description, is_private, default_branch, created_at, updated_at) \
        VALUES (1, 1, 'app', 'the repository this instance exists for', 0, 'main', \
                '2026-05-03T00:00:00Z', '2026-05-03T00:00:00Z');\
    INSERT INTO oci_repository (id, repo_id, namespace, owner_id, is_public, created_at, updated_at) \
        VALUES (1, 1, 'alice/app', 1, 0, '2026-05-03T00:00:00Z', '2026-05-03T00:00:00Z');\
    INSERT INTO oci_manifest \
        (id, oci_repository_id, digest, media_type, size, manifest_json, schema_version, push_by, \
         created_at, updated_at) \
    VALUES \
        (1, 1, 'sha256:1111', 'application/vnd.oci.image.manifest.v1+json', 2, '{}', 2, 1, \
         '2026-05-04T09:00:00Z', '2026-05-04T09:00:00Z'),\
        (2, 1, 'sha256:2222', 'application/vnd.oci.image.manifest.v1+json', 2, '{}', 2, 77, \
         '2026-05-05T09:00:00Z', '2026-05-05T09:00:00Z');";

/// The upgrade an operator runs on a live instance: everything they have is
/// still there afterwards, and the one thing that was only in a dropped column
/// moved somewhere they can read it.
#[tokio::test]
async fn upgrading_a_populated_instance_keeps_everything_but_the_four_columns() {
    let (db, _temp) = instance_on_the_old_schema("populated").await;
    db.execute_unprepared(POPULATED)
        .await
        .expect("seed an instance on the old schema");

    rg_db::run_migrations(&db).await.expect("run the upgrade");

    // Nothing that belongs to a person is gone.
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM users").await,
        2,
        "the upgrade lost an account"
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM repositories").await,
        1,
        "the upgrade lost a repository"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM users WHERE mfa_enabled AND id = 1"
        )
        .await,
        1,
        "the upgrade turned somebody's second factor off"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM mfa_backup_codes WHERE user_id = 1 AND NOT used"
        )
        .await,
        1,
        "the upgrade destroyed a live recovery code"
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM oci_manifest").await,
        2,
        "the upgrade lost a published image"
    );
    // The directory account keeps the pair that resolves its bind; only the DN
    // went.
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM users WHERE id = 2 AND ldap_uid = 'bob' \
             AND ldap_provider_id = 1"
        )
        .await,
        1,
        "the upgrade broke the identity an LDAP bind resolves through"
    );

    // The attribution that lived in `push_by` is in the journal, dated when the
    // push happened rather than when the migration ran.
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM audit_log WHERE action = 'oci.manifest.push'"
        )
        .await,
        2,
        "the historical OCI publishers did not reach the audit log"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM audit_log WHERE action = 'oci.manifest.push' \
             AND resource_id = 1 AND user_id = 1 AND username = 'alice' \
             AND resource_name = 'alice/app' AND created_at = '2026-05-04T09:00:00Z'"
        )
        .await,
        1,
        "the backfilled event does not name who published what, when"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM audit_log WHERE action = 'oci.manifest.push' \
             AND resource_id = 2 AND user_id = 77 AND username IS NULL"
        )
        .await,
        1,
        "a publisher whose account is gone must keep its id and record no name — a blank name \
         is indistinguishable from one that failed to load"
    );

    // And the schema the real migrations produced no longer has the columns.
    assert_eq!(
        column_count(&db, "users", &["mfa_type", "ldap_dn", "backup_codes"]).await,
        0,
        "`users` still carries a column with a writer and no reader"
    );
    assert_eq!(
        column_count(&db, "oci_manifest", &["push_by"]).await,
        0,
        "`oci_manifest` still carries a column with a writer and no reader"
    );
}

/// An instance where somebody's second factor is on, their only recovery
/// material is the legacy blob, and no modern set was ever issued. Dropping the
/// column is the moment that becomes unrecoverable, so the upgrade stops and
/// says who.
#[tokio::test]
async fn the_upgrade_stops_rather_than_strand_an_account_without_recovery() {
    let (db, _temp) = instance_on_the_old_schema("stranded").await;
    db.execute_unprepared(
        "INSERT INTO users \
            (id, username, email, password_hash, is_admin, is_active, auth_provider, \
             mfa_enabled, totp_secret, backup_codes, login_attempts, session_version, \
             created_at, updated_at) \
         VALUES (5, 'erin', 'erin@example.invalid', 'argon2', 0, 1, 'local', 1, 'secret', \
                 '[\"legacy-hash\"]', 0, 0, '2026-05-01T00:00:00Z', '2026-05-01T00:00:00Z');",
    )
    .await
    .expect("seed an account with only legacy recovery codes");

    let refusal = format!(
        "{:#}",
        rg_db::run_migrations(&db)
            .await
            .expect_err("the upgrade must refuse to strand an account")
    );
    assert!(
        refusal.contains("ids 5"),
        "the refusal must name who to talk to: {refusal}"
    );
    assert_eq!(
        column_count(&db, "users", &["backup_codes"]).await,
        1,
        "a refused upgrade dropped the column anyway"
    );

    // The remedy the refusal names, and the upgrade goes through.
    db.execute_unprepared(
        "INSERT INTO mfa_backup_codes (user_id, code_hash, used, created_at) \
         VALUES (5, 'modern-hash', 0, '2026-05-06T00:00:00Z');",
    )
    .await
    .unwrap();
    rg_db::run_migrations(&db)
        .await
        .expect("the documented remedy must unblock the upgrade");
    assert_eq!(column_count(&db, "users", &["backup_codes"]).await, 0);
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM users WHERE id = 5").await,
        1,
        "the account the refusal was protecting did not survive the upgrade"
    );
}

/// A second factor this server cannot honour is the one thing `mfa_type` could
/// still be telling anybody. The upgrade refuses rather than delete it quietly.
#[tokio::test]
async fn the_upgrade_stops_on_a_second_factor_this_server_cannot_honour() {
    let (db, _temp) = instance_on_the_old_schema("unhonourable").await;
    db.execute_unprepared(
        "INSERT INTO users \
            (id, username, email, password_hash, is_admin, is_active, auth_provider, \
             mfa_enabled, mfa_type, login_attempts, session_version, created_at, updated_at) \
         VALUES (9, 'carol', 'carol@example.invalid', 'argon2', 0, 1, 'local', 1, 'sms', 0, 0, \
                 '2026-05-01T00:00:00Z', '2026-05-01T00:00:00Z');",
    )
    .await
    .expect("seed an account enrolled in something this server never checked");

    let refusal = format!(
        "{:#}",
        rg_db::run_migrations(&db)
            .await
            .expect_err("the upgrade must refuse to erase an unhonourable enrolment")
    );
    assert!(
        refusal.contains("ids 9"),
        "the refusal must name the accounts to look at: {refusal}"
    );
    assert_eq!(
        column_count(&db, "users", &["mfa_type"]).await,
        1,
        "a refused upgrade dropped the column anyway"
    );

    db.execute_unprepared("UPDATE users SET mfa_type = 'totp' WHERE id = 9;")
        .await
        .unwrap();
    rg_db::run_migrations(&db)
        .await
        .expect("the documented remedy must unblock the upgrade");
    assert_eq!(column_count(&db, "users", &["mfa_type"]).await, 0);
}
