//! A sealed SSO secret is AES-GCM ciphertext in base64, a third larger than
//! the plaintext plus nonce and tag. `VARCHAR(1024)` caps it on MySQL and
//! PostgreSQL alike, so a client secret or bind password the admin API accepts
//! could fail at the SQL write. Widen both columns in place; SQLite enforces
//! no declared length and needs nothing.

use sea_orm::{ConnectionTrait, DatabaseBackend};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = match manager.get_database_backend() {
            DatabaseBackend::MySql => {
                "ALTER TABLE `sso_providers` \
                 MODIFY COLUMN `client_secret_enc` TEXT NULL, \
                 MODIFY COLUMN `ldap_bind_password_enc` TEXT NULL"
            }
            DatabaseBackend::Postgres => {
                "ALTER TABLE sso_providers \
                 ALTER COLUMN client_secret_enc TYPE TEXT, \
                 ALTER COLUMN ldap_bind_password_enc TYPE TEXT"
            }
            DatabaseBackend::Sqlite => return Ok(()),
        };
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing could truncate ciphertext written after this migration.
        Ok(())
    }
}
