//! Keep the encrypted secret mask alongside the external runner assignment.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261011_000001_runner_job_mask_snapshot"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("pipeline_jobs", "mask_secrets_encrypted")
            .await?
        {
            // A single accepted CI secret may be 65,536 bytes before JSON and
            // authenticated encryption. MySQL TEXT stops at 65,535 bytes.
            let snapshot = if manager.get_database_backend() == sea_orm::DatabaseBackend::MySql {
                ColumnDef::new(PipelineJobs::MaskSecretsEncrypted)
                    .custom(Alias::new("LONGTEXT"))
                    .null()
                    .to_owned()
            } else {
                ColumnDef::new(PipelineJobs::MaskSecretsEncrypted)
                    .text()
                    .null()
                    .to_owned()
            };
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(snapshot)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("pipeline_jobs", "mask_secrets_encrypted")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::MaskSecretsEncrypted)
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
    MaskSecretsEncrypted,
}
