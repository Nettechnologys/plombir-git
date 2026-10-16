//! A job's `image:` did not fit its column on MySQL (card_51b860efc19c).
//!
//! `pipeline_jobs.image` was sea-query's `string()`: `VARCHAR(255)` on MySQL,
//! unbounded on PostgreSQL and SQLite. The image reference grammar the engine
//! validates allows 255 bytes for the name alone, then a tag of up to 128 and
//! a digest of up to `sha512:` plus 128 hex digits, so a valid, digest-pinned
//! reference could be refused by the insert. Widened in place, so live rows
//! survive.

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
                .execute_unprepared("ALTER TABLE `pipeline_jobs` MODIFY COLUMN `image` TEXT NULL")
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing after accepting longer values would truncate live rows.
        Ok(())
    }
}
