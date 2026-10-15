//! Account-owned public keys used to verify Git commit signatures.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SigningKeys::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SigningKeys::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SigningKeys::UserId).big_integer().not_null())
                    .col(ColumnDef::new(SigningKeys::Title).string().not_null())
                    .col(ColumnDef::new(SigningKeys::Kind).string().not_null())
                    .col(ColumnDef::new(SigningKeys::PublicKey).text().not_null())
                    .col(
                        ColumnDef::new(SigningKeys::Fingerprint)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(SigningKeys::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(SigningKeys::Table, SigningKeys::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_commit_signing_keys_user_id")
                    .table(SigningKeys::Table)
                    .col(SigningKeys::UserId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SigningKeys::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum SigningKeys {
    #[iden = "commit_signing_keys"]
    Table,
    Id,
    UserId,
    Title,
    Kind,
    PublicKey,
    Fingerprint,
    CreatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
