use sea_orm_migration::prelude::*;
pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000013_add_pipeline_job_condition"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipeline_jobs", "if_condition").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(ColumnDef::new(PipelineJobs::IfCondition).text().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("pipeline_jobs", "if_condition").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::IfCondition)
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
    IfCondition,
}
