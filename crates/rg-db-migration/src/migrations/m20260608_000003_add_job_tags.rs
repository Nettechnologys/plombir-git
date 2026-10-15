use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260608_000003_add_job_tags"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PipelineJobs::Table)
                    .add_column(ColumnDef::new(PipelineJobs::Tags).string().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PipelineJobs::Table)
                    .drop_column(PipelineJobs::Tags)
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum PipelineJobs {
    Table,
    Tags,
}
