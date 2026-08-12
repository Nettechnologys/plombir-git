use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260812_000001_add_merge_queue_attempt_number"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("merge_queue_entries", "attempt_number")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(MergeQueueEntries::Table)
                        .add_column(
                            ColumnDef::new(MergeQueueEntries::AttemptNumber)
                                .big_integer()
                                .not_null()
                                .default(1),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("merge_queue_entries", "attempt_number")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(MergeQueueEntries::Table)
                        .drop_column(MergeQueueEntries::AttemptNumber)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum MergeQueueEntries {
    Table,
    AttemptNumber,
}
