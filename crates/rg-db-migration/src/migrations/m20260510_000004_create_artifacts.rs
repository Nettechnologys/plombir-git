use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260510_000004_create_artifacts"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Artifacts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Artifacts::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Artifacts::JobId).big_integer().not_null())
                    .col(ColumnDef::new(Artifacts::Name).string_len(255).not_null())
                    .col(
                        ColumnDef::new(Artifacts::FilePath)
                            .string_len(1024)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Artifacts::Size)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Artifacts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Artifacts::ExpiresAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_artifacts_job_id")
                    .table(Artifacts::Table)
                    .col(Artifacts::JobId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Artifacts::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum Artifacts {
    Table,
    Id,
    JobId,
    Name,
    FilePath,
    Size,
    CreatedAt,
    ExpiresAt,
}
