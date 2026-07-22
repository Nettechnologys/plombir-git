use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000008_add_pipeline_job_cache"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipeline_jobs", "cache_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(ColumnDef::new(PipelineJobs::CacheKey).string().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("pipeline_jobs", "cache_paths").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(ColumnDef::new(PipelineJobs::CachePaths).text().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("pipeline_jobs", "cache_paths").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::CachePaths)
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_column("pipeline_jobs", "cache_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::CacheKey)
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
    CacheKey,
    CachePaths,
}
