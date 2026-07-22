//! Create the repository-scoped pull-request merge queue.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260711_000003_merge_queue"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("merge_queue_entries").await? {
            return Ok(());
        }
        manager
            .create_table(
                Table::create()
                    .table(MergeQueueEntries::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MergeQueueEntries::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::PrId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::EnqueuedById)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::Strategy)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::Status)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::FailureReason)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::StartedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(MergeQueueEntries::FinishedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MergeQueueEntries::Table, MergeQueueEntries::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MergeQueueEntries::Table, MergeQueueEntries::PrId)
                            .to(PullRequests::Table, PullRequests::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_merge_queue_pr_unique")
                    .table(MergeQueueEntries::Table)
                    .col(MergeQueueEntries::PrId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_merge_queue_repo_status_created")
                    .table(MergeQueueEntries::Table)
                    .col(MergeQueueEntries::RepoId)
                    .col(MergeQueueEntries::Status)
                    .col(MergeQueueEntries::CreatedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(MergeQueueEntries::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum MergeQueueEntries {
    Table,
    Id,
    RepoId,
    PrId,
    EnqueuedById,
    Strategy,
    Status,
    FailureReason,
    CreatedAt,
    UpdatedAt,
    StartedAt,
    FinishedAt,
}

#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum PullRequests {
    Table,
    Id,
}
