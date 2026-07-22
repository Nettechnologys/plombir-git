use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260510_000003_add_pipeline_jobs_updated_at"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add updated_at column to pipeline_jobs
        manager
            .alter_table(
                Table::alter()
                    .table(PipelineJobs::Table)
                    .add_column(
                        ColumnDef::new(PipelineJobs::UpdatedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite does not support DROP COLUMN in ALTER TABLE
        // For production, use a proper migration strategy
        Ok(())
    }
}

#[derive(Iden)]
enum PipelineJobs {
    Table,
    UpdatedAt,
}
