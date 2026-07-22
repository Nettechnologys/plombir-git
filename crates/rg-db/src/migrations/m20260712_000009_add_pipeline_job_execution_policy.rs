use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000009_add_pipeline_job_execution_policy"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipeline_jobs", "allow_failure").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(
                            ColumnDef::new(PipelineJobs::AllowFailure)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("pipeline_jobs", "timeout_seconds")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(
                            ColumnDef::new(PipelineJobs::TimeoutSeconds)
                                .big_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("pipeline_jobs", "timeout_seconds")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::TimeoutSeconds)
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_column("pipeline_jobs", "allow_failure").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::AllowFailure)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
#[derive(DeriveIden)]
enum PipelineJobs {
    Table,
    AllowFailure,
    TimeoutSeconds,
}
