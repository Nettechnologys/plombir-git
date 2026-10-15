//! card_61e4278d15e3: create the database arbitration point for one
//! `(repository, concurrency.group)`.
//!
//! `trigger_pipeline` used to read active pipelines before it began the graph
//! transaction. Two producers could therefore both observe an empty group and
//! commit complete graphs. A pipeline row cannot lock the absence of a row, and
//! locking the repository would serialize unrelated groups. This table gives
//! every group a stable row that an UPSERT can lock inside the same transaction
//! as active-state inspection, cancellation, and graph publication.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UNIQUE_GROUP: &str = "uq_pipeline_concurrency_locks_repo_group";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PipelineConcurrencyLocks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PipelineConcurrencyLocks::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PipelineConcurrencyLocks::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    // Matches the existing `pipelines.concurrency_group`
                    // capacity. 255 UTF-8 characters plus the integer repo key
                    // remain within modern InnoDB's composite-index limit.
                    .col(
                        ColumnDef::new(PipelineConcurrencyLocks::GroupName)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PipelineConcurrencyLocks::TouchedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                PipelineConcurrencyLocks::Table,
                                PipelineConcurrencyLocks::RepoId,
                            )
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .unique()
                    .name(UNIQUE_GROUP)
                    .table(PipelineConcurrencyLocks::Table)
                    .col(PipelineConcurrencyLocks::RepoId)
                    .col(PipelineConcurrencyLocks::GroupName)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(PipelineConcurrencyLocks::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum PipelineConcurrencyLocks {
    Table,
    Id,
    RepoId,
    GroupName,
    TouchedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
