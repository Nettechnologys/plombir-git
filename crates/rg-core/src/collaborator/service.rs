//! Collaborator service — repo collaborators + permission management.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{DatabaseConnection, Set};

use rg_db::entities::repo_collaborator::{self, Model as RepoCollaborator};
use rg_db::ops::{repo_collaborator_ops, repo_ops};

/// Add a collaborator to a repo.
pub async fn add_collaborator(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    user_id: i64,
    permission: String,
) -> Result<RepoCollaborator> {
    let repo = resolve_repo(db, owner, repo_name).await?;

    // Validate permission
    match permission.as_str() {
        "read" | "write" | "admin" => {}
        _ => bail!(
            "invalid permission: {}, must be read/write/admin",
            permission
        ),
    }

    // Check if already a collaborator
    if let Some(existing) =
        repo_collaborator_ops::find_by_repo_and_user(db, repo.id, user_id).await?
    {
        bail!(
            "user {} is already a collaborator (permission: {})",
            user_id,
            existing.permission
        );
    }

    let model = repo_collaborator::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo.id),
        user_id: Set(user_id),
        permission: Set(permission),
        created_at: Set(Utc::now()),
    };

    let created = repo_collaborator_ops::create(db, model).await?;
    crate::repo::service::invalidate_perm_cache_user(repo.id, user_id);
    Ok(created)
}

/// List all collaborators for a repo.
pub async fn list_collaborators(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<Vec<RepoCollaborator>> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    repo_collaborator_ops::list_by_repo(db, repo.id).await
}

/// Update a collaborator's permission.
pub async fn update_permission(
    db: &DatabaseConnection,
    collaborator_id: i64,
    permission: String,
) -> Result<RepoCollaborator> {
    match permission.as_str() {
        "read" | "write" | "admin" => {}
        _ => bail!("invalid permission: {}", permission),
    }

    let mut collab = repo_collaborator_ops::find_by_id(db, collaborator_id)
        .await?
        .context("collaborator not found")?;

    let (repo_id, user_id) = (collab.repo_id, collab.user_id);
    collab.permission = permission;
    let active: repo_collaborator::ActiveModel = collab.into();
    let updated = repo_collaborator_ops::update(db, active).await?;
    crate::repo::service::invalidate_perm_cache_user(repo_id, user_id);
    Ok(updated)
}

/// Remove a collaborator from a repo.
pub async fn remove_collaborator(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    user_id: i64,
) -> Result<()> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    repo_collaborator_ops::delete_by_repo_and_user(db, repo.id, user_id).await?;
    crate::repo::service::invalidate_perm_cache_user(repo.id, user_id);
    Ok(())
}

/// Get the effective permission for a user on a repo.
/// Takes into account: repo owner (admin) + collaborator permission.
/// Returns: "admin" | "write" | "read" | None
pub async fn get_effective_permission(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    user_id: i64,
) -> Result<Option<String>> {
    let repo = resolve_repo(db, owner, repo_name).await?;

    // Owner always has admin
    if repo.owner_id == user_id {
        return Ok(Some("admin".to_string()));
    }

    // Check collaborator
    repo_collaborator_ops::get_permission(db, repo.id, user_id).await
}

// ── Helpers ───────────────────────────────────────────────────────────

async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<rg_db::entities::repository::Model> {
    let user = rg_db::ops::user_ops::find_by_username(db, owner)
        .await?
        .context("owner not found")?;
    repo_ops::find_by_owner_and_name(db, user.id, repo_name)
        .await?
        .context("repository not found")
}
