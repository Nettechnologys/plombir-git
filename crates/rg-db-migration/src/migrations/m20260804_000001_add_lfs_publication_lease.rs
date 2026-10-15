//! Migration: give `lfs_objects` a cross-process publication lease.
//!
//! Two concurrent first publications of the same LFS `oid` write the same
//! stable content-addressed key. Without a serialisation point, the request
//! whose metadata commit fails cannot tell its own bytes from bytes another
//! request has already handed to a live row — and its rollback deletes them.
//! The lease turns "did I publish these bytes" into a claim the database
//! arbitrates, so the answer survives across processes.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260804_000001_add_lfs_publication_lease"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("lfs_objects", "publisher_token").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .add_column(ColumnDef::new(LfsObjects::PublisherToken).string().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("lfs_objects", "publisher_since").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .add_column(
                            ColumnDef::new(LfsObjects::PublisherSince)
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
        if manager.has_column("lfs_objects", "publisher_since").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .drop_column(LfsObjects::PublisherSince)
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_column("lfs_objects", "publisher_token").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .drop_column(LfsObjects::PublisherToken)
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
    PublisherToken,
    PublisherSince,
}
