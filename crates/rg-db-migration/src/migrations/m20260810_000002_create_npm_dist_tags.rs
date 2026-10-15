//! Persist npm distribution tags independently from package versions.
//!
//! A dist-tag is a mutable selector (`latest`, `beta`, `next`, ...), not a
//! property of the version it currently names.  `npm publish --tag beta`
//! carries that selector in the publish packument, and acknowledging the
//! publish without storing it makes the version unreachable by the spelling
//! the client just asked for.
//!
//! `npm_dist_tag_sets` is deliberately separate from the tag rows.  Its
//! presence distinguishes a legacy package, whose historical `latest` must be
//! derived once for compatibility, from an initialized package whose last tag
//! was explicitly removed and whose correct tag map is therefore empty.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(NpmDistTagSets::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NpmDistTagSets::PackageId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    // The initializer reads this token back after an
                    // INSERT-ON-CONFLICT.  That makes exactly one concurrent
                    // request responsible for materializing legacy `latest`.
                    .col(
                        ColumnDef::new(NpmDistTagSets::InitializationToken)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(NpmDistTagSets::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(NpmDistTagSets::Table, NpmDistTagSets::PackageId)
                            .to(Packages::Table, Packages::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(NpmDistTags::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(NpmDistTags::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(NpmDistTags::PackageId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(NpmDistTags::Tag).string().not_null())
                    .col(
                        ColumnDef::new(NpmDistTags::VersionId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(NpmDistTags::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(NpmDistTags::Table, NpmDistTags::PackageId)
                            .to(Packages::Table, Packages::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(NpmDistTags::Table, NpmDistTags::VersionId)
                            .to(PackageVersions::Table, PackageVersions::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_npm_dist_tags_package_tag")
                            .col(NpmDistTags::PackageId)
                            .col(NpmDistTags::Tag)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(NpmDistTags::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(NpmDistTagSets::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum NpmDistTagSets {
    Table,
    PackageId,
    InitializationToken,
    CreatedAt,
}

#[derive(Iden)]
enum NpmDistTags {
    Table,
    Id,
    PackageId,
    Tag,
    VersionId,
    UpdatedAt,
}

#[derive(Iden)]
enum Packages {
    Table,
    Id,
}

#[derive(Iden)]
enum PackageVersions {
    Table,
    Id,
}
