//! Migration: create `password_reset_tokens` table.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PasswordResetTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PasswordResetTokens::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PasswordResetTokens::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PasswordResetTokens::TokenHash)
                            .string_len(128)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PasswordResetTokens::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PasswordResetTokens::Used)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(PasswordResetTokens::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(PasswordResetTokens::Table, PasswordResetTokens::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .table(PasswordResetTokens::Table)
                    .name("idx_reset_tokens_hash")
                    .col(PasswordResetTokens::TokenHash)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .table(PasswordResetTokens::Table)
                    .name("idx_reset_tokens_user_id")
                    .col(PasswordResetTokens::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PasswordResetTokens::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
pub enum PasswordResetTokens {
    Table,
    Id,
    UserId,
    TokenHash,
    ExpiresAt,
    Used,
    CreatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
