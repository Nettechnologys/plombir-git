//! Accepted mirror and import addresses can exceed MySQL's default VARCHAR(255).
//! Widen existing columns in place so live rows survive an upgrade.

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
                    "ALTER TABLE `mirrors` \
                     MODIFY COLUMN `url` TEXT NOT NULL, \
                     MODIFY COLUMN `username` TEXT NULL",
                )
                .await?;
            manager
                .get_connection()
                .execute_unprepared(
                    "ALTER TABLE `import_tasks` MODIFY COLUMN `source_url` TEXT NOT NULL",
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing after accepting longer values would truncate live rows.
        Ok(())
    }
}
