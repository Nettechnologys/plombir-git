use sea_orm_migration::prelude::*;

/// Create `repositories` table.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260424_000002_create_repositories"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Repositories::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Repositories::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Repositories::OwnerId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Repositories::Name).string().not_null())
                    .col(ColumnDef::new(Repositories::Description).text().null())
                    .col(
                        ColumnDef::new(Repositories::IsPrivate)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Repositories::DefaultBranch)
                            .string()
                            .not_null()
                            .default("main"),
                    )
                    .col(ColumnDef::new(Repositories::ForkId).big_integer().null())
                    .col(
                        ColumnDef::new(Repositories::StarsCount)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Repositories::ForksCount)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Repositories::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Repositories::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Repositories::Table, Repositories::OwnerId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    // unique constraint: (owner_id, name)
                    .index(
                        Index::create()
                            .unique()
                            .col(Repositories::OwnerId)
                            .col(Repositories::Name),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Repositories::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
    OwnerId,
    Name,
    Description,
    IsPrivate,
    DefaultBranch,
    ForkId,
    StarsCount,
    ForksCount,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
