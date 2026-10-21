//! `deferred_condition` on a job of an Actions workflow that lists `needs:`:
//! what decides, once the jobs it needs have finished, whether it runs at all.
//!
//! Its `if:` may read `needs.<id>.outputs.*` and `needs.<id>.result`, which
//! have no value when the pipeline is written, and a job whose needed job was
//! skipped is skipped too. So such a job is held — never handed to a runner —
//! until its stage is reached; the column is cleared once it may run, and the
//! job is skipped instead when it may not. As JSON: the condition, the needs,
//! and the context the condition reads. `MEDIUMTEXT` on MySQL: that context
//! carries the job's variables, and `TEXT` stops at 64 KiB.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

pub const NAME: &str = "m20261021_000002_pipeline_job_deferred_condition";

impl MigrationName for Migration {
    fn name(&self) -> &str {
        NAME
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("pipeline_jobs", "deferred_condition")
            .await?
        {
            return Ok(());
        }
        let mut column = ColumnDef::new(PipelineJobs::DeferredCondition);
        if manager.get_database_backend() == DatabaseBackend::MySql {
            column.custom(Alias::new("MEDIUMTEXT"));
        } else {
            column.text();
        }
        manager
            .alter_table(
                Table::alter()
                    .table(PipelineJobs::Table)
                    .add_column(column.null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("pipeline_jobs", "deferred_condition")
            .await?
        {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(PipelineJobs::Table)
                    .drop_column(PipelineJobs::DeferredCondition)
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden, Clone, Copy)]
enum PipelineJobs {
    Table,
    DeferredCondition,
}
