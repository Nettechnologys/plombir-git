use crate::entities::{package_version, package_version::Entity as PackageVersion};
use sea_orm::sea_query::Expr;
use sea_orm::*;

/// Create a new package version entry.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    db: &impl ConnectionTrait,
    package_id: i64,
    version: &str,
    protocol_version_key: Option<&str>,
    semver: Option<&str>,
    metadata: Option<&str>,
    size: i64,
    sha256: Option<&str>,
    author_id: Option<i64>,
) -> Result<package_version::Model, DbErr> {
    create_with_protocol_identity(
        db,
        package_id,
        version,
        protocol_version_key,
        "",
        semver,
        metadata,
        size,
        sha256,
        author_id,
    )
    .await
}

/// Create a version whose raw spelling is scoped by a protocol variant.
///
/// The variant is empty for ordinary package protocols and is the platform for
/// RubyGems, where `1.0.0-ruby` and `1.0.0-java` are distinct releases even
/// though both rows retain the publisher's raw version number `1.0.0`.
#[allow(clippy::too_many_arguments)]
pub async fn create_with_protocol_identity(
    db: &impl ConnectionTrait,
    package_id: i64,
    version: &str,
    protocol_version_key: Option<&str>,
    protocol_variant_key: &str,
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
        protocol_version_key: Set(protocol_version_key.map(str::to_string)),
        protocol_variant_key: Set(protocol_variant_key.to_string()),
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

/// Find a version through its protocol-defined identity key.
pub async fn find_by_package_and_protocol_version_key(
    db: &impl ConnectionTrait,
    package_id: i64,
    protocol_version_key: &str,
) -> Result<Option<package_version::Model>, DbErr> {
    PackageVersion::find()
        .filter(package_version::Column::PackageId.eq(package_id))
        .filter(package_version::Column::ProtocolVersionKey.eq(protocol_version_key))
        .one(db)
        .await
}

/// Find a version by package and version string.
pub async fn find_by_package_and_version(
    db: &impl ConnectionTrait,
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

/// List all versions for a package, newest publication first.
///
/// `created_at` is not unique (bulk imports and coarse upstream timestamps can
/// tie), so the primary key completes the order and keeps every consumer's
/// fallback deterministic.
pub async fn list_by_package(
    db: &impl ConnectionTrait,
    package_id: i64,
) -> Result<Vec<package_version::Model>, DbErr> {
    PackageVersion::find()
        .filter(package_version::Column::PackageId.eq(package_id))
        .order_by_desc(package_version::Column::CreatedAt)
        .order_by_desc(package_version::Column::Id)
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
pub async fn add_size(db: &impl ConnectionTrait, id: i64, delta: i64) -> Result<(), DbErr> {
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
///
/// The caller resolves the package version in a separate statement, so a
/// concurrent DELETE can win before this write. `None` reports that ordinary
/// absence without leaking SeaORM's backend-shaped `RecordNotUpdated` error.
pub async fn set_yanked(
    db: &DatabaseConnection,
    id: i64,
    yanked: bool,
) -> Result<Option<package_version::Model>, DbErr> {
    let result = PackageVersion::update_many()
        .col_expr(package_version::Column::IsYanked, Expr::value(yanked))
        .filter(package_version::Column::Id.eq(id))
        .exec(db)
        .await?;
    match result.rows_affected {
        // MySQL may report zero rows for a no-op UPDATE. Re-read the stable
        // identity to distinguish that from a winning DELETE on every backend.
        0 | 1 => find_by_id(db, id).await,
        rows => Err(DbErr::Custom(format!(
            "package version yank update affected {rows} rows for id {id}"
        ))),
    }
}

/// Delete a version by id.
///
/// Takes any connection so the caller can pair it with the file-row delete
/// inside one transaction — see [`super::package_file_ops::delete_by_version`].
pub async fn delete_by_id(db: &impl ConnectionTrait, id: i64) -> Result<u64, DbErr> {
    let result = PackageVersion::delete_by_id(id).exec(db).await?;
    Ok(result.rows_affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn equal_creation_times_are_ordered_by_descending_id() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE package_versions (\
                 id INTEGER PRIMARY KEY, package_id BIGINT NOT NULL, version TEXT NOT NULL, \
                 protocol_version_key TEXT, protocol_variant_key TEXT NOT NULL DEFAULT '', \
                 semver TEXT, metadata TEXT, size BIGINT NOT NULL, sha256 TEXT, \
                 is_yanked BOOLEAN NOT NULL, download_count BIGINT NOT NULL, \
                 author_id BIGINT, created_at TIMESTAMP NOT NULL\
             );\
             INSERT INTO package_versions \
                 (id, package_id, version, semver, metadata, size, sha256, is_yanked, \
                  download_count, author_id, created_at) VALUES \
                 (11, 7, '1.0.0', '1.0.0', NULL, 0, NULL, 0, 0, NULL, '2026-08-09T00:00:00Z'), \
                 (13, 7, '1.2.0', '1.2.0', NULL, 0, NULL, 0, 0, NULL, '2026-08-09T00:00:00Z'), \
                 (12, 7, '1.1.0', '1.1.0', NULL, 0, NULL, 0, 0, NULL, '2026-08-09T00:00:00Z');",
        )
        .await
        .unwrap();

        for _ in 0..3 {
            let ids = list_by_package(&db, 7)
                .await
                .unwrap()
                .into_iter()
                .map(|version| version.id)
                .collect::<Vec<_>>();
            assert_eq!(ids, [13, 12, 11]);
        }
    }
}
