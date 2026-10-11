//! Existing MySQL installations recorded the old CI secret TEXT column.
//! Widening it preserves ciphertext already stored there.

use sea_orm::{ConnectionTrait, DatabaseBackend};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() == DatabaseBackend::MySql {
            manager
                .get_connection()
                .execute_unprepared(
                    "ALTER TABLE `ci_secrets` MODIFY COLUMN `encrypted_value` LONGTEXT NOT NULL",
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // A ciphertext written after this migration may exceed TEXT. Narrowing
        // the column on rollback would fail or truncate a live secret.
        Ok(())
    }
}
