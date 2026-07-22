//! Corrective migration: phase8 (m20260424_000009) created the org/team
//! tables via `#[derive(Iden)]`, which produced **singular** table names
//! (`organization`, `organization_member`, `team`, `team_member`). The
//! SeaORM entities, however, declare **plural** `table_name`s
//! (`organizations`, `organization_members`, `teams`, `team_members`), so
//! every org/team query failed at runtime with "no such table". The
//! notification table hit the same bug and was patched separately in
//! m20260511_000002; this migration fixes the remaining four tables.
//!
//! Strategy: rename the singular table to its plural name when (and only
//! when) the singular table exists and the plural one does not yet. This is
//! idempotent and safe on databases carrying the historical broken schema.
//! Fresh databases now create the plural names directly, making this a no-op.
//! The migrator registers this correction before the first migration that
//! references `organizations`, so strict-FK backends can migrate from scratch.
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260616_0000015_rename_org_team_plural"
    }
}

const RENAMES: &[(&str, &str)] = &[
    ("organization", "organizations"),
    ("organization_member", "organization_members"),
    ("team", "teams"),
    ("team_member", "team_members"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (old, new) in RENAMES {
            if manager.has_table(*old).await? && !manager.has_table(*new).await? {
                manager
                    .rename_table(
                        Table::rename()
                            .table(Alias::new(*old), Alias::new(*new))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        // Drop the dead singular `notification` table left over from phase8;
        // the live plural `notifications` table is created by m20260511_000002.
        if manager.has_table("notification").await? && manager.has_table("notifications").await? {
            manager
                .drop_table(Table::drop().table(Alias::new("notification")).to_owned())
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (old, new) in RENAMES {
            if manager.has_table(*new).await? && !manager.has_table(*old).await? {
                manager
                    .rename_table(
                        Table::rename()
                            .table(Alias::new(*new), Alias::new(*old))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}
