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

/// Find a package by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<package::Model>, DbErr> {
    Package::find_by_id(id).one(db).await
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

/// Delete a package by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<u64, DbErr> {
    let result = Package::delete_by_id(id).exec(db).await?;
    Ok(result.rows_affected)
}
