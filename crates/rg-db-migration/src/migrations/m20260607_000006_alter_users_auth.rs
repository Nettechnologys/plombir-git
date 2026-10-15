//! Migration: alter users table for LDAP/SSO/2FA support.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite supports ADD COLUMN one at a time.
        // auth_provider: "local" | "ldap" | "oauth2"
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(UsersCol::AuthProvider)
                            .string_len(20)
                            .not_null()
                            .default("local"),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(UsersCol::LdapDn).string_len(512).null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(UsersCol::LdapUid).string_len(255).null())
                    .to_owned(),
            )
            .await?;

        // totp_secret is encrypted with AES-GCM before storage
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(UsersCol::TotpSecret).string_len(128).null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(UsersCol::MfaEnabled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(UsersCol::MfaType).string_len(20).null())
                    .to_owned(),
            )
            .await?;

        // backup codes: JSON array of hashed codes, stored as TEXT
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(UsersCol::BackupCodes).text().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(UsersCol::LastLoginAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(UsersCol::LoginAttempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(UsersCol::LockedUntil)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite does not support DROP COLUMN without recreating the table.
        Ok(())
    }
}

#[derive(Iden)]
enum Users {
    Table,
}

#[derive(Iden)]
enum UsersCol {
    AuthProvider,
    LdapDn,
    LdapUid,
    TotpSecret,
    MfaEnabled,
    MfaType,
    BackupCodes,
    LastLoginAt,
    LoginAttempts,
    LockedUntil,
}
