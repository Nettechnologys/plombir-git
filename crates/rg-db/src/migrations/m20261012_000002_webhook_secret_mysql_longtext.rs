//! A webhook signing secret grows after sealing and base64 encoding. Preserve
//! existing ciphertext while removing MySQL's old VARCHAR(255) ceiling.

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
                    "ALTER TABLE `webhooks` MODIFY COLUMN `secret_encrypted` LONGTEXT NULL",
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing could truncate ciphertext written after this migration.
        Ok(())
    }
}
