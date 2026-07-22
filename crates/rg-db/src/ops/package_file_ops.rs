use crate::entities::{package_file, package_file::Entity as PackageFile};
use sea_orm::*;

/// Create a new package file entry.
pub async fn create(
    db: &DatabaseConnection,
    version_id: i64,
    filename: &str,
    size: i64,
    sha256: Option<&str>,
    storage_path: &str,
) -> Result<package_file::Model, DbErr> {
    use package_file::ActiveModel;
    let now = chrono::Utc::now();

    let f = ActiveModel {
        id: sea_orm::NotSet,
        version_id: Set(version_id),
        filename: Set(filename.to_string()),
        size: Set(size),
        sha256: Set(sha256.map(|s| s.to_string())),
        storage_path: Set(storage_path.to_string()),
        created_at: Set(now),
    };

    f.insert(db).await
}

/// List all files for a package version.
pub async fn list_by_version(
    db: &DatabaseConnection,
    version_id: i64,
) -> Result<Vec<package_file::Model>, DbErr> {
    PackageFile::find()
        .filter(package_file::Column::VersionId.eq(version_id))
        .all(db)
        .await
}

/// Find a file by id.
pub async fn find_by_id(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<package_file::Model>, DbErr> {
    PackageFile::find_by_id(id).one(db).await
}

/// Delete a file by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<u64, DbErr> {
    let result = PackageFile::delete_by_id(id).exec(db).await?;
    Ok(result.rows_affected)
}

/// Delete all files for a version.
pub async fn delete_by_version(db: &DatabaseConnection, version_id: i64) -> Result<u64, DbErr> {
    let result = PackageFile::delete_many()
        .filter(package_file::Column::VersionId.eq(version_id))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}
