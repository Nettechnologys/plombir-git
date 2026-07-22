use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000010_add_pipeline_job_when"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("pipeline_jobs", "when_condition")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(
                            ColumnDef::new(PipelineJobs::WhenCondition)
                                .string()
                                .not_null()
                                .default("on_success"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("pipeline_jobs", "when_condition")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::WhenCondition)
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
    WhenCondition,
}
