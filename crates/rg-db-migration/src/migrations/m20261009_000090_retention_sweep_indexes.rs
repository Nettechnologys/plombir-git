//! Indexes the retention sweep reads by (card_f25c98fdf3ee).
//!
//! `rg_core::retention` deletes webhook deliveries and notifications older than
//! a window, oldest first, a bounded batch at a time. Both tables were indexed
//! only for their per-owner listings (`webhook_id` / `user_id` first), so every
//! batch's "oldest rows before the cutoff" walked the whole table — and the
//! tables are big precisely when the sweep has work to do. `login_logs` already
//! carries `idx_login_logs_created_at`.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261009_000090_retention_sweep_indexes"
    }
}

/// `(name, table, columns)` of every index this migration creates.
const CREATED: &[(&str, &str, &[&str])] = &[
    (
        "idx_webhook_deliveries_created",
        "webhook_deliveries",
        &["created_at", "id"],
    ),
    (
        "idx_notifications_created",
        "notifications",
        &["created_at", "id"],
    ),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, table, columns) in CREATED {
            if manager.has_index(table, name).await? {
                continue;
            }
            let mut index = Index::create();
            index.name(*name).table(Alias::new(*table));
            for column in *columns {
                index.col(Alias::new(*column));
            }
            manager.create_index(index.to_owned()).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, table, _) in CREATED.iter().rev() {
            manager
                .drop_index(
                    Index::drop()
                        .name(*name)
                        .table(Alias::new(*table))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
