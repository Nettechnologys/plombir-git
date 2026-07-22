//! Migration: create pipelines, pipeline_stages, pipeline_jobs tables.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── pipelines ──────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Pipelines::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Pipelines::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Pipelines::RepoId).integer().not_null())
                    .col(ColumnDef::new(Pipelines::CommitSha).string().not_null())
                    .col(ColumnDef::new(Pipelines::RefName).string().not_null())
                    .col(ColumnDef::new(Pipelines::Status).string().not_null())
                    .col(ColumnDef::new(Pipelines::TriggerType).string().not_null())
                    .col(ColumnDef::new(Pipelines::TriggeredBy).integer())
                    .col(ColumnDef::new(Pipelines::StartedAt).date_time())
                    .col(ColumnDef::new(Pipelines::FinishedAt).date_time())
                    .col(
                        ColumnDef::new(Pipelines::CreatedAt)
                            .date_time()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pipelines_repo_id")
                    .table(Pipelines::Table)
                    .col(Pipelines::RepoId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pipelines_status")
                    .table(Pipelines::Table)
                    .col(Pipelines::Status)
                    .to_owned(),
            )
            .await?;

        // ── pipeline_stages ────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(PipelineStages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PipelineStages::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PipelineStages::PipelineId)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PipelineStages::Name).string().not_null())
                    .col(
                        ColumnDef::new(PipelineStages::StageOrder)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PipelineStages::Status).string().not_null())
                    .col(ColumnDef::new(PipelineStages::StartedAt).date_time())
                    .col(ColumnDef::new(PipelineStages::FinishedAt).date_time())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pipeline_stages_pipeline_id")
                    .table(PipelineStages::Table)
                    .col(PipelineStages::PipelineId)
                    .to_owned(),
            )
            .await?;

        // ── pipeline_jobs ─────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(PipelineJobs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PipelineJobs::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PipelineJobs::StageId).integer().not_null())
                    .col(ColumnDef::new(PipelineJobs::Name).string().not_null())
                    .col(ColumnDef::new(PipelineJobs::Image).string())
                    .col(ColumnDef::new(PipelineJobs::Script).string().not_null())
                    .col(ColumnDef::new(PipelineJobs::Status).string().not_null())
                    .col(ColumnDef::new(PipelineJobs::ExitCode).integer())
                    .col(ColumnDef::new(PipelineJobs::Log).string())
                    .col(ColumnDef::new(PipelineJobs::StartedAt).date_time())
                    .col(ColumnDef::new(PipelineJobs::FinishedAt).date_time())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pipeline_jobs_stage_id")
                    .table(PipelineJobs::Table)
                    .col(PipelineJobs::StageId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PipelineJobs::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(PipelineStages::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Pipelines::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Pipelines {
    Table,
    Id,
    RepoId,
    CommitSha,
    RefName,
    Status,
    TriggerType,
    TriggeredBy,
    StartedAt,
    FinishedAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum PipelineStages {
    Table,
    Id,
    PipelineId,
    Name,
    StageOrder,
    Status,
    StartedAt,
    FinishedAt,
}

#[derive(DeriveIden)]
enum PipelineJobs {
    Table,
    Id,
    StageId,
    Name,
    Image,
    Script,
    Status,
    ExitCode,
    Log,
    StartedAt,
    FinishedAt,
}
