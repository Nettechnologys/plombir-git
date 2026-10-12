//! A delivered webhook payload can be much larger than MySQL's default
//! `VARCHAR(255)`. Preserve existing delivery rows while widening both
//! variable-length bodies that can otherwise make the journal insert fail.

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
                    "ALTER TABLE `webhook_deliveries` \
                     MODIFY COLUMN `request_payload` LONGTEXT NULL, \
                     MODIFY COLUMN `response_body` LONGTEXT NULL",
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing either column could truncate a delivery written since up.
        Ok(())
    }
}
