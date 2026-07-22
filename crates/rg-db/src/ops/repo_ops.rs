//! Database operations for repositories.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{ActiveValue::Set, *};

use crate::entities::repository::{
    self, ActiveModel as RepoActiveModel, Entity as RepoEntity, Model as Repo,
};

/// Find a non-deleted repository by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Repo>> {
    RepoEntity::find_by_id(id)
        .filter(repository::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find repo by id")
}

/// Find a repo by (owner_id, name). Excludes soft-deleted repos.
pub async fn find_by_owner_and_name(
    db: &DatabaseConnection,
    owner_id: i64,
    name: &str,
) -> Result<Option<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OwnerId.eq(owner_id))
        .filter(repository::Column::Name.eq(name))
        .filter(repository::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find repo by owner and name")
}

/// List all non-deleted repos owned by a user.
pub async fn list_by_owner(db: &DatabaseConnection, owner_id: i64) -> Result<Vec<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OwnerId.eq(owner_id))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::Name)
        .all(db)
        .await
        .context("db: list repos by owner")
}

/// Paginated list of non-deleted repos owned by a user.
pub async fn list_by_owner_paginated(
    db: &DatabaseConnection,
    owner_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::OwnerId.eq(owner_id))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::Name);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count repos by owner")? as i64;
    let repos = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list repos by owner (paginated)")?;

    Ok((repos, total))
}

/// Find a repo by (org_id, name). Excludes soft-deleted repos.
pub async fn find_by_org_and_name(
    db: &DatabaseConnection,
    org_id: i64,
    name: &str,
) -> Result<Option<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OrgId.eq(org_id))
        .filter(repository::Column::Name.eq(name))
        .filter(repository::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find repo by org and name")
}

/// List all non-deleted repos belonging to an organization.
pub async fn list_by_org(db: &DatabaseConnection, org_id: i64) -> Result<Vec<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OrgId.eq(org_id))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::Name)
        .all(db)
        .await
        .context("db: list repos by org")
}

/// Paginated list of non-deleted repos belonging to an organization.
pub async fn list_by_org_paginated(
    db: &DatabaseConnection,
    org_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::OrgId.eq(org_id))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::Name);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count repos by org")? as i64;
    let repos = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list repos by org (paginated)")?;

    Ok((repos, total))
}

/// Paginated list of public, non-deleted repos — ordered by recently updated.
pub async fn list_public_paginated(
    db: &DatabaseConnection,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::IsPrivate.eq(false))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_desc(repository::Column::UpdatedAt);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count public repos")? as i64;
    let repos = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list public repos (paginated)")?;

    Ok((repos, total))
}

/// Create a new repo.
pub async fn create(db: &DatabaseConnection, model: RepoActiveModel) -> Result<Repo> {
    model.insert(db).await.context("db: create repo")
}

/// Delete a repo by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    RepoEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete repo")?;
    Ok(())
}

/// Soft-delete a repository (set deleted_at timestamp).
pub async fn soft_delete(db: &DatabaseConnection, id: i64) -> Result<()> {
    let repo = RepoEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find repo for soft delete")?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;

    let mut model: RepoActiveModel = repo.into();
    model.deleted_at = Set(Some(Utc::now()));
    model.update(db).await.context("db: soft delete repo")?;

    Ok(())
}

/// Update stars_count for a repository based on actual star count (atomic).
pub async fn update_stars_count(db: &DatabaseConnection, id: i64) -> Result<()> {
    let count = crate::ops::repo_star_ops::count_by_repo(db, id).await?;
    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(
            backend,
            "UPDATE repositories SET stars_count = ? WHERE id = ?",
        ),
        [count.into(), id.into()],
    ))
    .await
    .context("db: update stars count")?;
    Ok(())
}

/// Update forks_count for a repository based on actual fork count (atomic).
pub async fn update_forks_count(db: &DatabaseConnection, id: i64) -> Result<()> {
    use crate::entities::repository::Column;
    let count = RepoEntity::find()
        .filter(Column::OriginRepoId.eq(Some(id)))
        .filter(Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: count forks")? as i64;
    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(
            backend,
            "UPDATE repositories SET forks_count = ? WHERE id = ?",
        ),
        [count.into(), id.into()],
    ))
    .await
    .context("db: update forks count")?;
    Ok(())
}

/// List all forks of a repo.
pub async fn list_forks(
    db: &DatabaseConnection,
    origin_repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::OriginRepoId.eq(Some(origin_repo_id)))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::CreatedAt);
    let total = base.clone().count(db).await.context("db: count forks")? as i64;
    let repos = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list forks")?;
    Ok((repos, total))
}

/// Update repo owner (for transfer).
pub async fn update_owner(
    db: &DatabaseConnection,
    repo_id: i64,
    owner_id: i64,
    org_id: Option<i64>,
) -> Result<()> {
    let repo = RepoEntity::find_by_id(repo_id)
        .one(db)
        .await?
        .context("repo not found")?;
    let mut active: RepoActiveModel = repo.into();
    active.owner_id = Set(owner_id);
    active.org_id = Set(org_id);
    active.updated_at = Set(Utc::now());
    active.update(db).await.context("db: update repo owner")?;
    Ok(())
}
