//! Migration: add `deleted_at` columns to user, org, and issue tables
//! for soft-delete support.
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260616_000002_add_soft_delete_columns"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Idempotent: skip tables/columns that already exist. This migration
        // previously crashed mid-way (the `organizations` table did not yet
        // exist before m20260616_0000015 renamed it), so on re-run the
        // `users.deleted_at` column may already be present.
        for table in ["users", "organizations", "issues"] {
            if manager.has_table(table).await? && !manager.has_column(table, "deleted_at").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(
                                ColumnDef::new(Alias::new("deleted_at"))
                                    .timestamp_with_time_zone()
                                    .null(),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in ["users", "organizations", "issues"] {
            if manager.has_column(table, "deleted_at").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .drop_column(Alias::new("deleted_at"))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}
