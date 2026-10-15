//! Drop `users.backup_codes` — the recovery-code store that stopped being the
//! recovery-code store.
//!
//! Before `mfa_backup_codes` existed, an account's recovery codes were a JSON
//! array of hashes in this column. The table replaced it, every live path uses
//! the table — issue, list, verify-and-consume, revoke — and nothing has read
//! the column since (card_b70de2169bd6). What is left on an instance that
//! predates the change is hashes of live-looking recovery codes that no login
//! will ever accept, in a column no rekey pass touches and no deletion path
//! clears. That is credential-shaped material kept past the last moment it
//! could do any good, which is the thing this repository's `Credential
//! Strength` work exists to stop.
//!
//! The hashes are **not** carried into the new table. They are SHA-256 of
//! six-digit codes, which is a keyspace a laptop exhausts; the modern set is
//! deliberately stronger. Reviving them would put weak recovery secrets back
//! into the live authentication path — the opposite of why the table exists.
//!
//! ## The preflight
//!
//! `up` refuses only for the case where dropping actually takes something away:
//! a live account, with the second factor **on**, holding legacy codes and
//! having no row at all in `mfa_backup_codes`. Those are the people who would
//! be left with a TOTP app and no way back if they lose it — and the column
//! already gives them nothing, so the finding is worth a stop rather than a
//! line in a log nobody re-reads. The remedy is theirs to run and takes a
//! minute: `POST /users/mfa/backup/regenerate` issues a modern set, after which
//! this migration runs.
//!
//! Everything else — MFA off, the account soft-deleted, or a modern set already
//! issued — is dropped without ceremony, because for those rows the column is
//! demonstrably dead weight.
//!
//! `down` restores the column empty, and deliberately so: the hashes are what
//! this migration exists to destroy.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260823_000004_drop_user_backup_codes"
    }
}

/// Accounts whose only recovery material is the legacy blob.
///
/// `WHERE u.mfa_enabled` without a comparison on purpose: the column is a
/// boolean on PostgreSQL, an integer on SQLite and a `TINYINT` on MySQL, and
/// only the bare predicate reads the same on all three. Ids only — the hashes
/// must not reach a boot log.
const STRANDED_ACCOUNTS: &str = "\
    SELECT u.id FROM users u \
    WHERE u.backup_codes IS NOT NULL \
      AND u.mfa_enabled \
      AND u.deleted_at IS NULL \
      AND NOT EXISTS (SELECT 1 FROM mfa_backup_codes c WHERE c.user_id = u.id) \
    ORDER BY u.id";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "backup_codes").await? {
            return Ok(());
        }

        let db = manager.get_connection();
        let backend = db.get_database_backend();
        let stranded = db
            .query_all(Statement::from_string(
                backend,
                STRANDED_ACCOUNTS.to_string(),
            ))
            .await?;
        if !stranded.is_empty() {
            let ids = stranded
                .iter()
                .map(|row| row.try_get::<i64>("", "id"))
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(DbErr::Custom(format!(
                "refusing to drop `users.backup_codes`: {} account(s) have the second factor on, \
                 hold only the legacy recovery codes, and have no row in `mfa_backup_codes` — \
                 ids {ids}. Those codes already do not work (no login path has read this column \
                 since `mfa_backup_codes` replaced it), so those accounts have no way back if \
                 they lose their TOTP device, and dropping the column would remove the last sign \
                 of it. Have each of them call `POST /users/mfa/backup/regenerate` — or turn the \
                 second factor off — and start the server again; this migration is idempotent \
                 and will then run. The old hashes are deliberately not migrated: they are \
                 SHA-256 of six-digit codes, and reviving them would put weak recovery secrets \
                 back into the login path.",
                stranded.len()
            )));
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::BackupCodes)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "backup_codes").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(ColumnDef::new(Users::BackupCodes).text().null())
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
    BackupCodes,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, DatabaseConnection, DbBackend};

    const SCHEMA: &str = "\
        CREATE TABLE users (\
            id INTEGER PRIMARY KEY, username TEXT NOT NULL, mfa_enabled BOOLEAN NOT NULL,\
            totp_secret TEXT, backup_codes TEXT, deleted_at TEXT\
        );\
        CREATE TABLE mfa_backup_codes (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, user_id BIGINT NOT NULL,\
            code_hash TEXT NOT NULL, used BOOLEAN NOT NULL\
        );";

    async fn fixture(rows: &str) -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(SCHEMA).await.unwrap();
        db.execute_unprepared(rows).await.unwrap();
        db
    }

    async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
        db.query_one(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap()
    }

    /// The three shapes the column is genuinely dead in: MFA off, a modern set
    /// already issued, and an account already deleted. None of them may hold up
    /// a boot, and none of them may lose an account.
    #[tokio::test]
    async fn dead_weight_goes_and_every_account_stays() {
        let db = fixture(
            "INSERT INTO users (id, username, mfa_enabled, totp_secret, backup_codes, deleted_at) \
             VALUES (1, 'alice', 1, 'secret', '[\"legacy\"]', NULL),\
                    (2, 'bob', 0, NULL, '[\"legacy\"]', NULL),\
                    (3, 'carol', 1, 'secret', '[\"legacy\"]', '2026-01-01T00:00:00Z'),\
                    (4, 'dave', 1, 'secret', NULL, NULL);\
             INSERT INTO mfa_backup_codes (user_id, code_hash, used) VALUES (1, 'modern', 0);",
        )
        .await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        assert!(!manager.has_column("users", "backup_codes").await.unwrap());
        assert_eq!(
            count(&db, "SELECT COUNT(*) AS n FROM users").await,
            4,
            "this migration destroys a column, never an account"
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) AS n FROM mfa_backup_codes").await,
            1,
            "the modern recovery set must be untouched"
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) AS n FROM users WHERE mfa_enabled AND totp_secret IS NOT NULL"
            )
            .await,
            3,
            "the second factor itself must be untouched"
        );
    }

    /// The case the preflight exists for, and the remedy the refusal names.
    #[tokio::test]
    async fn an_account_left_without_recovery_stops_the_migration() {
        let db = fixture(
            "INSERT INTO users (id, username, mfa_enabled, totp_secret, backup_codes, deleted_at) \
             VALUES (5, 'erin', 1, 'secret', '[\"legacy\"]', NULL);",
        )
        .await;
        let manager = SchemaManager::new(&db);

        let refusal = Migration
            .up(&manager)
            .await
            .expect_err("an account with no modern recovery set must stop the migration");
        let message = format!("{refusal}");
        assert!(
            message.contains("ids 5"),
            "the refusal must name who to talk to: {message}"
        );
        assert!(
            manager.has_column("users", "backup_codes").await.unwrap(),
            "a refused migration must not have dropped anything"
        );

        // The documented remedy: a modern set is issued, and the boot proceeds.
        db.execute_unprepared(
            "INSERT INTO mfa_backup_codes (user_id, code_hash, used) VALUES (5, 'modern', 0);",
        )
        .await
        .unwrap();
        Migration.up(&manager).await.unwrap();
        assert!(!manager.has_column("users", "backup_codes").await.unwrap());
    }

    #[tokio::test]
    async fn down_restores_the_column_empty() {
        let db = fixture(
            "INSERT INTO users (id, username, mfa_enabled, totp_secret, backup_codes, deleted_at) \
             VALUES (1, 'alice', 0, NULL, '[\"legacy\"]', NULL);",
        )
        .await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();

        Migration.down(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();

        assert!(manager.has_column("users", "backup_codes").await.unwrap());
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) AS n FROM users WHERE backup_codes IS NOT NULL"
            )
            .await,
            0,
            "the hashes are the thing this migration exists to destroy"
        );
    }
}
