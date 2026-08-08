//! card_dd6ae4f40206: drop the import user mapping nothing ever wrote.
//!
//! `import_tasks.user_mapping` was documented as "JSON mapping of external user
//! logins to local user IDs". Every production write set it to `NULL` and
//! nothing read it back — the column existed only in the migration, the entity
//! and three `Set(None)`s.
//!
//! The mapping it was meant to hold did not exist either. `map_users` inserted
//! `task.user_id` for every login it was given, so the "map" was a constant,
//! and the walk that fed it listed the source's issues and merge requests even
//! when neither was being imported.
//!
//! Keeping the column would have been an invitation to fill it by matching
//! logins, which is the one implementation that must not be added by default:
//! the source platform's `alice` and this instance's `alice` are unrelated
//! accounts, so a name match would let an importer publish issues and reviews
//! under a colleague's name. A mapping the importer states explicitly needs an
//! API and a UI to state it through; when that exists it can add a column that
//! something writes.
//!
//! `down` restores the nullable column. Every value it ever held was `NULL`, so
//! reversing this migration loses nothing at all.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260808_000004_drop_import_task_user_mapping"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("import_tasks", "user_mapping").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(ImportTasks::Table)
                        .drop_column(ImportTasks::UserMapping)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("import_tasks", "user_mapping").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(ImportTasks::Table)
                        .add_column(ColumnDef::new(ImportTasks::UserMapping).text().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ImportTasks {
    Table,
    UserMapping,
}
