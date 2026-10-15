use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000011_create_ci_environments"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CiEnvironments::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CiEnvironments::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironments::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironments::Name)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironments::Protected)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(CiEnvironments::RequiredApprovals)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(CiEnvironments::AllowedApproverIds)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironments::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironments::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(CiEnvironments::Table, CiEnvironments::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_ci_environments_repo_name")
                            .col(CiEnvironments::RepoId)
                            .col(CiEnvironments::Name)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(CiEnvironmentApprovals::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CiEnvironmentApprovals::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironmentApprovals::JobId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironmentApprovals::EnvironmentId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironmentApprovals::ApprovedBy)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiEnvironmentApprovals::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(CiEnvironmentApprovals::Table, CiEnvironmentApprovals::JobId)
                            .to(PipelineJobs::Table, PipelineJobs::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                CiEnvironmentApprovals::Table,
                                CiEnvironmentApprovals::EnvironmentId,
                            )
                            .to(CiEnvironments::Table, CiEnvironments::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                CiEnvironmentApprovals::Table,
                                CiEnvironmentApprovals::ApprovedBy,
                            )
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_ci_environment_approval_job_user")
                            .col(CiEnvironmentApprovals::JobId)
                            .col(CiEnvironmentApprovals::ApprovedBy)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;
        if !manager
            .has_column("pipeline_jobs", "environment_id")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(
                            ColumnDef::new(PipelineJobs::EnvironmentId)
                                .big_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("pipeline_jobs", "environment_name")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(
                            ColumnDef::new(PipelineJobs::EnvironmentName)
                                .string_len(255)
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("pipeline_jobs", "environment_name")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::EnvironmentName)
                        .to_owned(),
                )
                .await?;
        }
        if manager
            .has_column("pipeline_jobs", "environment_id")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .drop_column(PipelineJobs::EnvironmentId)
                        .to_owned(),
                )
                .await?;
        }
        manager
            .drop_table(
                Table::drop()
                    .table(CiEnvironmentApprovals::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(CiEnvironments::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum CiEnvironments {
    Table,
    Id,
    RepoId,
    Name,
    Protected,
    RequiredApprovals,
    AllowedApproverIds,
    CreatedAt,
    UpdatedAt,
}
#[derive(DeriveIden)]
enum CiEnvironmentApprovals {
    Table,
    Id,
    JobId,
    EnvironmentId,
    ApprovedBy,
    CreatedAt,
}
#[derive(DeriveIden)]
enum PipelineJobs {
    Table,
    Id,
    EnvironmentId,
    EnvironmentName,
}
#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
