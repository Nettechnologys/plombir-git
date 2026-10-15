//! Migration: `webhooks.secret` held the HMAC signing key in the clear — give
//! the column the name the value is about to have.
//!
//! Every delivery Plombir Git sends is signed with this value
//! (`X-Hub-Signature-256`), so unlike a runner token it cannot be hashed: the
//! server has to read it back on every dispatch. That leaves encryption, the
//! same treatment `ci_secrets.encrypted_value` and `mirrors.password_encrypted`
//! already get — and the column joins
//! `rg_core::auth::encrypted_columns` so the startup preflight and
//! `plombir-git rotate-encryption-key` see it like any other at-rest secret.
//!
//! **The rename is all this migration can do.** Sealing the existing values
//! needs the instance's at-rest key, which lives in a key file / config and
//! never reaches the migration runner. So the values are sealed one step later,
//! by `rg_core::webhook::service::seal_legacy_secrets`, which `plombir-git serve`
//! runs immediately after the key preflight — the first moment in the boot
//! where both the migrated schema and the key exist. Until that pass runs the
//! column name promises more than the bytes deliver; the dispatcher knows it
//! and reads a value that is structurally not our ciphertext as the legacy
//! plaintext it is, rather than signing with garbage.
//!
//! There is no `down`: the rename is half of a change whose other half is
//! ciphertext, and renaming back would leave `secret` holding sealed values
//! under a name that promises the opposite.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260804_000003_rename_webhook_secret_encrypted"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("webhooks").await? {
            return Ok(());
        }
        // A database that has already run this migration has no `secret` left
        // to rename; re-running must not fail on the missing column.
        if !manager.has_column("webhooks", "secret").await? {
            return Ok(());
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("webhooks"))
                    .rename_column(Alias::new("secret"), Alias::new("secret_encrypted"))
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
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    const SCHEMA: &str = "CREATE TABLE webhooks (id INTEGER PRIMARY KEY, repo_id BIGINT NOT NULL, \
         url TEXT NOT NULL, content_type TEXT NOT NULL, secret TEXT, \
         active BOOLEAN NOT NULL DEFAULT 1, events TEXT NOT NULL);";

    /// The value is carried over untouched — sealing it is the startup pass's
    /// job, and a hook whose secret vanished here would start signing nothing.
    #[tokio::test]
    async fn the_column_is_renamed_and_keeps_its_value() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(&format!(
            "{SCHEMA}\
             INSERT INTO webhooks (id, repo_id, url, content_type, secret, events) VALUES \
               (1, 7, 'https://example.com/hook', 'json', 'hunter2', 'push');"
        ))
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT secret_encrypted FROM webhooks WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<Option<String>>("", "secret_encrypted")
                .unwrap(),
            Some("hunter2".to_string())
        );
    }

    /// Migrations re-run from the top after an interrupted boot.
    #[tokio::test]
    async fn a_second_run_is_a_no_op() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(SCHEMA).await.unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }

    /// A database that never reached the webhook migrations must not make the
    /// migration runner fail.
    #[tokio::test]
    async fn is_a_no_op_without_the_webhooks_table() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }
}
