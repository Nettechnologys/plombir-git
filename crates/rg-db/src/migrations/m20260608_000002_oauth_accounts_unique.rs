//! Migration: Add unique index on `oauth_accounts` (provider, provider_user_id).
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .table(OAuthAccounts::Table)
                    .name("uq_oauth_accounts_provider_uid")
                    .col(OAuthAccounts::Provider)
                    .col(OAuthAccounts::ProviderUserId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("uq_oauth_accounts_provider_uid")
                    .table(OAuthAccounts::Table)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(Iden)]
enum OAuthAccounts {
    Table,
    Provider,
    ProviderUserId,
}
