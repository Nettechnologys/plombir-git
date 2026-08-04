use crate::entities::{package_version, package_version::Entity as PackageVersion};
use sea_orm::*;

/// Create a new package version entry.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    db: &impl ConnectionTrait,
    package_id: i64,
    version: &str,
    semver: Option<&str>,
    metadata: Option<&str>,
    size: i64,
    sha256: Option<&str>,
    author_id: Option<i64>,
) -> Result<package_version::Model, DbErr> {
    use package_version::ActiveModel;
    let now = chrono::Utc::now();

    let v = ActiveModel {
        id: sea_orm::NotSet,
        package_id: Set(package_id),
        version: Set(version.to_string()),
        semver: Set(semver.map(|s| s.to_string())),
        metadata: Set(metadata.map(|s| s.to_string())),
        size: Set(size),
        sha256: Set(sha256.map(|s| s.to_string())),
        is_yanked: Set(false),
        download_count: Set(0),
        author_id: Set(author_id),
        created_at: Set(now),
    };

    v.insert(db).await
}

/// Find a version by package and version string.
pub async fn find_by_package_and_version(
    db: &DatabaseConnection,
    package_id: i64,
    version: &str,
) -> Result<Option<package_version::Model>, DbErr> {
    PackageVersion::find()
        .filter(package_version::Column::PackageId.eq(package_id))
        .filter(package_version::Column::Version.eq(version))
        .one(db)
        .await
}

/// Find a version by id.
pub async fn find_by_id(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<package_version::Model>, DbErr> {
    PackageVersion::find_by_id(id).one(db).await
}

/// List all versions for a package, ordered by created_at descending.
pub async fn list_by_package(
    db: &DatabaseConnection,
    package_id: i64,
) -> Result<Vec<package_version::Model>, DbErr> {
    PackageVersion::find()
        .filter(package_version::Column::PackageId.eq(package_id))
        .order_by_desc(package_version::Column::CreatedAt)
        .all(db)
        .await
}

/// Increment download count for a version.
pub async fn increment_download_count(db: &DatabaseConnection, id: i64) -> Result<(), DbErr> {
    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(
            backend,
            "UPDATE package_versions SET download_count = download_count + 1 WHERE id = ?",
        ),
        [Value::from(id)],
    ))
    .await?;
    Ok(())
}

/// Grow a version's recorded size by what a later publish added to it.
///
/// A version is not always uploaded in one request — `mvn deploy` sends the
/// POM, the JAR and the sources separately, and PyPI puts an sdist and a wheel
/// under one version — so `size`, the total of what the version holds, is set
/// at creation and has to keep up with every file added afterwards.
///
/// Written as one statement rather than read-modify-write: two uploads landing
/// on the same version concurrently would otherwise each add their bytes to the
/// same stale total, and one of the two would be lost.
pub async fn add_size(db: &DatabaseConnection, id: i64, delta: i64) -> Result<(), DbErr> {
    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(
            backend,
            "UPDATE package_versions SET size = size + ? WHERE id = ?",
        ),
        [Value::from(delta), Value::from(id)],
    ))
    .await?;
    Ok(())
}

/// Set the yanked status for a version.
pub async fn set_yanked(db: &DatabaseConnection, id: i64, yanked: bool) -> Result<(), DbErr> {
    use package_version::ActiveModel;
    if let Some(v) = find_by_id(db, id).await? {
        let mut am: ActiveModel = v.into();
        am.is_yanked = Set(yanked);
        am.update(db).await?;
    }
    Ok(())
}

/// Delete a version by id.
///
/// Takes any connection so the caller can pair it with the file-row delete
/// inside one transaction — see [`super::package_file_ops::delete_by_version`].
pub async fn delete_by_id(db: &impl ConnectionTrait, id: i64) -> Result<u64, DbErr> {
    let result = PackageVersion::delete_by_id(id).exec(db).await?;
    Ok(result.rows_affected)
}
