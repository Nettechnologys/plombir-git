//! Migration: `runners.token` held the bearer token in the clear — hash it and
//! rename the column to say so.
//!
//! Every other credential in this schema that is only ever *checked* is stored
//! as a digest (`access_tokens.token_hash`, `password_reset_tokens.token_hash`)
//! or as ciphertext (`ci_secrets.encrypted_value`, `mirrors.password_encrypted`,
//! `sso_providers.client_secret_enc`). The runner token was the one holdout, and
//! it is a full bearer credential: it opens the CI API — jobs, artifacts, the
//! build cache. A database dump handed those out ready to use.
//!
//! Nothing needs the plaintext. It is shown once, in the response to
//! `POST /runners/register`, out of a value that is still in memory; from then
//! on it only ever arrives from the outside and is compared. So the fix is the
//! PAT scheme: store `sha256(token)`, look up by the hash of what was presented.
//!
//! **Tokens already issued keep working.** The existing values are hashed in
//! place rather than regenerated, so no runner has to be re-registered — the
//! token in a runner's config file still authenticates, it simply no longer
//! matches anything a dump would show.
//!
//! Order matters: the values are hashed *before* the column is renamed. A run
//! that dies halfway leaves the migration unrecorded and re-runs from the top,
//! and the "already 64 hex characters" filter makes the second pass skip what
//! the first one finished — a generated token is 32 hex characters, so the two
//! shapes cannot be confused. Hashing after the rename would have had no such
//! resting point.
//!
//! There is no `down`: SHA-256 is what this migration exists to apply, and the
//! plaintext is not recoverable from here.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DbBackend, Statement};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260804_000002_hash_runner_tokens"
    }
}

/// Whether a stored value is already a SHA-256 hex digest.
///
/// A runner token is 32 hex characters (a v4 UUID minus its hyphens), a digest
/// is 64, so length alone separates them — no value can be mistaken for the
/// other kind.
fn looks_hashed(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("runners").await? {
            return Ok(());
        }
        // A fresh database is created with the column already named
        // `token_hash` by nothing — the create migration writes `token` — but a
        // database that has run this migration before must not have the rename
        // attempted twice.
        if !manager.has_column("runners", "token").await? {
            return Ok(());
        }

        let db = manager.get_connection();
        let backend = db.get_database_backend();

        let rows = db
            .query_all(Statement::from_string(
                backend,
                "SELECT id, token FROM runners".to_string(),
            ))
            .await?;

        for row in rows {
            let id: i64 = row.try_get("", "id")?;
            let token: String = row.try_get("", "token")?;
            if looks_hashed(&token) {
                continue;
            }
            db.execute(Statement::from_sql_and_values(
                backend,
                match backend {
                    DbBackend::Postgres => "UPDATE runners SET token = $1 WHERE id = $2",
                    _ => "UPDATE runners SET token = ? WHERE id = ?",
                },
                [sha256_hex(&token).into(), id.into()],
            ))
            .await?;
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("runners"))
                    .rename_column(Alias::new("token"), Alias::new("token_hash"))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::Database;

    const SCHEMA: &str = "CREATE TABLE runners (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
         token TEXT NOT NULL UNIQUE, status TEXT NOT NULL DEFAULT 'offline');";

    #[tokio::test]
    async fn an_issued_token_survives_as_its_hash_and_not_as_itself() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(&format!(
            "{SCHEMA}\
             INSERT INTO runners (id, name, token) VALUES \
               (1, 'builder', '4f9c1e2a7b3d48f0a1c25e6d7b8f0912'), \
               (2, 'spare', 'aa11bb22cc33dd44ee55ff6677889900');"
        ))
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT token_hash FROM runners WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let stored: String = row.try_get("", "token_hash").unwrap();

        // The one thing that matters: the plaintext is gone, and the token the
        // runner already holds still resolves to the row.
        assert_ne!(stored, "4f9c1e2a7b3d48f0a1c25e6d7b8f0912");
        assert_eq!(stored, sha256_hex("4f9c1e2a7b3d48f0a1c25e6d7b8f0912"));

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM runners WHERE token_hash IN \
                 ('4f9c1e2a7b3d48f0a1c25e6d7b8f0912', 'aa11bb22cc33dd44ee55ff6677889900')"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<i64>("", "n").unwrap(),
            0,
            "a plaintext runner token survived the migration"
        );
    }

    /// The unique constraint has to survive the rename, or two runners could
    /// end up sharing a credential.
    #[tokio::test]
    async fn the_hashed_column_is_still_unique() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(&format!(
            "{SCHEMA}INSERT INTO runners (id, name, token) VALUES (1, 'builder', 'abc123');"
        ))
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let duplicate = db
            .execute_unprepared(&format!(
                "INSERT INTO runners (id, name, token_hash) VALUES (2, 'clone', '{}');",
                sha256_hex("abc123")
            ))
            .await;
        assert!(
            duplicate.is_err(),
            "the uniqueness of the runner credential was lost in the rename"
        );
    }

    /// A half-finished run must be resumable: the rows the first pass hashed
    /// are left alone by the second instead of being hashed twice, which would
    /// have locked out every runner it had already migrated.
    #[tokio::test]
    async fn a_resumed_run_does_not_hash_an_already_hashed_value_twice() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(&format!(
            "{SCHEMA}INSERT INTO runners (id, name, token) VALUES (1, 'builder', '{}'), \
             (2, 'spare', 'aa11bb22cc33dd44ee55ff6677889900');",
            sha256_hex("4f9c1e2a7b3d48f0a1c25e6d7b8f0912")
        ))
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT token_hash FROM runners WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<String>("", "token_hash").unwrap(),
            sha256_hex("4f9c1e2a7b3d48f0a1c25e6d7b8f0912"),
            "the resumed pass re-hashed a value the first pass had finished"
        );
    }

    /// Running the migration a second time on an already-migrated database has
    /// to be a no-op rather than a failed rename.
    #[tokio::test]
    async fn is_idempotent() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(&format!(
            "{SCHEMA}INSERT INTO runners (id, name, token) VALUES (1, 'builder', 'abc123');"
        ))
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }

    /// A database that never reached the runner migrations must not make the
    /// migration runner fail.
    #[tokio::test]
    async fn is_a_no_op_without_the_runners_table() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }
}
