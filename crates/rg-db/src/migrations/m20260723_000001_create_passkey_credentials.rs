//! Migration: create `passkey_credentials` table (WebAuthn / passkeys).
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PasskeyCredentials::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PasskeyCredentials::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PasskeyCredentials::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    // URL-safe base64 of the raw credential id; unique so an
                    // authenticator cannot be enrolled twice.
                    .col(
                        ColumnDef::new(PasskeyCredentials::CredentialId)
                            .string_len(512)
                            .not_null()
                            .unique_key(),
                    )
                    // Serialized `webauthn_rs::prelude::Passkey` (JSON).
                    .col(
                        ColumnDef::new(PasskeyCredentials::Passkey)
                            .text()
                            .not_null(),
                    )
                    // User-supplied label (e.g. "YubiKey", "MacBook Touch ID").
                    .col(
                        ColumnDef::new(PasskeyCredentials::Name)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PasskeyCredentials::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PasskeyCredentials::LastUsedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(PasskeyCredentials::Table, PasskeyCredentials::UserId)
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
                    .table(PasskeyCredentials::Table)
                    .name("idx_passkey_credentials_user_id")
                    .col(PasskeyCredentials::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PasskeyCredentials::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum PasskeyCredentials {
    Table,
    Id,
    UserId,
    CredentialId,
    Passkey,
    Name,
    CreatedAt,
    LastUsedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
