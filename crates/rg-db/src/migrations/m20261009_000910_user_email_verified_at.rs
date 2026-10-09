//! `users.email_verified_at` — when the account last proved that it receives
//! mail at `users.email` (card_2296f052332b).
//!
//! The address was whatever somebody typed. `verify-email` registration and a
//! confirmed address change already proved theirs by a mailed link, but left
//! no mark, so nothing could tell a proved address from a typed one — not the
//! operator, not the account holder, not a future consumer that must only
//! trust a proved address. `NULL` is "not proved": every row that exists today
//! starts there, because no row's address was ever recorded as proved.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("users", "email_verified_at").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(
                        ColumnDef::new(Users::EmailVerifiedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::EmailVerifiedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum Users {
    Table,
    EmailVerifiedAt,
}
