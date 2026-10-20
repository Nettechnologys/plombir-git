//! `jobs.<id>.outputs`: what a job of steps reads of the jobs it needs, and
//! what it leaves for the jobs that need it.
//!
//! `pipeline_jobs.job_context` is written with the graph: the job's `needs:`
//! (the id its expressions use → the job's name in the pipeline) and its
//! `outputs:` templates, as JSON. `pipeline_jobs.outputs` is written by the
//! runner once the job's last step ran: the resolved outputs, masked, as a
//! JSON object. Both `NULL` for a native job. `MEDIUMTEXT` on MySQL: a job may
//! leave 1 MiB of outputs, and `TEXT` stops at 64 KiB.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

pub const NAME: &str = "m20261020_000003_pipeline_job_outputs";

impl MigrationName for Migration {
    fn name(&self) -> &str {
        NAME
    }
}

const COLUMNS: [(PipelineJobs, &str); 2] = [
    (PipelineJobs::JobContext, "job_context"),
    (PipelineJobs::Outputs, "outputs"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mysql = manager.get_database_backend() == DatabaseBackend::MySql;
        for (column, name) in COLUMNS {
            if manager.has_column("pipeline_jobs", name).await? {
                continue;
            }
            let definition = if mysql {
                ColumnDef::new(column)
                    .custom(Alias::new("MEDIUMTEXT"))
                    .null()
                    .to_owned()
            } else {
                ColumnDef::new(column).text().null().to_owned()
            };
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
        for (column, name) in COLUMNS {
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
    JobContext,
    Outputs,
}
