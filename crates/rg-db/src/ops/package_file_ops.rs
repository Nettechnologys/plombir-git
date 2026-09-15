use crate::entities::{
    package, package_file, package_file::Entity as PackageFile, package_registry, package_version,
};
use anyhow::Context;
use sea_orm::*;

/// The digests of a stored file, as the package protocols ask for them.
///
/// Each field is lowercase hex, or `None` when the caller does not have that
/// digest — a metadata route then omits the field instead of publishing
/// another algorithm's value under its name.
#[derive(Debug, Default, Clone, Copy)]
pub struct FileDigests<'a> {
    pub sha256: Option<&'a str>,
    pub sha1: Option<&'a str>,
    pub sha512: Option<&'a str>,
}

/// Create a new package file entry.
pub async fn create(
    db: &impl ConnectionTrait,
    version_id: i64,
    filename: &str,
    size: i64,
    digests: FileDigests<'_>,
    storage_path: &str,
) -> Result<package_file::Model, DbErr> {
    use package_file::ActiveModel;
    let now = chrono::Utc::now();

    let f = ActiveModel {
        id: sea_orm::NotSet,
        version_id: Set(version_id),
        filename: Set(filename.to_string()),
        size: Set(size),
        sha256: Set(digests.sha256.map(|s| s.to_string())),
        sha1: Set(digests.sha1.map(|s| s.to_string())),
        sha512: Set(digests.sha512.map(|s| s.to_string())),
        storage_path: Set(storage_path.to_string()),
        created_at: Set(now),
    };

    f.insert(db).await
}

/// Check whether durable metadata owns an exact package blob key.
///
/// Startup recovery is the only caller. Package publication writes a
/// request-private UUID key before committing its file row, so recovery must
/// prove that exact row absent before it may delete an interrupted publication.
pub async fn exists_by_storage_path(
    db: &DatabaseConnection,
    storage_path: &str,
) -> anyhow::Result<bool> {
    Ok(PackageFile::find()
        .select_only()
        .column(package_file::Column::Id)
        .filter(package_file::Column::StoragePath.eq(storage_path))
        .into_tuple::<i64>()
        .one(db)
        .await
        .context("db: find package file by storage path")?
        .is_some())
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

/// Every package-file storage path owned by `repo_id`, walked
/// repo → package_registry → packages → package_versions → package_files.
///
/// Repository deletion needs this because rows written before the blob-storage
/// migration hold an absolute filesystem path instead of a key: those bytes sit
/// outside the `packages/<owner>/<repo>` prefix the deletion moves, so the only
/// way from a repository to them is through the database. Ownership is read out
/// of the rows rather than guessed from the shape of the path — the caller
/// decides which of the returned paths are pre-migration ones.
///
/// Returns bare paths through four id-only statements for the same reason
/// [`crate::ops::pipeline_ops::list_job_ids_by_repo`] does: a repository with
/// many published versions would otherwise cost one query per version, and the
/// caller has no use for the rows themselves.
pub async fn list_storage_paths_by_repo(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Vec<String>, DbErr> {
    let registry_ids: Vec<i64> = package_registry::Entity::find()
        .select_only()
        .column(package_registry::Column::Id)
        .filter(package_registry::Column::RepoId.eq(repo_id))
        .into_tuple()
        .all(db)
        .await?;
    if registry_ids.is_empty() {
        return Ok(Vec::new());
    }

    let package_ids: Vec<i64> = package::Entity::find()
        .select_only()
        .column(package::Column::Id)
        .filter(package::Column::PackageRegistryId.is_in(registry_ids))
        .into_tuple()
        .all(db)
        .await?;
    if package_ids.is_empty() {
        return Ok(Vec::new());
    }

    let version_ids: Vec<i64> = package_version::Entity::find()
        .select_only()
        .column(package_version::Column::Id)
        .filter(package_version::Column::PackageId.is_in(package_ids))
        .into_tuple()
        .all(db)
        .await?;
    if version_ids.is_empty() {
        return Ok(Vec::new());
    }

    PackageFile::find()
        .select_only()
        .column(package_file::Column::StoragePath)
        .filter(package_file::Column::VersionId.is_in(version_ids))
        .into_tuple()
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

/// Delete all files for a version.
///
/// Takes any connection, not just the pool: a version delete has to remove the
/// file rows and the version row as one statement pair, or a failure between
/// them leaves a live version whose files are gone.
pub async fn delete_by_version(db: &impl ConnectionTrait, version_id: i64) -> Result<u64, DbErr> {
    let result = PackageFile::delete_many()
        .filter(package_file::Column::VersionId.eq(version_id))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}
