//! `strategy` of an Actions matrix job: the variants that belong together, how
//! they run beside each other, and a matrix decided only at run time.
//!
//! Every variant of one matrix job carries the same `matrix_group` — the job's
//! name in the pipeline — and its `strategy`: `matrix_fail_fast` (a failed
//! variant cancels the ones that have not finished) and `matrix_max_parallel`
//! (at most that many of them run at once). All three `NULL` for a job that
//! is not a matrix job.
//!
//! `deferred_matrix` is set on one placeholder row for a job whose matrix reads
//! a needed job's outputs: what the job's variants will be built from, as
//! JSON. The placeholder is never handed to a runner; once the stage it is in
//! is reached it is replaced by its variants. `MEDIUMTEXT` on MySQL: the job's
//! steps travel in it, and `TEXT` stops at 64 KiB.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

pub const NAME: &str = "m20261020_000004_pipeline_job_matrix";

impl MigrationName for Migration {
    fn name(&self) -> &str {
        NAME
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mysql = manager.get_database_backend() == DatabaseBackend::MySql;
        let deferred = if mysql {
            ColumnDef::new(PipelineJobs::DeferredMatrix)
                .custom(Alias::new("MEDIUMTEXT"))
                .null()
                .to_owned()
        } else {
            ColumnDef::new(PipelineJobs::DeferredMatrix)
                .text()
                .null()
                .to_owned()
        };
        let columns = [
            ("deferred_matrix", deferred),
            (
                "matrix_group",
                ColumnDef::new(PipelineJobs::MatrixGroup)
                    .text()
                    .null()
                    .to_owned(),
            ),
            (
                "matrix_fail_fast",
                ColumnDef::new(PipelineJobs::MatrixFailFast)
                    .boolean()
                    .null()
                    .to_owned(),
            ),
            (
                "matrix_max_parallel",
                ColumnDef::new(PipelineJobs::MatrixMaxParallel)
                    .integer()
                    .null()
                    .to_owned(),
            ),
        ];
        for (name, definition) in columns {
            if manager.has_column("pipeline_jobs", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(definition)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (column, name) in [
            (PipelineJobs::DeferredMatrix, "deferred_matrix"),
            (PipelineJobs::MatrixGroup, "matrix_group"),
            (PipelineJobs::MatrixFailFast, "matrix_fail_fast"),
            (PipelineJobs::MatrixMaxParallel, "matrix_max_parallel"),
        ] {
            if manager.has_column("pipeline_jobs", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(PipelineJobs::Table)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

#[derive(Iden, Clone, Copy)]
enum PipelineJobs {
    Table,
    DeferredMatrix,
    MatrixGroup,
    MatrixFailFast,
    MatrixMaxParallel,
}
