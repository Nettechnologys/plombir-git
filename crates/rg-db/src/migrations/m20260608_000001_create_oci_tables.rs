use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. OCI repositories — links an IronForge repo to an OCI namespace
        manager
            .create_table(
                Table::create()
                    .table(OciRepository::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OciRepository::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OciRepository::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OciRepository::Namespace).string().not_null())
                    .col(
                        ColumnDef::new(OciRepository::OwnerId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OciRepository::IsPublic)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(OciRepository::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OciRepository::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_oci_repo_namespace")
                    .table(OciRepository::Table)
                    .col(OciRepository::RepoId)
                    .col(OciRepository::Namespace)
                    .to_owned(),
            )
            .await?;

        // 2. OCI manifests — maps tag/digest pairs + manifest JSON content
        manager
            .create_table(
                Table::create()
                    .table(OciManifest::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OciManifest::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OciManifest::OciRepositoryId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OciManifest::Digest).string().not_null())
                    .col(ColumnDef::new(OciManifest::Tag).string().null())
                    .col(ColumnDef::new(OciManifest::MediaType).string().not_null())
                    .col(ColumnDef::new(OciManifest::Size).big_integer().not_null())
                    .col(ColumnDef::new(OciManifest::ManifestJson).text().not_null())
                    .col(
                        ColumnDef::new(OciManifest::SchemaVersion)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OciManifest::PushBy).big_integer().null())
                    .col(
                        ColumnDef::new(OciManifest::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OciManifest::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Digest must be unique per repository (only one manifest per digest)
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_oci_manifest_digest")
                    .table(OciManifest::Table)
                    .col(OciManifest::OciRepositoryId)
                    .col(OciManifest::Digest)
                    .to_owned(),
            )
            .await?;

        // Tag uniqueness per repository
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_oci_manifest_tag")
                    .table(OciManifest::Table)
                    .col(OciManifest::OciRepositoryId)
                    .col(OciManifest::Tag)
                    .to_owned(),
            )
            .await?;

        // 3. OCI blobs — content-addressed layer storage
        manager
            .create_table(
                Table::create()
                    .table(OciBlob::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OciBlob::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OciBlob::OciRepositoryId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OciBlob::Digest).string().not_null())
                    .col(ColumnDef::new(OciBlob::MediaType).string().not_null())
                    .col(ColumnDef::new(OciBlob::Size).big_integer().not_null())
                    .col(ColumnDef::new(OciBlob::StoragePath).string().not_null())
                    .col(
                        ColumnDef::new(OciBlob::RefCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(OciBlob::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_oci_blob_digest")
                    .table(OciBlob::Table)
                    .col(OciBlob::OciRepositoryId)
                    .col(OciBlob::Digest)
                    .to_owned(),
            )
            .await?;

        // 4. OCI uploads — tracks in-progress chunked upload sessions
        manager
            .create_table(
                Table::create()
                    .table(OciUpload::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OciUpload::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OciUpload::OciRepositoryId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OciUpload::Uuid).string().not_null())
                    .col(ColumnDef::new(OciUpload::Digest).string().null())
                    .col(
                        ColumnDef::new(OciUpload::BytesUploaded)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(OciUpload::UploadPath).string().not_null())
                    .col(
                        ColumnDef::new(OciUpload::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OciUpload::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_oci_upload_uuid")
                    .table(OciUpload::Table)
                    .col(OciUpload::Uuid)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OciUpload::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(OciBlob::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(OciManifest::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(OciRepository::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum OciRepository {
    Table,
    Id,
    RepoId,
    Namespace,
    OwnerId,
    IsPublic,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum OciManifest {
    Table,
    Id,
    OciRepositoryId,
    Digest,
    Tag,
    MediaType,
    Size,
    ManifestJson,
    SchemaVersion,
    PushBy,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum OciBlob {
    Table,
    Id,
    OciRepositoryId,
    Digest,
    MediaType,
    Size,
    StoragePath,
    RefCount,
    CreatedAt,
}

#[derive(DeriveIden)]
enum OciUpload {
    Table,
    Id,
    OciRepositoryId,
    Uuid,
    Digest,
    BytesUploaded,
    UploadPath,
    CreatedAt,
    ExpiresAt,
}
