//! What an account needs to look after itself, and what an administrator needs
//! to hand one over.
//!
//! * `users.password_change_required` — set on a password an administrator
//!   chose (a new account, a reset), so the first sign-in with it is refused
//!   until the holder has replaced it with one only they know
//!   (card_9f18b657580b).
//! * `email_confirmations` — one row per link mailed out to prove an address:
//!   a registration waiting for its address to be confirmed, or an account
//!   moving to a new address (card_45f98ab2fe1a, card_ca894e30ac80). A pending
//!   registration is a row here and nothing in `users`, so an address that was
//!   never confirmed holds no account, no name and no session. Only the SHA-256
//!   of the token is stored, like the password-reset tokens.
//! * `user_avatars` — the picture an account uploaded. In the database rather
//!   than in blob storage on purpose: a few hundred kilobytes per account, and
//!   the foreign key takes it with the account. Blob storage would need its own
//!   cleanup on account deletion — exactly the class of leftover the deletion
//!   phases spent weeks closing.
//!
//! Both new tables cascade on the account: a confirmation or a picture is
//! nothing without the account it belongs to.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UNIQUE_TOKEN: &str = "uq_email_confirmations_token_hash";
const BY_EMAIL: &str = "idx_email_confirmations_email";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("users", "password_change_required")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(
                            ColumnDef::new(Users::PasswordChangeRequired)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        manager
            .create_table(
                Table::create()
                    .table(EmailConfirmations::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EmailConfirmations::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // `registration` or `email_change`.
                    .col(
                        ColumnDef::new(EmailConfirmations::Purpose)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EmailConfirmations::TokenHash)
                            .string_len(64)
                            .not_null(),
                    )
                    // The address being proved.
                    .col(
                        ColumnDef::new(EmailConfirmations::Email)
                            .string_len(255)
                            .not_null(),
                    )
                    // The account moving to `email`; null for a registration,
                    // which has no account yet.
                    .col(
                        ColumnDef::new(EmailConfirmations::UserId)
                            .big_integer()
                            .null(),
                    )
                    // A registration's chosen name and its Argon2 hash — what
                    // the account is created from once the address is proved.
                    .col(
                        ColumnDef::new(EmailConfirmations::Username)
                            .string_len(64)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(EmailConfirmations::PasswordHash)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(EmailConfirmations::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EmailConfirmations::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(EmailConfirmations::Table, EmailConfirmations::UserId)
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
                    .unique()
                    .name(UNIQUE_TOKEN)
                    .table(EmailConfirmations::Table)
                    .col(EmailConfirmations::TokenHash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(BY_EMAIL)
                    .table(EmailConfirmations::Table)
                    .col(EmailConfirmations::Email)
                    .to_owned(),
            )
            .await?;

        // MySQL's plain BLOB stops at 64 KiB, under the picture ceiling the API
        // accepts; MEDIUMBLOB is the smallest that holds it. The other two
        // backends have one binary type each.
        let bytes = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::MySql => ColumnDef::new(UserAvatars::Bytes)
                .custom(Alias::new("MEDIUMBLOB"))
                .not_null()
                .to_owned(),
            _ => ColumnDef::new(UserAvatars::Bytes)
                .blob()
                .not_null()
                .to_owned(),
        };
        manager
            .create_table(
                Table::create()
                    .table(UserAvatars::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(UserAvatars::UserId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(UserAvatars::ContentType)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(UserAvatars::Sha256)
                            .string_len(64)
                            .not_null(),
                    )
                    .col(bytes)
                    .col(
                        ColumnDef::new(UserAvatars::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(UserAvatars::Table, UserAvatars::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(UserAvatars::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(EmailConfirmations::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        if manager
            .has_column("users", "password_change_required")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .drop_column(Users::PasswordChangeRequired)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
    PasswordChangeRequired,
}

#[derive(Iden)]
enum EmailConfirmations {
    Table,
    Id,
    Purpose,
    TokenHash,
    Email,
    UserId,
    Username,
    PasswordHash,
    ExpiresAt,
    CreatedAt,
}

#[derive(Iden)]
enum UserAvatars {
    Table,
    UserId,
    ContentType,
    Sha256,
    Bytes,
    UpdatedAt,
}
