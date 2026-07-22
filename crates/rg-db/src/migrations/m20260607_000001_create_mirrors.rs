//! Migration: create mirrors table for repository mirroring.
//!
//! A mirror syncs a remote repository into a local bare clone.
//! Supports authentication via username/password or token.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260607_000001_create_mirrors"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Mirror::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Mirror::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Mirror::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(Mirror::Url).string().not_null())
                    .col(ColumnDef::new(Mirror::Username).string().null())
                    .col(ColumnDef::new(Mirror::PasswordEncrypted).text().null())
                    .col(
                        ColumnDef::new(Mirror::SyncIntervalSeconds)
                            .big_integer()
                            .not_null()
                            .default(86400),
                    )
                    .col(
                        ColumnDef::new(Mirror::NextSyncAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Mirror::LastSyncAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(Mirror::LastSyncError).text().null())
                    .col(
                        ColumnDef::new(Mirror::Status)
                            .string()
                            .not_null()
                            .default("active"),
                    )
                    .col(
                        ColumnDef::new(Mirror::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Mirror::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Mirror::Table, Mirror::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_mirrors_repo_unique")
                    .table(Mirror::Table)
                    .col(Mirror::RepoId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_mirrors_next_sync")
                    .table(Mirror::Table)
                    .col(Mirror::NextSyncAt)
                    .col(Mirror::Status)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Mirror::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum Mirror {
    Table,
    Id,
    RepoId,
    Url,
    Username,
    PasswordEncrypted,
    SyncIntervalSeconds,
    NextSyncAt,
    LastSyncAt,
    LastSyncError,
    Status,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
