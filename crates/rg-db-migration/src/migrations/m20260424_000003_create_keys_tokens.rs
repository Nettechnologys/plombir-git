use sea_orm_migration::prelude::*;

/// Create `ssh_keys` and `access_tokens` tables.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260424_000003_create_keys_tokens"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // --- ssh_keys ---
        manager
            .create_table(
                Table::create()
                    .table(SshKeys::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SshKeys::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SshKeys::UserId).big_integer().not_null())
                    .col(ColumnDef::new(SshKeys::Title).string().not_null())
                    .col(ColumnDef::new(SshKeys::PublicKey).text().not_null())
                    .col(
                        ColumnDef::new(SshKeys::Fingerprint)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(SshKeys::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SshKeys::LastUsedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(SshKeys::Table, SshKeys::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // --- access_tokens ---
        manager
            .create_table(
                Table::create()
                    .table(AccessTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AccessTokens::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AccessTokens::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AccessTokens::Name).string().not_null())
                    .col(
                        ColumnDef::new(AccessTokens::TokenHash)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(AccessTokens::Scopes)
                            .string()
                            .not_null()
                            .default("repo"),
                    )
                    .col(
                        ColumnDef::new(AccessTokens::ExpiresAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AccessTokens::LastUsedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AccessTokens::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(AccessTokens::Table, AccessTokens::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AccessTokens::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SshKeys::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum SshKeys {
    Table,
    Id,
    UserId,
    Title,
    PublicKey,
    Fingerprint,
    CreatedAt,
    LastUsedAt,
}

#[derive(Iden)]
enum AccessTokens {
    Table,
    Id,
    UserId,
    Name,
    TokenHash,
    Scopes,
    ExpiresAt,
    LastUsedAt,
    CreatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
