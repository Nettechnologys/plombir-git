use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260508_000002_create_releases"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // --- releases ---
        manager
            .create_table(
                Table::create()
                    .table(Releases::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Releases::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Releases::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(Releases::TagName).string().not_null())
                    .col(
                        ColumnDef::new(Releases::TargetCommitish)
                            .string()
                            .not_null()
                            .default("main".to_owned()),
                    )
                    .col(ColumnDef::new(Releases::Title).string().not_null())
                    .col(ColumnDef::new(Releases::Body).text().null())
                    .col(
                        ColumnDef::new(Releases::IsDraft)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Releases::IsPrerelease)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Releases::AuthorId).big_integer().not_null())
                    .col(
                        ColumnDef::new(Releases::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Releases::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // Unique constraint: keep inline (SeaORM generates valid UNIQUE syntax for SQLite)
                    .index(
                        Index::create()
                            .unique()
                            .col(Releases::RepoId)
                            .col(Releases::TagName)
                            .name("idx_releases_repo_tag_unique"),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Releases::Table, Releases::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Releases::Table, Releases::AuthorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Non-unique index must be created separately in SQLite
        manager
            .create_index(
                Index::create()
                    .table(Releases::Table)
                    .col(Releases::RepoId)
                    .name("idx_releases_repo_id")
                    .to_owned(),
            )
            .await?;

        // --- release_assets ---
        manager
            .create_table(
                Table::create()
                    .table(ReleaseAssets::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ReleaseAssets::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ReleaseAssets::ReleaseId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ReleaseAssets::Filename).string().not_null())
                    .col(
                        ColumnDef::new(ReleaseAssets::Size)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(ReleaseAssets::ContentType)
                            .string()
                            .not_null()
                            .default("application/octet-stream".to_owned()),
                    )
                    .col(
                        ColumnDef::new(ReleaseAssets::DownloadCount)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(ReleaseAssets::UploaderId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ReleaseAssets::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ReleaseAssets::Table, ReleaseAssets::ReleaseId)
                            .to(Releases::Table, Releases::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ReleaseAssets::Table, ReleaseAssets::UploaderId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Non-unique index for release_assets
        manager
            .create_index(
                Index::create()
                    .table(ReleaseAssets::Table)
                    .col(ReleaseAssets::ReleaseId)
                    .name("idx_release_assets_release_id")
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ReleaseAssets::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Releases::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum Releases {
    Table,
    Id,
    RepoId,
    TagName,
    TargetCommitish,
    Title,
    Body,
    IsDraft,
    IsPrerelease,
    AuthorId,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum ReleaseAssets {
    Table,
    Id,
    ReleaseId,
    Filename,
    Size,
    ContentType,
    DownloadCount,
    UploaderId,
    CreatedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
