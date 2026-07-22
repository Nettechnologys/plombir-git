//! Database operations for repository collaborators.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::repo_collaborator::{
    self, ActiveModel, Entity as CollabEntity, Model as RepoCollaborator,
};

/// Find a collaborator by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<RepoCollaborator>> {
    CollabEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find collaborator by id")
}

/// Find a collaborator by repo and user.
pub async fn find_by_repo_and_user(
    db: &DatabaseConnection,
    repo_id: i64,
    user_id: i64,
) -> Result<Option<RepoCollaborator>> {
    CollabEntity::find()
        .filter(repo_collaborator::Column::RepoId.eq(repo_id))
        .filter(repo_collaborator::Column::UserId.eq(user_id))
        .one(db)
        .await
        .context("db: find collaborator by repo and user")
}

/// List all collaborators for a repo.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<RepoCollaborator>> {
    CollabEntity::find()
        .filter(repo_collaborator::Column::RepoId.eq(repo_id))
        .all(db)
        .await
        .context("db: list collaborators by repo")
}

/// Get the effective permission of a user on a repo.
/// Returns: "admin" | "write" | "read" | None
pub async fn get_permission(
    db: &DatabaseConnection,
    repo_id: i64,
    user_id: i64,
) -> Result<Option<String>> {
    let collab = find_by_repo_and_user(db, repo_id, user_id).await?;
    Ok(collab.map(|c| c.permission))
}

/// Create a new collaborator.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<RepoCollaborator> {
    model.insert(db).await.context("db: create collaborator")
}

/// Update a collaborator's permission.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<RepoCollaborator> {
    model.update(db).await.context("db: update collaborator")
}

/// Remove a collaborator by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    CollabEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete collaborator")?;
    Ok(())
}

/// Remove a collaborator by repo and user.
pub async fn delete_by_repo_and_user(
    db: &DatabaseConnection,
    repo_id: i64,
    user_id: i64,
) -> Result<()> {
    use sea_orm::QueryFilter;
    CollabEntity::delete_many()
        .filter(repo_collaborator::Column::RepoId.eq(repo_id))
        .filter(repo_collaborator::Column::UserId.eq(user_id))
        .exec(db)
        .await
        .context("db: delete collaborator by repo and user")?;
    Ok(())
}
