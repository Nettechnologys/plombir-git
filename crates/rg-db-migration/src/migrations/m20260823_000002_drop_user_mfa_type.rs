//! Drop `users.mfa_type` — a column enrolment writes and the second factor
//! never consults.
//!
//! Turning MFA on stores `"totp"` here (`user_ops::enable_mfa`), and nothing
//! reads it back: the challenge path branches on `mfa_enabled` and on whether a
//! TOTP secret is stored, which is what actually decides whether a code can be
//! checked (card_b70de2169bd6). A column whose value cannot change any
//! behaviour is a column that describes the account incorrectly the moment
//! anything else writes to it.
//!
//! ## The preflight
//!
//! `up` refuses to run if any account holds a value outside `NULL` / `"totp"`.
//! Not because such a value works today — it does not, an `"sms"` account is
//! already an account whose only usable factor is TOTP — but because it is the
//! *only* record that somebody once meant something else. Destroying it
//! silently is how an operator finds out months later, from nothing. The
//! refusal names the account ids so the answer is a two-minute look rather than
//! an investigation, and this migration is idempotent, so fixing the rows and
//! restarting is the whole remedy.
//!
//! `down` restores the column empty. Every live account would write `"totp"`
//! into it again on the next enrolment, and the value it held before was, by
//! construction, one nothing read.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260823_000002_drop_user_mfa_type"
    }
}

/// Ids only. The column is not a credential, but a migration that prints
/// account state into a boot log is a habit, and the next column it is applied
/// to might be.
const UNEXPECTED_VALUES: &str =
    "SELECT id FROM users WHERE mfa_type IS NOT NULL AND mfa_type <> 'totp' ORDER BY id";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "mfa_type").await? {
            return Ok(());
        }

        let db = manager.get_connection();
        let backend = db.get_database_backend();
        let unexpected = db
            .query_all(Statement::from_string(
                backend,
                UNEXPECTED_VALUES.to_string(),
            ))
            .await?;
        if !unexpected.is_empty() {
            let ids = unexpected
                .iter()
                .map(|row| row.try_get::<i64>("", "id"))
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(DbErr::Custom(format!(
                "refusing to drop `users.mfa_type`: {} account(s) hold a value this server \
                 cannot honour — ids {ids}. Plombir Git only ever checks TOTP, so those accounts \
                 already have no second factor beyond it, and this column is the last record \
                 that something else was intended. Decide what those accounts should be \
                 (`UPDATE users SET mfa_type = 'totp'` if TOTP is enrolled, `NULL` if the second \
                 factor should be off) and start the server again — this migration is \
                 idempotent and will then run.",
                unexpected.len()
            )));
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::MfaType)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "mfa_type").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(ColumnDef::new(Users::MfaType).string_len(20).null())
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    MfaType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, DatabaseConnection, DbBackend};

    const SCHEMA: &str = "\
        CREATE TABLE users (\
            id INTEGER PRIMARY KEY, username TEXT NOT NULL, mfa_enabled BOOLEAN NOT NULL,\
            mfa_type TEXT, totp_secret TEXT, deleted_at TEXT\
        );";

    async fn fixture(rows: &str) -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(SCHEMA).await.unwrap();
        db.execute_unprepared(rows).await.unwrap();
        db
    }

    /// The ordinary instance: TOTP enrolments and accounts with no second
    /// factor, which is every account Plombir Git itself has ever written.
    #[tokio::test]
    async fn the_column_goes_and_the_accounts_stay() {
        let db = fixture(
            "INSERT INTO users (id, username, mfa_enabled, mfa_type, totp_secret) VALUES \
                (1, 'alice', 1, 'totp', 'secret'), (2, 'bob', 0, NULL, NULL);",
        )
        .await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        assert!(!manager.has_column("users", "mfa_type").await.unwrap());
        let survivors = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM users WHERE mfa_enabled AND totp_secret IS NOT NULL"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            survivors.try_get::<i64>("", "n").unwrap(),
            1,
            "the enrolment itself must be untouched — only the unread label goes"
        );
    }

    /// The reason the preflight exists: a value the server cannot honour is the
    /// one thing this migration must not destroy quietly.
    #[tokio::test]
    async fn a_value_this_server_cannot_honour_stops_the_migration() {
        let db = fixture(
            "INSERT INTO users (id, username, mfa_enabled, mfa_type, totp_secret) VALUES \
                (1, 'alice', 1, 'totp', 'secret'), (9, 'carol', 1, 'sms', NULL);",
        )
        .await;
        let manager = SchemaManager::new(&db);

        let refusal = Migration
            .up(&manager)
            .await
            .expect_err("an unhonourable value must stop the migration");
        let message = format!("{refusal}");
        assert!(
            message.contains("ids 9"),
            "the refusal must name the accounts to look at: {message}"
        );
        assert!(
            manager.has_column("users", "mfa_type").await.unwrap(),
            "a refused migration must not have dropped anything"
        );

        // And the documented remedy actually unblocks it.
        db.execute_unprepared("UPDATE users SET mfa_type = NULL WHERE id = 9;")
            .await
            .unwrap();
        Migration.up(&manager).await.unwrap();
        assert!(!manager.has_column("users", "mfa_type").await.unwrap());
    }

    #[tokio::test]
    async fn down_restores_the_column_empty() {
        let db = fixture(
            "INSERT INTO users (id, username, mfa_enabled, mfa_type, totp_secret) VALUES \
                (1, 'alice', 1, 'totp', 'secret');",
        )
        .await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();

        Migration.down(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();

        assert!(manager.has_column("users", "mfa_type").await.unwrap());
        let filled = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM users WHERE mfa_type IS NOT NULL".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(filled.try_get::<i64>("", "n").unwrap(), 0);
    }
}
