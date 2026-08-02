//! Migration: add a durable, encrypted proof of the instance encryption key.
//!
//! Sampling user secrets catches a wrong key on existing databases, but a fresh
//! database has nothing to sample. Without this marker, replacing the generated
//! key file before the first TOTP/CI secret is stored is undetectable.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(EncryptionKeyCheck::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EncryptionKeyCheck::Id)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EncryptionKeyCheck::ValueEncrypted)
                            .text()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EncryptionKeyCheck::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum EncryptionKeyCheck {
    Table,
    Id,
    ValueEncrypted,
}
