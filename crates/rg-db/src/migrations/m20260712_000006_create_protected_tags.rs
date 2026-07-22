use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000006_create_protected_tags"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("protected_tags").await? {
            return Ok(());
        }
        manager
            .create_table(
                Table::create()
                    .table(ProtectedTags::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProtectedTags::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ProtectedTags::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ProtectedTags::Pattern).string().not_null())
                    .col(ColumnDef::new(ProtectedTags::AllowedUserIds).text().null())
                    .col(
                        ColumnDef::new(ProtectedTags::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProtectedTags::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ProtectedTags::Table, ProtectedTags::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_protected_tags_repo_pattern")
                            .col(ProtectedTags::RepoId)
                            .col(ProtectedTags::Pattern)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ProtectedTags::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
#[derive(DeriveIden)]
enum ProtectedTags {
    Table,
    Id,
    RepoId,
    Pattern,
    AllowedUserIds,
    CreatedAt,
    UpdatedAt,
}
#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}
