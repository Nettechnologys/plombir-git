//! Create repository-scoped SSH deploy keys.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000002_create_deploy_keys"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("deploy_keys").await? {
            return Ok(());
        }
        manager
            .create_table(
                Table::create()
                    .table(DeployKeys::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DeployKeys::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DeployKeys::RepoId).big_integer().not_null())
                    .col(
                        ColumnDef::new(DeployKeys::CreatedById)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DeployKeys::Title).string().not_null())
                    .col(ColumnDef::new(DeployKeys::PublicKey).text().not_null())
                    .col(
                        ColumnDef::new(DeployKeys::Fingerprint)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(DeployKeys::ReadOnly)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(DeployKeys::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeployKeys::LastUsedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(DeployKeys::Table, DeployKeys::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(DeployKeys::Table, DeployKeys::CreatedById)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_deploy_keys_repo_created")
                    .table(DeployKeys::Table)
                    .col(DeployKeys::RepoId)
                    .col(DeployKeys::CreatedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(DeployKeys::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum DeployKeys {
    Table,
    Id,
    RepoId,
    CreatedById,
    Title,
    PublicKey,
    Fingerprint,
    ReadOnly,
    CreatedAt,
    LastUsedAt,
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
