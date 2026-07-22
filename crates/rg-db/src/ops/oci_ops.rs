//! OCI Registry database operations.
//!
//! Covers oci_repository, oci_manifest, oci_blob, and oci_upload tables.

use crate::entities::{oci_blob, oci_manifest, oci_repository, oci_upload};
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::*;

// ── OCI Repository ─────────────────────────────────────────

/// Find an OCI repository by IronForge repo_id.
pub async fn find_repo_by_id(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Option<oci_repository::Model>, DbErr> {
    use oci_repository::Entity as OciRepo;
    OciRepo::find()
        .filter(oci_repository::Column::RepoId.eq(repo_id))
        .one(db)
        .await
}

/// Find or create an OCI repository.
#[allow(clippy::too_many_arguments)]
pub async fn find_or_create_repo(
    db: &DatabaseConnection,
    repo_id: i64,
    namespace: &str,
    owner_id: i64,
) -> Result<oci_repository::Model, DbErr> {
    if let Some(r) = find_repo_by_id(db, repo_id).await? {
        return Ok(r);
    }
    let now = Utc::now();
    let m = oci_repository::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        namespace: Set(namespace.to_string()),
        owner_id: Set(owner_id),
        is_public: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };
    m.insert(db).await
}

// ── OCI Manifest ────────────────────────────────────────────

/// Find a manifest by digest.
pub async fn find_manifest_by_digest(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
) -> Result<Option<oci_manifest::Model>, DbErr> {
    use oci_manifest::Entity as Manifest;
    Manifest::find()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Digest.eq(digest))
        .one(db)
        .await
}

/// Find a manifest by tag.
pub async fn find_manifest_by_tag(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    tag: &str,
) -> Result<Option<oci_manifest::Model>, DbErr> {
    use oci_manifest::Entity as Manifest;
    Manifest::find()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Tag.eq(tag))
        .one(db)
        .await
}

/// List all tags for an OCI repository.
pub async fn list_tags(db: &DatabaseConnection, oci_repo_id: i64) -> Result<Vec<String>, DbErr> {
    use oci_manifest::Entity as Manifest;
    let manifests = Manifest::find()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Tag.is_not_null())
        .all(db)
        .await?;
    Ok(manifests.into_iter().filter_map(|m| m.tag).collect())
}

/// Insert a new manifest.
#[allow(clippy::too_many_arguments)]
pub async fn insert_manifest(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
    tag: Option<&str>,
    media_type: &str,
    size: i64,
    manifest_json: &str,
    schema_version: i32,
    push_by: Option<i64>,
) -> Result<oci_manifest::Model, DbErr> {
    let now = Utc::now();
    let m = oci_manifest::ActiveModel {
        id: NotSet,
        oci_repository_id: Set(oci_repo_id),
        digest: Set(digest.to_string()),
        tag: Set(tag.map(|s| s.to_string())),
        media_type: Set(media_type.to_string()),
        size: Set(size),
        manifest_json: Set(manifest_json.to_string()),
        schema_version: Set(schema_version),
        push_by: Set(push_by),
        created_at: Set(now),
        updated_at: Set(now),
    };
    m.insert(db).await
}

/// Update a manifest's tag (move a tag to a new digest).
#[allow(clippy::too_many_arguments)]
pub async fn update_manifest_tag(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    tag: &str,
    new_digest: &str,
    new_media_type: &str,
    new_size: i64,
    new_manifest_json: &str,
    new_schema_version: i32,
    push_by: Option<i64>,
) -> Result<oci_manifest::Model, DbErr> {
    // Delete existing manifest with this tag
    delete_manifest_by_tag(db, oci_repo_id, tag).await?;
    // Insert new manifest
    insert_manifest(
        db,
        oci_repo_id,
        new_digest,
        Some(tag),
        new_media_type,
        new_size,
        new_manifest_json,
        new_schema_version,
        push_by,
    )
    .await
}

/// Delete a manifest by tag.
pub async fn delete_manifest_by_tag(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    tag: &str,
) -> Result<u64, DbErr> {
    use oci_manifest::Entity as Manifest;
    let result = Manifest::delete_many()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Tag.eq(tag))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

// ── OCI Blob ────────────────────────────────────────────────

/// Find a blob by digest.
pub async fn find_blob(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
) -> Result<Option<oci_blob::Model>, DbErr> {
    use oci_blob::Entity as Blob;
    Blob::find()
        .filter(oci_blob::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_blob::Column::Digest.eq(digest))
        .one(db)
        .await
}

/// Insert a new blob record.
pub async fn insert_blob(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
    media_type: &str,
    size: i64,
    storage_path: &str,
) -> Result<oci_blob::Model, DbErr> {
    let now = Utc::now();
    let m = oci_blob::ActiveModel {
        id: NotSet,
        oci_repository_id: Set(oci_repo_id),
        digest: Set(digest.to_string()),
        media_type: Set(media_type.to_string()),
        size: Set(size),
        storage_path: Set(storage_path.to_string()),
        ref_count: Set(0),
        created_at: Set(now),
    };
    m.insert(db).await
}

/// Increment blob reference count.
pub async fn increment_blob_ref(db: &DatabaseConnection, blob_id: i64) -> Result<(), DbErr> {
    use oci_blob::Entity as Blob;
    Blob::update_many()
        .col_expr(
            oci_blob::Column::RefCount,
            Expr::col(oci_blob::Column::RefCount).add(1),
        )
        .filter(oci_blob::Column::Id.eq(blob_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Decrement blob reference count.
pub async fn decrement_blob_ref(db: &DatabaseConnection, blob_id: i64) -> Result<(), DbErr> {
    use oci_blob::Entity as Blob;
    Blob::update_many()
        .col_expr(
            oci_blob::Column::RefCount,
            Expr::col(oci_blob::Column::RefCount).sub(1),
        )
        .filter(oci_blob::Column::Id.eq(blob_id))
        .exec(db)
        .await?;
    Ok(())
}

// ── OCI Upload ──────────────────────────────────────────────

/// Create a new upload session.
pub async fn create_upload(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    uuid: &str,
    upload_path: &str,
) -> Result<oci_upload::Model, DbErr> {
    let now = Utc::now();
    let expires = now + chrono::Duration::hours(24);
    let m = oci_upload::ActiveModel {
        id: NotSet,
        oci_repository_id: Set(oci_repo_id),
        uuid: Set(uuid.to_string()),
        digest: Set(None),
        bytes_uploaded: Set(0),
        upload_path: Set(upload_path.to_string()),
        created_at: Set(now),
        expires_at: Set(expires),
    };
    m.insert(db).await
}

/// Find an upload by UUID.
pub async fn find_upload(
    db: &DatabaseConnection,
    uuid: &str,
) -> Result<Option<oci_upload::Model>, DbErr> {
    use oci_upload::Entity as Upload;
    Upload::find()
        .filter(oci_upload::Column::Uuid.eq(uuid))
        .one(db)
        .await
}

/// Update upload progress.
pub async fn update_upload_progress(
    db: &DatabaseConnection,
    uuid: &str,
    bytes_uploaded: i64,
) -> Result<(), DbErr> {
    use oci_upload::Entity as Upload;
    Upload::update_many()
        .col_expr(
            oci_upload::Column::BytesUploaded,
            Expr::value(bytes_uploaded),
        )
        .filter(oci_upload::Column::Uuid.eq(uuid))
        .exec(db)
        .await?;
    Ok(())
}

/// Complete an upload (set digest).
pub async fn complete_upload(
    db: &DatabaseConnection,
    uuid: &str,
    digest: &str,
) -> Result<(), DbErr> {
    use oci_upload::Entity as Upload;
    Upload::update_many()
        .col_expr(oci_upload::Column::Digest, Expr::value(digest.to_string()))
        .filter(oci_upload::Column::Uuid.eq(uuid))
        .exec(db)
        .await?;
    Ok(())
}

/// Delete an upload session.
pub async fn delete_upload(db: &DatabaseConnection, uuid: &str) -> Result<u64, DbErr> {
    use oci_upload::Entity as Upload;
    let result = Upload::delete_many()
        .filter(oci_upload::Column::Uuid.eq(uuid))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

/// Clean up expired uploads.
pub async fn cleanup_expired_uploads(db: &DatabaseConnection) -> Result<u64, DbErr> {
    use oci_upload::Entity as Upload;
    let now = Utc::now();
    let result = Upload::delete_many()
        .filter(oci_upload::Column::ExpiresAt.lt(now))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}
