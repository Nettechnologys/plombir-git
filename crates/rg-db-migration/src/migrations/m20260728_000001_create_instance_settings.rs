//! Migration: create `instance_settings` — the durable home of the
//! instance-wide switches the admin API toggles (maintenance mode, banner).
//!
//! One row, always id 1. The settings describe the instance, and an instance
//! has exactly one of them, so the table carries no key beyond the constant
//! primary key: a second row would be a second answer to a question that has
//! one. Absence of the row is meaningful and legal — it means "never
//! configured", which the reader resolves to the defaults.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(InstanceSettings::Table)
                    .if_not_exists()
                    // Not auto-increment: the writer always names id 1, which is
                    // what keeps the singleton a singleton.
                    .col(
                        ColumnDef::new(InstanceSettings::Id)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(InstanceSettings::MaintenanceMode)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    // `NULL` is "no banner", distinct from an empty message.
                    .col(
                        ColumnDef::new(InstanceSettings::BannerMessage)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(InstanceSettings::BannerType)
                            .string_len(32)
                            .not_null()
                            .default(""),
                    )
                    .col(
                        ColumnDef::new(InstanceSettings::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(InstanceSettings::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum InstanceSettings {
    Table,
    Id,
    MaintenanceMode,
    BannerMessage,
    BannerType,
    UpdatedAt,
}
