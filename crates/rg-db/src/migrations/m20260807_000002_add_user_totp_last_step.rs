//! card_9585caf5692d: give a successful TOTP check something to spend.
//!
//! A TOTP code is a pure function of the shared secret and the clock, so the
//! secret cannot tell a first use from a replay. With `skew = 1` over a
//! 30-second step one code is accepted for the previous, current and next step
//! — around 90 seconds during which an intercepted code passed the second
//! factor as many times as it was presented. RFC 6238 §5.2 requires the
//! opposite: "the verifier MUST NOT accept the second attempt of the OTP after
//! the successful validation has been issued for the first OTP".
//!
//! `NULL` is an account that has never completed a TOTP login, which is every
//! account at the moment this migration runs — so no live enrolment is
//! invalidated by adding the column.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260807_000002_add_user_totp_last_step"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "totp_last_step").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(ColumnDef::new(Users::TotpLastStep).big_integer().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("users", "totp_last_step").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .drop_column(Users::TotpLastStep)
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
    TotpLastStep,
}
