//! Corrective migration: rename `import_task` to `import_tasks`.
//!
//! The original import task migration used `#[derive(Iden)] enum ImportTask`
//! for the table identifier, which resolves to the singular table name
//! `import_task`. The entity maps to `import_tasks`, so fresh databases need
//! this compatibility rename when the singular table exists.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260629_000001_rename_import_task_plural"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("import_task").await? && !manager.has_table("import_tasks").await? {
            manager
                .rename_table(
                    Table::rename()
                        .table(Alias::new("import_task"), Alias::new("import_tasks"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("import_tasks").await? && !manager.has_table("import_task").await? {
            manager
                .rename_table(
                    Table::rename()
                        .table(Alias::new("import_tasks"), Alias::new("import_task"))
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}
