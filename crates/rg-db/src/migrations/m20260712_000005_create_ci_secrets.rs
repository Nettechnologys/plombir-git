use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000005_create_ci_secrets"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("ci_secrets").await? {
            return Ok(());
        }
        manager
            .create_table(
                Table::create()
                    .table(CiSecrets::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CiSecrets::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(CiSecrets::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(CiSecrets::Name).string().not_null())
                    .col(ColumnDef::new(CiSecrets::EncryptedValue).text().not_null())
                    .col(
                        ColumnDef::new(CiSecrets::CreatedById)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiSecrets::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiSecrets::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(CiSecrets::Table, CiSecrets::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(CiSecrets::Table, CiSecrets::CreatedById)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_ci_secrets_repo_name")
                            .col(CiSecrets::RepoId)
                            .col(CiSecrets::Name)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CiSecrets::Table).if_exists().to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CiSecrets {
    Table,
    Id,
    RepoId,
    Name,
    EncryptedValue,
    CreatedById,
    CreatedAt,
    UpdatedAt,
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
