//! Give webhooks an explicit owner scope while preserving repository hooks.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("webhooks", "scope_kind").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Webhooks::Table)
                        .add_column(
                            ColumnDef::new(Webhooks::ScopeKind)
                                .string()
                                .not_null()
                                .default("repo"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("webhooks", "scope_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Webhooks::Table)
                        .add_column(
                            ColumnDef::new(Webhooks::ScopeId)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE webhooks SET scope_id = repo_id WHERE scope_kind = 'repo' AND scope_id = 0",
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_webhook_scope")
                    .table(Webhooks::Table)
                    .col(Webhooks::ScopeKind)
                    .col(Webhooks::ScopeId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping scope metadata would turn org/system hooks into repository
        // hooks with repo_id=0. Keep it on rollback.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Webhooks {
    Table,
    ScopeKind,
    ScopeId,
}
