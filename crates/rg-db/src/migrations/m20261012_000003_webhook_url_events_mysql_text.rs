//! A webhook's target URL and its comma-joined event list are both values the
//! API accepts in full — every declared event at once, an HTTPS URL with a
//! long path or query — and both outgrow MySQL's default `VARCHAR(255)`.
//! Widen them in place so existing hooks keep their values.

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
                    "ALTER TABLE `webhooks` \
                     MODIFY COLUMN `url` TEXT NOT NULL, \
                     MODIFY COLUMN `events` TEXT NOT NULL",
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing either column could truncate a hook written since up.
        Ok(())
    }
}
