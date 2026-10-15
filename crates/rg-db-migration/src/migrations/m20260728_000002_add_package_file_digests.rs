use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260728_000002_add_package_file_digests"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The registries do not agree on one digest: npm and Composer publish
        // `dist.shasum`, which both protocols define as SHA-1, while npm's
        // `dist.integrity` is a SHA-512 SRI string. One `sha256` column meant
        // every adapter had to pass it off as whatever its protocol asked for.
        //
        // Nullable: a file published before this migration has no recorded
        // SHA-1 or SHA-512, and the metadata routes then leave those fields out
        // rather than publish a digest that is not the file's — an absent
        // checksum is skipped by the client, a wrong one fails the install.
        if !manager.has_column("package_files", "sha1").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PackageFiles::Table)
                        .add_column(ColumnDef::new(PackageFiles::Sha1).string().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("package_files", "sha512").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PackageFiles::Table)
                        .add_column(ColumnDef::new(PackageFiles::Sha512).string().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("package_files", "sha512").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PackageFiles::Table)
                        .drop_column(PackageFiles::Sha512)
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_column("package_files", "sha1").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PackageFiles::Table)
                        .drop_column(PackageFiles::Sha1)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PackageFiles {
    Table,
    Sha1,
    Sha512,
}
