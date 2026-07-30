//! Collaborator service — repo collaborators + permission management.

use anyhow::Result;
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

    // Validate permission. Typed like its sibling in `update_permission`: only
    // these two outcomes are the caller's fault, and only they may answer 400 —
    // the `resolve_repo` above and the insert below are ours.
    match permission.as_str() {
        "read" | "write" | "admin" => {}
        _ => {
            return Err(crate::error::invalid_request(format!(
                "invalid permission: {permission}, must be read/write/admin"
            )))
        }
    }

    // Check if already a collaborator
    if let Some(existing) =
        repo_collaborator_ops::find_by_repo_and_user(db, repo.id, user_id).await?
    {
        return Err(crate::error::invalid_request(format!(
            "user {} is already a collaborator (permission: {})",
            user_id, existing.permission
        )));
    }

    let model = repo_collaborator::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo.id),
        user_id: Set(user_id),
        permission: Set(permission),
        created_at: Set(Utc::now()),
    };

    let created = repo_collaborator_ops::create(db, model).await?;
    crate::repo::service::invalidate_perm_cache_user(db, repo.id, user_id);
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
///
/// `repo_id` is the repository the caller was authorized against. The
/// collaborator id is a global primary key, so scoping the lookup to it is what
/// stops an admin of one repository from rewriting the access list of another —
/// and a row belonging to a different repo has to be indistinguishable from one
/// that does not exist, or the id space itself becomes an existence oracle.
pub async fn update_permission(
    db: &DatabaseConnection,
    repo_id: i64,
    collaborator_id: i64,
    permission: String,
) -> Result<RepoCollaborator> {
    match permission.as_str() {
        "read" | "write" | "admin" => {}
        _ => {
            return Err(crate::error::invalid_request(format!(
                "invalid permission: {permission}, must be read/write/admin"
            )))
        }
    }

    let collab = repo_collaborator_ops::find_by_id(db, collaborator_id)
        .await?
        .filter(|collab| collab.repo_id == repo_id)
        .ok_or_else(|| crate::error::not_found("collaborator"))?;

    let user_id = collab.user_id;
    // `From<Model> for ActiveModel` marks every field `Unchanged`, so mutating
    // the model first and converting afterwards produced an update with no SET
    // clause: the call returned the row and changed nothing. The column has to
    // be `Set` on the ActiveModel itself.
    let mut active: repo_collaborator::ActiveModel = collab.into();
    active.permission = Set(permission);
    let updated = repo_collaborator_ops::update(db, active).await?;
    crate::repo::service::invalidate_perm_cache_user(db, repo_id, user_id);
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
    crate::repo::service::invalidate_perm_cache_user(db, repo.id, user_id);
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
    // `NotFound`, not `.context(...)`: an unknown owner or repository is a 404,
    // and the untyped context made it indistinguishable from a failed lookup —
    // which the handlers then reported as the caller's bad request.
    let user = rg_db::ops::user_ops::find_by_username(db, owner)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))?;
    repo_ops::find_personal_by_owner_and_name(db, user.id, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))
}
