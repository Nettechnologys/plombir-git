use crate::entities::{package, package::Entity as Package};
use sea_orm::*;

/// Create a new package entry.
pub async fn create(
    db: &DatabaseConnection,
    registry_id: i64,
    owner_id: i64,
    name: &str,
    description: Option<&str>,
    homepage: Option<&str>,
    repository_url: Option<&str>,
) -> Result<package::Model, DbErr> {
    use package::ActiveModel;
    let now = chrono::Utc::now();

    let pkg = ActiveModel {
        id: sea_orm::NotSet,
        package_registry_id: Set(registry_id),
        owner_id: Set(owner_id),
        name: Set(name.to_string()),
        description: Set(description.map(|s| s.to_string())),
        homepage: Set(homepage.map(|s| s.to_string())),
        repository_url: Set(repository_url.map(|s| s.to_string())),
        is_public: Set(true),
        download_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
    };

    pkg.insert(db).await
}

/// Ensure a package row exists for `(registry_id, name)` (get-or-create).
///
/// Same shape, and same reason, as `package_registry_ops::find_or_create`: the
/// lookup and the insert are separate statements, and `idx_package_registry_name`
/// is UNIQUE on `(package_registry_id, name)`. Two CI jobs publishing *different
/// versions of the same new package* — the ordinary case, not an exotic one —
/// both read `None` and both insert; the loser would get a 5xx on a request
/// that contradicts nothing.
///
/// The loser adopts the winner's row. The descriptive columns are deliberately
/// left as the winner wrote them: the existing-row branch does not rewrite them
/// either, so a package's description belongs to whoever published it first and
/// a race does not change that. The version row written afterwards is where a
/// genuine conflict lives, and that one is still reported (409).
pub async fn find_or_create(
    db: &DatabaseConnection,
    registry_id: i64,
    owner_id: i64,
    name: &str,
    description: Option<&str>,
    homepage: Option<&str>,
    repository_url: Option<&str>,
) -> Result<package::Model, DbErr> {
    if let Some(existing) = find_by_registry_and_name(db, registry_id, name).await? {
        return Ok(existing);
    }

    match create(
        db,
        registry_id,
        owner_id,
        name,
        description,
        homepage,
        repository_url,
    )
    .await
    {
        Ok(created) => Ok(created),
        Err(error) if crate::is_unique_violation(&error) => {
            match find_by_registry_and_name(db, registry_id, name).await? {
                Some(winner) => Ok(winner),
                // Not there after all, so the collision was on some other
                // constraint. Report the original failure.
                None => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// Find a package by registry and name.
pub async fn find_by_registry_and_name(
    db: &DatabaseConnection,
    registry_id: i64,
    name: &str,
) -> Result<Option<package::Model>, DbErr> {
    Package::find()
        .filter(package::Column::PackageRegistryId.eq(registry_id))
        .filter(package::Column::Name.eq(name))
        .one(db)
        .await
}

/// Refresh the package-level metadata carried by a newly published version.
///
/// Missing fields deliberately stay untouched: package manifests commonly omit
/// optional metadata, and publishing one of those versions must not erase the
/// last value the package did provide.
pub async fn update_metadata(
    db: &DatabaseConnection,
    id: i64,
    description: Option<&str>,
    homepage: Option<&str>,
    repository_url: Option<&str>,
) -> Result<(), DbErr> {
    if description.is_none() && homepage.is_none() && repository_url.is_none() {
        return Ok(());
    }

    let mut package = package::ActiveModel {
        id: Unchanged(id),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    if let Some(description) = description {
        package.description = Set(Some(description.to_string()));
    }
    if let Some(homepage) = homepage {
        package.homepage = Set(Some(homepage.to_string()));
    }
    if let Some(repository_url) = repository_url {
        package.repository_url = Set(Some(repository_url.to_string()));
    }

    package.update(db).await?;
    Ok(())
}

/// List all packages in a registry.
pub async fn list_by_registry(
    db: &DatabaseConnection,
    registry_id: i64,
) -> Result<Vec<package::Model>, DbErr> {
    Package::find()
        .filter(package::Column::PackageRegistryId.eq(registry_id))
        .order_by_asc(package::Column::Name)
        .all(db)
        .await
}

/// Increment download count.
pub async fn increment_download_count(db: &DatabaseConnection, id: i64) -> Result<(), DbErr> {
    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(
            backend,
            "UPDATE packages SET download_count = download_count + 1 WHERE id = ?",
        ),
        [Value::from(id)],
    ))
    .await?;
    Ok(())
}
