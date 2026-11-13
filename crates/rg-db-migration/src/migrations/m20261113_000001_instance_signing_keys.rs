//! Migration: replace the singleton `instance_signing_key` with
//! `instance_signing_keys`, one row per Ed25519 identity the instance has held.
//!
//! The singleton could only be overwritten, so a rotation erased the public half
//! every stored attestation was signed with, and a working server kept signing
//! with the key it had loaded at start. The new table keeps retired and revoked
//! keys as public halves only (their sealed seed is dropped on retirement) and
//! enforces "at most one active key" through a nullable `UNIQUE` slot.
//!
//! The carried-over row keeps its ciphertext untouched; its `kid` and public key
//! need the encryption key and are filled in by the first process that holds it.
//! Its origin is exact: the old table had two writers only — adoption from
//! `jwt_secret` (`rotated_at` NULL) and `rotate-instance-key` (random seed,
//! `rotated_at` set).
//!
//! Every step is safe to repeat. MySQL commits DDL implicitly, so a crash between
//! the CREATE, the copy, the DROP and the migration record must leave a state the
//! next `up` completes rather than trips over.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

const OLD: &str = "instance_signing_key";
const NEW: &str = "instance_signing_keys";

const CARRY_OVER: &str = "\
    INSERT INTO instance_signing_keys \
        (seed_encrypted, origin, status, active_slot, created_at) \
    SELECT seed_encrypted, \
           CASE WHEN rotated_at IS NULL THEN 'legacy_derived' ELSE 'generated' END, \
           'active', 1, COALESCE(rotated_at, created_at) \
    FROM instance_signing_key";

const CARRY_BACK: &str = "\
    INSERT INTO instance_signing_key (id, seed_encrypted, created_at, rotated_at) \
    SELECT 1, seed_encrypted, created_at, \
           CASE WHEN origin = 'generated' THEN created_at ELSE NULL END \
    FROM instance_signing_keys \
    WHERE status = 'active' AND seed_encrypted IS NOT NULL";

fn alias(value: &str) -> Alias {
    Alias::new(value)
}

async fn is_empty(manager: &SchemaManager<'_>, table: &str) -> Result<bool, DbErr> {
    let db = manager.get_connection();
    let row = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            format!("SELECT COUNT(*) AS n FROM {table}"),
        ))
        .await?
        .ok_or_else(|| DbErr::Custom(format!("COUNT(*) on {table} returned no row")))?;
    Ok(row.try_get::<i64>("", "n")? == 0)
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let timestamp = |name: &str| {
            ColumnDef::new(alias(name))
                .timestamp_with_time_zone()
                .to_owned()
        };
        manager
            .create_table(
                Table::create()
                    .table(alias(NEW))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(alias("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(alias("kid"))
                            .string_len(16)
                            .null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(alias("public_key")).string_len(64).null())
                    .col(ColumnDef::new(alias("seed_encrypted")).text().null())
                    .col(ColumnDef::new(alias("origin")).string_len(16).not_null())
                    .col(ColumnDef::new(alias("status")).string_len(16).not_null())
                    // NULL on every row but the active one; several NULLs are
                    // allowed by a UNIQUE index on SQLite, PostgreSQL and InnoDB.
                    .col(
                        ColumnDef::new(alias("active_slot"))
                            .small_integer()
                            .null()
                            .unique_key(),
                    )
                    .col(timestamp("created_at").not_null())
                    .col(timestamp("retired_at").null())
                    .col(timestamp("revoked_at").null())
                    .to_owned(),
            )
            .await?;

        if manager.has_table(OLD).await? {
            if is_empty(manager, NEW).await? {
                manager
                    .get_connection()
                    .execute_unprepared(CARRY_OVER)
                    .await?;
            }
            manager
                .drop_table(Table::drop().table(alias(OLD)).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(alias(OLD))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(alias("id"))
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(alias("seed_encrypted")).text().not_null())
                    .col(
                        ColumnDef::new(alias("created_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(alias("rotated_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        if manager.has_table(NEW).await? {
            if is_empty(manager, OLD).await? {
                manager
                    .get_connection()
                    .execute_unprepared(CARRY_BACK)
                    .await?;
            }
            manager
                .drop_table(Table::drop().table(alias(NEW)).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20261113_000001_instance_signing_keys_tests.rs"]
mod tests;
