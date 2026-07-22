use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260510_000001_create_runners"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create runners table
        manager
            .create_table(
                Table::create()
                    .table(Runners::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Runners::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Runners::Name).string().not_null())
                    .col(
                        ColumnDef::new(Runners::Token)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Runners::Status)
                            .string()
                            .not_null()
                            .default("offline"),
                    )
                    .col(
                        ColumnDef::new(Runners::Labels)
                            .string()
                            .not_null()
                            .default("[]"),
                    )
                    .col(
                        ColumnDef::new(Runners::LastSeenAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Runners::Version).string().default("unknown"))
                    .col(ColumnDef::new(Runners::Os).string().default("unknown"))
                    .col(ColumnDef::new(Runners::Arch).string().default("unknown"))
                    .col(
                        ColumnDef::new(Runners::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Runners::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index on status
        manager
            .create_index(
                Index::create()
                    .table(Runners::Table)
                    .name("idx_runners_status")
                    .col(Runners::Status)
                    .to_owned(),
            )
            .await?;

        // Create index on last_seen_at
        manager
            .create_index(
                Index::create()
                    .table(Runners::Table)
                    .name("idx_runners_last_seen")
                    .col(Runners::LastSeenAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Runners::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum Runners {
    Table,
    Id,
    Name,
    Token,
    Status,
    Labels,
    LastSeenAt,
    Version,
    Os,
    Arch,
    CreatedAt,
    UpdatedAt,
}
