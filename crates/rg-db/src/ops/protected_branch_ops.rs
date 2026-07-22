//! Database operations for protected branches.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::protected_branch::{
    self, ActiveModel, Entity as PbEntity, Model as ProtectedBranch,
};

/// Find a protected branch rule by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<ProtectedBranch>> {
    PbEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find protected branch by id")
}

/// Find a protected branch rule by repo and branch name.
pub async fn find_by_repo_and_branch(
    db: &DatabaseConnection,
    repo_id: i64,
    branch_name: &str,
) -> Result<Option<ProtectedBranch>> {
    PbEntity::find()
        .filter(protected_branch::Column::RepoId.eq(repo_id))
        .filter(protected_branch::Column::BranchName.eq(branch_name))
        .one(db)
        .await
        .context("db: find protected branch by repo and branch")
}

/// List all protected branch rules for a repo.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<ProtectedBranch>> {
    PbEntity::find()
        .filter(protected_branch::Column::RepoId.eq(repo_id))
        .order_by_asc(protected_branch::Column::BranchName)
        .all(db)
        .await
        .context("db: list protected branches by repo")
}

/// Check if a branch is protected.
pub async fn is_protected(
    db: &DatabaseConnection,
    repo_id: i64,
    branch_name: &str,
) -> Result<bool> {
    let found = find_by_repo_and_branch(db, repo_id, branch_name).await?;
    Ok(found.is_some())
}

/// Create a new protected branch rule.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<ProtectedBranch> {
    model
        .insert(db)
        .await
        .context("db: create protected branch")
}

/// Update a protected branch rule.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<ProtectedBranch> {
    model
        .update(db)
        .await
        .context("db: update protected branch")
}

/// Delete a protected branch rule by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    PbEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete protected branch")?;
    Ok(())
}
