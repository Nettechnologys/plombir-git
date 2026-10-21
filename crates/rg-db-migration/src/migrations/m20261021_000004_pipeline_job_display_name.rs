//! `display_name` on a job: an Actions job's `jobs.<id>.name`, rendered for
//! its matrix variant (`Build on linux`), what the job page shows beside the
//! job's own name (card_1535b3af81ac).
//!
//! The row's `name` stays the job's id in the pipeline — `<file>/<job>`, with
//! the variant's coordinates — because `needs:`, matrix membership and
//! artifacts address the job by it. `NULL` is "the workflow gave the job no
//! name": every native job, and every row that exists when this migration
//! runs, is shown by its `name` as before.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261021_000004_pipeline_job_display_name"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("pipeline_jobs", "display_name").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(PipelineJobs::Table)
                    .add_column(ColumnDef::new(PipelineJobs::DisplayName).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipeline_jobs", "display_name").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(PipelineJobs::Table)
                    .drop_column(PipelineJobs::DisplayName)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PipelineJobs {
    Table,
    DisplayName,
}
