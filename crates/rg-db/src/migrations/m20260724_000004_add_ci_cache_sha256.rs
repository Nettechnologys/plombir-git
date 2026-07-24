use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260724_000004_add_ci_cache_sha256"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Nullable: pre-existing CI cache archives have no recorded content
        // digest (`key_hash` is a hash of the cache *key*, not the payload), and
        // integrity verification skips them rather than failing an old download.
        if !manager.has_column("ci_cache_entries", "sha256").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(CiCacheEntries::Table)
                        .add_column(ColumnDef::new(CiCacheEntries::Sha256).string().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("ci_cache_entries", "sha256").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(CiCacheEntries::Table)
                        .drop_column(CiCacheEntries::Sha256)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum CiCacheEntries {
    Table,
    Sha256,
}
