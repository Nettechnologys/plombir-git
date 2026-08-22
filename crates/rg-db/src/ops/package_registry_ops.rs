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
///
/// The lookup and the insert are separate statements, so two first publishes of
/// the same package type into one repository both see `None` and both insert.
/// `idx_package_registry_repo_type` is UNIQUE on `(repo_id, package_type)`, so
/// the loser gets a constraint error on a request that contradicts nothing —
/// and since this is the first step of `publish`, it takes the whole
/// publication down with it.
///
/// The loser therefore adopts the winner's row and carries on. A registry row
/// has no caller-supplied state to reconcile — it is a marker that this repo
/// serves this package type — so adopting it is the same outcome the calls
/// would have had a millisecond apart. A collision that is *not* this UNIQUE,
/// or one whose row is not there on the re-read, stays the original error.
pub async fn find_or_create(
    db: &DatabaseConnection,
    repo_id: i64,
    package_type: &str,
) -> Result<package_registry::Model, DbErr> {
    if let Some(r) = find_by_repo_and_type(db, repo_id, package_type).await? {
        return Ok(r);
    }

    match create(db, repo_id, package_type).await {
        Ok(created) => Ok(created),
        Err(error) if crate::is_unique_violation(&error) => {
            match find_by_repo_and_type(db, repo_id, package_type).await? {
                Some(winner) => Ok(winner),
                // Not there after all, so the collision was on some other
                // constraint. Report the original failure.
                None => Err(error),
            }
        }
        Err(error) => Err(error),
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
