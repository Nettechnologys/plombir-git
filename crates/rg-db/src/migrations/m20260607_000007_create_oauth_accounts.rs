//! Migration: create `oauth_accounts` table for OAuth2/SSO bindings.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(OAuthAccounts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OAuthAccounts::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(OAuthAccounts::Provider).string().not_null())
                    .col(
                        ColumnDef::new(OAuthAccounts::ProviderUserId)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OAuthAccounts::ProviderUsername)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OAuthAccounts::Email).string().not_null())
                    .col(ColumnDef::new(OAuthAccounts::AccessToken).text().null())
                    .col(ColumnDef::new(OAuthAccounts::RefreshToken).text().null())
                    .col(
                        ColumnDef::new(OAuthAccounts::TokenExpiresAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(OAuthAccounts::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OAuthAccounts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OAuthAccounts::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(OAuthAccounts::Table, OAuthAccounts::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique constraint on (provider, provider_user_id)
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .table(OAuthAccounts::Table)
                    .name("idx_oauth_accounts_user_id")
                    .col(OAuthAccounts::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OAuthAccounts::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum OAuthAccounts {
    Table,
    Id,
    Provider,
    ProviderUserId,
    ProviderUsername,
    Email,
    AccessToken,
    RefreshToken,
    TokenExpiresAt,
    UserId,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
