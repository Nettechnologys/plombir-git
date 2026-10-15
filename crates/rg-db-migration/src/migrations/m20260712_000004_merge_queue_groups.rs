//! Track speculative merge-group commits and their CI pipelines.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000004_merge_queue_groups"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, definition) in [
            (
                "merge_group_sha",
                ColumnDef::new(MergeQueueEntries::MergeGroupSha)
                    .string()
                    .null()
                    .to_owned(),
            ),
            (
                "merge_group_base_sha",
                ColumnDef::new(MergeQueueEntries::MergeGroupBaseSha)
                    .string()
                    .null()
                    .to_owned(),
            ),
            (
                "merge_group_head_sha",
                ColumnDef::new(MergeQueueEntries::MergeGroupHeadSha)
                    .string()
                    .null()
                    .to_owned(),
            ),
            (
                "merge_group_pipeline_id",
                ColumnDef::new(MergeQueueEntries::MergeGroupPipelineId)
                    .big_integer()
                    .null()
                    .to_owned(),
            ),
        ] {
            if !manager.has_column("merge_queue_entries", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(MergeQueueEntries::Table)
                            .add_column(definition)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        manager
            .create_index(
                Index::create()
                    .name("idx_merge_queue_group_sha")
                    .table(MergeQueueEntries::Table)
                    .col(MergeQueueEntries::MergeGroupSha)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_merge_queue_group_sha")
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        for (name, column) in [
            (
                "merge_group_pipeline_id",
                MergeQueueEntries::MergeGroupPipelineId,
            ),
            ("merge_group_head_sha", MergeQueueEntries::MergeGroupHeadSha),
            ("merge_group_base_sha", MergeQueueEntries::MergeGroupBaseSha),
            ("merge_group_sha", MergeQueueEntries::MergeGroupSha),
        ] {
            if manager.has_column("merge_queue_entries", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(MergeQueueEntries::Table)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

#[derive(DeriveIden, Clone, Copy)]
enum MergeQueueEntries {
    Table,
    MergeGroupSha,
    MergeGroupBaseSha,
    MergeGroupHeadSha,
    MergeGroupPipelineId,
}
