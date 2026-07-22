//! Align PostgreSQL timestamp columns with entities that use `DateTimeUtc`.
//!
//! Several historical migrations used `date_time()` (PostgreSQL `TIMESTAMP`)
//! while the corresponding SeaORM entities decode `DateTimeUtc` as
//! `TIMESTAMPTZ`. SQLite and MySQL accept the mapping, but PostgreSQL rejects
//! non-null values at decode time. Existing values were written as UTC, so the
//! conversion explicitly interprets the old wall-clock timestamps as UTC.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260713_000001_fix_postgres_utc_timestamps"
    }
}

const UTC_COLUMNS: &[(&str, &str)] = &[
    ("users", "last_login_at"),
    ("users", "locked_until"),
    ("package_registry", "created_at"),
    ("package_registry", "updated_at"),
    ("packages", "created_at"),
    ("packages", "updated_at"),
    ("package_versions", "created_at"),
    ("package_files", "created_at"),
    ("oauth_accounts", "token_expires_at"),
    ("oauth_accounts", "created_at"),
    ("oauth_accounts", "updated_at"),
    ("mfa_backup_codes", "used_at"),
    ("mfa_backup_codes", "created_at"),
    ("login_logs", "created_at"),
    ("sso_providers", "created_at"),
    ("sso_providers", "updated_at"),
    ("audit_log", "created_at"),
    ("oci_repository", "created_at"),
    ("oci_repository", "updated_at"),
    ("oci_manifest", "created_at"),
    ("oci_manifest", "updated_at"),
    ("oci_blob", "created_at"),
    ("oci_upload", "created_at"),
    ("oci_upload", "expires_at"),
    ("password_reset_tokens", "expires_at"),
    ("password_reset_tokens", "created_at"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }

        for (table, column) in UTC_COLUMNS {
            if manager.has_table(*table).await? && manager.has_column(*table, *column).await? {
                manager
                    .get_connection()
                    .execute_unprepared(&format!(
                        "DO $ironforge$ BEGIN \
                         IF EXISTS (SELECT 1 FROM information_schema.columns \
                           WHERE table_schema = current_schema() \
                             AND table_name = '{table}' AND column_name = '{column}' \
                             AND data_type = 'timestamp without time zone') THEN \
                           ALTER TABLE \"{table}\" ALTER COLUMN \"{column}\" TYPE TIMESTAMPTZ \
                             USING \"{column}\" AT TIME ZONE 'UTC'; \
                         END IF; END $ironforge$;"
                    ))
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::Postgres {
            return Ok(());
        }

        for (table, column) in UTC_COLUMNS.iter().rev() {
            if manager.has_table(*table).await? && manager.has_column(*table, *column).await? {
                manager
                    .get_connection()
                    .execute_unprepared(&format!(
                        "DO $ironforge$ BEGIN \
                         IF EXISTS (SELECT 1 FROM information_schema.columns \
                           WHERE table_schema = current_schema() \
                             AND table_name = '{table}' AND column_name = '{column}' \
                             AND data_type = 'timestamp with time zone') THEN \
                           ALTER TABLE \"{table}\" ALTER COLUMN \"{column}\" TYPE TIMESTAMP \
                             USING \"{column}\" AT TIME ZONE 'UTC'; \
                         END IF; END $ironforge$;"
                    ))
                    .await?;
            }
        }
        Ok(())
    }
}
