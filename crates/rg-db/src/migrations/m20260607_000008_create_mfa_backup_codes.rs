//! Migration: create `mfa_backup_codes` table.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MfaBackupCodes::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MfaBackupCodes::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(MfaBackupCodes::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MfaBackupCodes::CodeHash)
                            .string_len(128)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MfaBackupCodes::Used)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(MfaBackupCodes::UsedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(MfaBackupCodes::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MfaBackupCodes::Table, MfaBackupCodes::UserId)
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
                    .table(MfaBackupCodes::Table)
                    .name("idx_mfa_backup_codes_user_id")
                    .col(MfaBackupCodes::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MfaBackupCodes::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum MfaBackupCodes {
    Table,
    Id,
    UserId,
    CodeHash,
    Used,
    UsedAt,
    CreatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
