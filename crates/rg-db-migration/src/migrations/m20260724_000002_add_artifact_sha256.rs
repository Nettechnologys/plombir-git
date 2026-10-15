use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260724_000002_add_artifact_sha256"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Nullable: pre-existing CI artifacts have no recorded digest, and
        // integrity verification skips them rather than failing an old download.
        if !manager.has_column("artifacts", "sha256").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Artifacts::Table)
                        .add_column(ColumnDef::new(Artifacts::Sha256).string().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("artifacts", "sha256").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Artifacts::Table)
                        .drop_column(Artifacts::Sha256)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Artifacts {
    Table,
    Sha256,
}
