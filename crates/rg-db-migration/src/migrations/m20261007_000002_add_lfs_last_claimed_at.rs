//! Migration: record when a push last relied on an LFS object being stored.
//!
//! The batch API answers "already have it" for an uploaded object, and the
//! client then sends no bytes and moves the ref. Removing unused objects
//! decides by the refs it reads, so between that answer and the ref update it
//! sees an old object nothing points at — and used to remove it. The claim
//! time lets that removal see the answer and keep the object for as long as it
//! keeps a freshly uploaded one.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261007_000002_add_lfs_last_claimed_at"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("lfs_objects", "last_claimed_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .add_column(
                            ColumnDef::new(LfsObjects::LastClaimedAt)
                                .timestamp_with_time_zone()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("lfs_objects", "last_claimed_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .drop_column(LfsObjects::LastClaimedAt)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum LfsObjects {
    Table,
    LastClaimedAt,
}
