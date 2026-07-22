//! Rename board/time tables from singular to plural to match entity declarations.
//!
//! Migration `m20260607_000002_create_boards` used `#[derive(Iden)] enum Board { Table }` which
//! generates the singular name "board". Entities declare `#[sea_orm(table_name = "boards")]`.
//! Same issue for board_column→board_columns, board_card→board_cards, time_entry→time_entries.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260617_000002_rename_board_time_tables_plural"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // board_cards must be renamed before board_columns (FK dependency)
        // board_columns before boards; wiki_revision → wiki_revisions
        for (old, new) in &[
            ("board_card", "board_cards"),
            ("board_column", "board_columns"),
            ("board", "boards"),
            ("time_entry", "time_entries"),
            ("wiki_revision", "wiki_revisions"),
        ] {
            // Skip if the old table doesn't exist (e.g. already renamed on a prior run)
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
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (old, new) in &[
            ("board_cards", "board_card"),
            ("board_columns", "board_column"),
            ("boards", "board"),
            ("time_entries", "time_entry"),
            ("wiki_revisions", "wiki_revision"),
        ] {
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
        Ok(())
    }
}
