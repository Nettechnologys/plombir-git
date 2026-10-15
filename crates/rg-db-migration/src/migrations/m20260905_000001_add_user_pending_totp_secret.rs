//! card_08400088bb40: give an enrolment in progress a slot of its own.
//!
//! `POST /users/mfa/setup` wrote the freshly generated secret straight into
//! `users.totp_secret` — the column the login path verifies against — while
//! `users.mfa_enabled` stayed `true`. An account whose owner merely re-opened
//! the wizard and never reached `POST /users/mfa/enable` was left demanding a
//! second factor whose secret no authenticator held: the app still computed
//! codes from the old secret, the server verified against the new one, and the
//! only way back in was a backup code.
//!
//! The new material now waits here until the step that proves possession of it
//! promotes it. `NULL` is "no enrolment in flight", which is every account at
//! the moment this migration runs — so no live factor is touched by adding the
//! columns, and an instance rolled back to the previous binary keeps working
//! off `totp_secret` exactly as before.
//!
//! `pending_totp_secret` holds the same AES-GCM ciphertext shape as
//! `totp_secret` and is registered beside it in
//! `rg_core::auth::encrypted_columns`, so the startup preflight and a key
//! rotation both see it. `pending_totp_secret_at` is the issue time, which is
//! what bounds how long a handed-out secret may still be armed.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260905_000001_add_user_pending_totp_secret"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "pending_totp_secret").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(
                            ColumnDef::new(Users::PendingTotpSecret)
                                .string_len(128)
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("users", "pending_totp_secret_at")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(
                            ColumnDef::new(Users::PendingTotpSecretAt)
                                .timestamp_with_time_zone()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("users", "pending_totp_secret_at")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .drop_column(Users::PendingTotpSecretAt)
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_column("users", "pending_totp_secret").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .drop_column(Users::PendingTotpSecret)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    PendingTotpSecret,
    PendingTotpSecretAt,
}
