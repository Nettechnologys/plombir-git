use crate::entities::{package_registry, package_registry::Entity as PackageRegistry};
use sea_orm::*;

/// Create a package registry entry for a repo and package type.
pub async fn create(
    db: &DatabaseConnection,
    repo_id: i64,
    package_type: &str,
) -> Result<package_registry::Model, DbErr> {
    use package_registry::ActiveModel;
    let now = chrono::Utc::now();

    let registry = ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        package_type: Set(package_type.to_string()),
        enabled: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };

    registry.insert(db).await
}

/// Ensure a package registry entry exists (get-or-create).
pub async fn find_or_create(
    db: &DatabaseConnection,
    repo_id: i64,
    package_type: &str,
) -> Result<package_registry::Model, DbErr> {
    if let Some(r) = find_by_repo_and_type(db, repo_id, package_type).await? {
        Ok(r)
    } else {
        create(db, repo_id, package_type).await
    }
}

/// Find a package registry by repo and package type.
pub async fn find_by_repo_and_type(
    db: &DatabaseConnection,
    repo_id: i64,
    package_type: &str,
) -> Result<Option<package_registry::Model>, DbErr> {
    PackageRegistry::find()
        .filter(package_registry::Column::RepoId.eq(repo_id))
        .filter(package_registry::Column::PackageType.eq(package_type))
        .one(db)
        .await
}

/// List all package registries for a repo.
pub async fn list_by_repo(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Vec<package_registry::Model>, DbErr> {
    PackageRegistry::find()
        .filter(package_registry::Column::RepoId.eq(repo_id))
        .all(db)
        .await
}

/// Delete a package registry by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<u64, DbErr> {
    let result = PackageRegistry::delete_by_id(id).exec(db).await?;
    Ok(result.rows_affected)
}
