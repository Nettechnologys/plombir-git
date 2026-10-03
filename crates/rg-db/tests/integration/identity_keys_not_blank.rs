//! card_8932bde0c931: the code-side gate on empty identity keys
//! (`card_0a08de4d6707`) guards one entry point; the schema underneath it still
//! let `''` occupy a `UNIQUE` slot. These exercise the schema, not the service:
//! every write below goes in as raw SQL, past every Rust validator there is,
//! because the whole point of the migration is to catch the writer that did not
//! come through them.
//!
//! What each test guards:
//!
//! * **Insert is refused** — one blank row is all it takes to restore the
//!   original defect, since the next lookup for `""` finds it.
//! * **Update is refused** — a row can be blanked after it was created, so the
//!   `BEFORE INSERT` half alone would leave the door open.
//! * **`NULL` is still allowed** — `users.ldap_uid` is nullable on purpose:
//!   "this account has no directory identity" must stay expressible.
//! * **An existing blank row stops the upgrade by name** — an operator meeting
//!   this must be told which rows to repair, not just that a constraint exists.

use rg_db::sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::MigratorTrait;

/// The migration under test, located by name: a count-based index silently
/// starts testing a different step the moment another migration lands.
const MIGRATION: &str = "m20260802_000003_identity_keys_not_blank";

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "plombir-git-identity-keys-{label}-{}.db",
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

/// One pooled connection on purpose.
///
/// These tests drive the migrator by hand — up to a named step, back down over
/// every migration after it, and up again — so they never pass through
/// `run_migrations`, whose closing `refresh_sqlite_pool_after_schema_change` is
/// what leaves *every* connection of a pool usable after that much schema
/// churn. Without it a second connection can answer the first statement it runs
/// afterwards with a bare `no such table: users` while the table is plainly
/// there (card_a28a7004b108).
///
/// Measured on this file at twelve-way parallelism: 12 of 120 runs failed on
/// the two-connection pool this used to open, 98 of 120 with four connections
/// warmed, and 0 of 300 on one (card_8d8e59fc4160). None of it has anything to
/// do with what the file asserts, which is what the schema refuses.
async fn connect_test_db(temp: &TempDb) -> DatabaseConnection {
    rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway database")
}

/// Index of the migration under test in the list, i.e. how many steps to run to
/// arrive at the schema as it was one step before it.
fn steps_before_migration() -> u32 {
    let position = rg_db::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == MIGRATION)
        .unwrap_or_else(|| panic!("{MIGRATION} must still be part of the migration list"));
    u32::try_from(position).expect("migration index fits in u32")
}

/// How many steps back from head undo the migration under test, inclusive.
///
/// `down(Some(1))` was the same bug this file warns about at the top, one
/// direction over: it means "undo the newest migration", which stopped being
/// this one the moment another migration landed after it — and the failure read
/// as "the constraint is not rolled back" rather than "you rolled back somebody
/// else's work".
fn steps_back_to_migration() -> u32 {
    let migrations = rg_db::migrations::Migrator::migrations();
    let position = migrations
        .iter()
        .position(|migration| migration.name() == MIGRATION)
        .unwrap_or_else(|| panic!("{MIGRATION} must still be part of the migration list"));
    u32::try_from(migrations.len() - position).expect("migration count fits in u32")
}

async fn execute(db: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    db.execute(Statement::from_string(
        rg_db::sea_orm::DatabaseBackend::Sqlite,
        sql.to_string(),
    ))
    .await
    .map(|_| ())
}

/// Insert a user straight into the table, bypassing every Rust-side validator.
fn insert_user_sql(id: i64, username: &str, email: &str, ldap_uid: &str) -> String {
    let ldap_uid = if ldap_uid == "<null>" {
        "NULL".to_string()
    } else {
        format!("'{ldap_uid}'")
    };
    format!(
        "INSERT INTO users \
         (id, username, email, password_hash, is_admin, is_active, auth_provider, ldap_uid, \
          mfa_enabled, login_attempts, session_version, created_at, updated_at) \
         VALUES ({id}, '{username}', '{email}', '', 0, 1, 'local', {ldap_uid}, \
          0, 0, 0, '2026-08-02 00:00:00+00:00', '2026-08-02 00:00:00+00:00')"
    )
}

fn insert_oauth_account_sql(id: i64, user_id: i64, provider_user_id: &str) -> String {
    format!(
        "INSERT INTO oauth_accounts \
         (id, provider, provider_user_id, provider_username, email, user_id, created_at, updated_at) \
         VALUES ({id}, 'github', '{provider_user_id}', 'someone', 'someone@example.invalid', \
          {user_id}, '2026-08-02 00:00:00+00:00', '2026-08-02 00:00:00+00:00')"
    )
}

#[tokio::test]
async fn the_database_refuses_a_blank_identity_key_on_insert_and_on_update() {
    let temp = TempDb::new("insert");
    let db = connect_test_db(&temp).await;
    rg_db::migrations::Migrator::up(&db, None)
        .await
        .expect("migrate to head");

    execute(
        &db,
        &insert_user_sql(1, "real", "real@example.invalid", "uid-1"),
    )
    .await
    .expect("baseline: a fully populated user is accepted");

    // ── Insert ──────────────────────────────────────────────────────────────
    for (label, sql) in [
        (
            "blank email",
            insert_user_sql(10, "blankmail", "", "<null>"),
        ),
        (
            "whitespace-only email",
            insert_user_sql(11, "spacemail", "   ", "<null>"),
        ),
        (
            "blank username",
            insert_user_sql(12, "", "blankname@example.invalid", "<null>"),
        ),
        (
            "blank ldap uid",
            insert_user_sql(13, "blankuid", "blankuid@example.invalid", ""),
        ),
    ] {
        let error = execute(&db, &sql)
            .await
            .expect_err(&format!("{label} must be refused by the database"));
        assert!(
            !rg_db::is_unique_violation(&error),
            "{label}: the refusal must not look like a lost race — \
             the recovery paths resolve only UNIQUE violations ({error})"
        );
    }

    let blank_provider_uid = insert_oauth_account_sql(20, 1, "");
    execute(&db, &blank_provider_uid)
        .await
        .expect_err("a blank provider_user_id must be refused by the database");

    execute(&db, &insert_oauth_account_sql(21, 1, "gh-1"))
        .await
        .expect("baseline: a populated oauth link is accepted");

    // ── Update ──────────────────────────────────────────────────────────────
    execute(&db, "UPDATE users SET email = '' WHERE id = 1")
        .await
        .expect_err("blanking an email after the fact must be refused too");
    execute(&db, "UPDATE users SET username = ' ' WHERE id = 1")
        .await
        .expect_err("blanking a username after the fact must be refused too");
    execute(&db, "UPDATE users SET ldap_uid = '' WHERE id = 1")
        .await
        .expect_err("blanking an ldap uid after the fact must be refused too");
    execute(
        &db,
        "UPDATE oauth_accounts SET provider_user_id = '' WHERE id = 21",
    )
    .await
    .expect_err("blanking a provider_user_id after the fact must be refused too");

    // ── What must still be allowed ──────────────────────────────────────────
    execute(&db, "UPDATE users SET ldap_uid = NULL WHERE id = 1")
        .await
        .expect("NULL means 'no directory identity' and stays legal");
    execute(
        &db,
        &insert_user_sql(30, "nodirectory", "nodirectory@example.invalid", "<null>"),
    )
    .await
    .expect("a user with no LDAP uid at all stays legal");
    execute(
        &db,
        "UPDATE users SET display_name = 'Renamed' WHERE id = 1",
    )
    .await
    .expect("an update that does not touch an identity key is unaffected");

    // ── The rollback is a real rollback ─────────────────────────────────────
    // A `down` that leaves its triggers behind makes the migration impossible to
    // step back over, and the failure would only show up during an incident.
    rg_db::migrations::Migrator::down(&db, Some(steps_back_to_migration()))
        .await
        .expect("roll the constraint back");
    execute(&db, &insert_user_sql(50, "afterdown", "", "<null>"))
        .await
        .expect("with the constraint rolled back the blank row goes in again");

    execute(&db, "DELETE FROM users WHERE id = 50")
        .await
        .expect("clear the blank row the rollback allowed");
    rg_db::migrations::Migrator::up(&db, None)
        .await
        .expect("and the constraint can be applied again");
    execute(&db, &insert_user_sql(51, "afterup", "", "<null>"))
        .await
        .expect_err("re-applied, it is in force again");
}

#[tokio::test]
async fn an_existing_blank_key_stops_the_upgrade_and_names_the_rows() {
    let temp = TempDb::new("preflight");
    let db = connect_test_db(&temp).await;

    rg_db::migrations::Migrator::up(&db, Some(steps_before_migration()))
        .await
        .expect("migrate up to the step before the constraint");

    // The row an older build could have written: `''` occupying the unique slot.
    execute(&db, &insert_user_sql(41, "ghost", "", "<null>"))
        .await
        .expect("baseline: without the constraint the blank row goes in");
    execute(
        &db,
        &insert_user_sql(42, "ghost2", "ghost2@example.invalid", ""),
    )
    .await
    .expect("baseline: a blank ldap uid goes in too");

    let error = rg_db::migrations::Migrator::up(&db, None)
        .await
        .expect_err("the upgrade must refuse to run over a blank identity key");
    let message = format!("{error:#}");

    for expected in [
        "users.email",
        "users.ldap_uid",
        "41",
        "42",
        "card_0a08de4d6707",
    ] {
        assert!(
            message.contains(expected),
            "the refusal must name {expected} so the operator can repair it — got: {message}"
        );
    }

    // The refusal is a refusal, not a half-applied upgrade: repair the rows and
    // the same upgrade goes through.
    execute(
        &db,
        "UPDATE users SET email = 'ghost@example.invalid' WHERE id = 41",
    )
    .await
    .expect("repair the blank email");
    execute(&db, "UPDATE users SET ldap_uid = NULL WHERE id = 42")
        .await
        .expect("repair the blank ldap uid");

    rg_db::migrations::Migrator::up(&db, None)
        .await
        .expect("after the repair the upgrade completes");

    execute(&db, &insert_user_sql(43, "late", "", "<null>"))
        .await
        .expect_err("and the constraint is in force afterwards");
}
