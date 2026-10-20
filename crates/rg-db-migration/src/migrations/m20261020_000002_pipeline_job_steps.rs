//! The step model: what a job runs one by one, and how each step went.
//!
//! `pipeline_jobs.steps` is the job's list of steps as JSON — written when the
//! pipeline graph is, for a job translated from an Actions workflow, and
//! `NULL` for a native `.plombir-git-ci.yml` job, whose `script` stays the one
//! thing it runs. `MEDIUMTEXT` on MySQL, like `script`: a workflow of one MiB
//! carries its scripts in here.
//!
//! `pipeline_job_steps` holds one row per step of such a job: written
//! `pending` with the graph so the job page lists its steps before a runner
//! takes it, then moved by the runner as each step starts and ends. The step's
//! own log is kept on its row (`MEDIUMTEXT` on MySQL); the job's log still
//! carries every step under a header, as it always carried the whole script.
//!
//! The two times are `DATETIME`, like every other CI time column since
//! `m20260830_000001_align_ci_schema_types`: MySQL hands a `TIMESTAMP` back as
//! a type `NaiveDateTime` does not decode.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

pub const NAME: &str = "m20261020_000002_pipeline_job_steps";
pub const KEY_INDEX: &str = "idx_pipeline_job_steps_job_number";

impl MigrationName for Migration {
    fn name(&self) -> &str {
        NAME
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mysql = manager.get_database_backend() == DatabaseBackend::MySql;
        if !manager.has_column("pipeline_jobs", "steps").await? {
            let column = if mysql {
                ColumnDef::new(PipelineJobs::Steps)
                    .custom(Alias::new("MEDIUMTEXT"))
                    .null()
                    .to_owned()
            } else {
                ColumnDef::new(PipelineJobs::Steps).text().null().to_owned()
            };
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(column)
                        .to_owned(),
                )
                .await?;
        }

        let log = if mysql {
            ColumnDef::new(PipelineJobSteps::Log)
                .custom(Alias::new("MEDIUMTEXT"))
                .null()
                .to_owned()
        } else {
            ColumnDef::new(PipelineJobSteps::Log)
                .text()
                .null()
                .to_owned()
        };
        manager
            .create_table(
                Table::create()
                    .table(PipelineJobSteps::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PipelineJobSteps::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PipelineJobSteps::JobId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PipelineJobSteps::Number)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PipelineJobSteps::Name).text().not_null())
                    .col(
                        ColumnDef::new(PipelineJobSteps::Status)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PipelineJobSteps::Conclusion)
                            .string_len(32)
                            .null(),
                    )
                    .col(ColumnDef::new(PipelineJobSteps::ExitCode).integer().null())
                    .col(log)
                    .col(
                        ColumnDef::new(PipelineJobSteps::StartedAt)
                            .date_time()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(PipelineJobSteps::FinishedAt)
                            .date_time()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_pipeline_job_steps_job")
                            .from(PipelineJobSteps::Table, PipelineJobSteps::JobId)
                            .to(PipelineJobs::Table, PipelineJobs::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(KEY_INDEX)
                    .table(PipelineJobSteps::Table)
                    .col(PipelineJobSteps::JobId)
                    .col(PipelineJobSteps::Number)
                    .unique()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(PipelineJobSteps::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        if manager.has_column("pipeline_jobs", "steps").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::Steps)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Iden)]
enum PipelineJobs {
    Table,
    Id,
    Steps,
}

#[derive(Iden)]
enum PipelineJobSteps {
    Table,
    Id,
    JobId,
    Number,
    Name,
    Status,
    Conclusion,
    ExitCode,
    Log,
    StartedAt,
    FinishedAt,
}
