use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260724_000003_add_release_asset_attestation"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Nullable and opt-in: an asset only carries a detached DSSE attestation
        // envelope once it has been explicitly signed. Stored as text alongside
        // the asset row rather than embedded in the asset bytes (detached).
        if !manager.has_column("release_assets", "attestation").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(ReleaseAssets::Table)
                        .add_column(ColumnDef::new(ReleaseAssets::Attestation).text().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("release_assets", "attestation").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(ReleaseAssets::Table)
                        .drop_column(ReleaseAssets::Attestation)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ReleaseAssets {
    Table,
    Attestation,
}
