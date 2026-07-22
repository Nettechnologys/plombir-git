//! Migration: create import_tasks table for GitHub/GitLab data migration.
//!
//! An import task tracks the progress of migrating a repository and its
//! metadata (issues, PRs, labels, milestones, releases, wiki) from
//! external platforms (GitHub, GitLab) into IronForge.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260607_000004_create_import_tasks"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ImportTask::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ImportTask::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ImportTask::UserId).big_integer().not_null())
                    .col(ColumnDef::new(ImportTask::RepoId).big_integer().null())
                    .col(ColumnDef::new(ImportTask::Platform).string().not_null())
                    .col(ColumnDef::new(ImportTask::SourceUrl).string().not_null())
                    .col(ColumnDef::new(ImportTask::TargetOwner).string().not_null())
                    .col(ColumnDef::new(ImportTask::TargetName).string().not_null())
                    .col(ColumnDef::new(ImportTask::AuthTokenEncrypted).text().null())
                    .col(
                        ColumnDef::new(ImportTask::Status)
                            .string()
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(ImportTask::Progress)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(ImportTask::Stage).string().null())
                    .col(ColumnDef::new(ImportTask::Error).text().null())
                    .col(ColumnDef::new(ImportTask::UserMapping).text().null())
                    .col(
                        ColumnDef::new(ImportTask::ImportRepo)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ImportTask::ImportIssues)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ImportTask::ImportPullRequests)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ImportTask::ImportWiki)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(ImportTask::ImportReleases)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ImportTask::ImportLabels)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ImportTask::ImportMilestones)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(ColumnDef::new(ImportTask::Stats).text().null())
                    .col(
                        ColumnDef::new(ImportTask::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ImportTask::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ImportTask::Table, ImportTask::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ImportTask::Table, ImportTask::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        // Index: lookup by user
        manager
            .create_index(
                Index::create()
                    .name("idx_import_tasks_user")
                    .table(ImportTask::Table)
                    .col(ImportTask::UserId)
                    .to_owned(),
            )
            .await?;

        // Index: lookup by repo
        manager
            .create_index(
                Index::create()
                    .name("idx_import_tasks_repo")
                    .table(ImportTask::Table)
                    .col(ImportTask::RepoId)
                    .to_owned(),
            )
            .await?;

        // Index: active imports for polling
        manager
            .create_index(
                Index::create()
                    .name("idx_import_tasks_status")
                    .table(ImportTask::Table)
                    .col(ImportTask::Status)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ImportTask::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum ImportTask {
    #[iden = "import_tasks"]
    Table,
    Id,
    UserId,
    RepoId,
    Platform,
    SourceUrl,
    TargetOwner,
    TargetName,
    AuthTokenEncrypted,
    Status,
    Progress,
    Stage,
    Error,
    UserMapping,
    ImportRepo,
    ImportIssues,
    ImportPullRequests,
    ImportWiki,
    ImportReleases,
    ImportLabels,
    ImportMilestones,
    Stats,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
