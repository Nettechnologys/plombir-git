//! Database operations for repository collaborators.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
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

/// Which of `user_ids` hold any collaborator grant on `repo_id`.
///
/// The batch form of [`get_permission`]`.is_some()` for a caller that has to
/// ask the same question about a page of people at once — the watch fan-out
/// checks every subscriber before it delivers. One statement for the page
/// instead of one per person; the caller bounds the page.
pub async fn collaborators_among(
    db: &DatabaseConnection,
    repo_id: i64,
    user_ids: &[i64],
) -> Result<Vec<i64>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    CollabEntity::find()
        .select_only()
        .column(repo_collaborator::Column::UserId)
        .filter(repo_collaborator::Column::RepoId.eq(repo_id))
        .filter(repo_collaborator::Column::UserId.is_in(user_ids.iter().copied()))
        .into_tuple()
        .all(db)
        .await
        .context("db: list collaborators among users")
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

/// Result of writing one collaborator permission.
///
/// `changed` is deliberately part of the value: permission-cache invalidation
/// is required after a real write, but an idempotent PATCH must not evict it.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionUpdate {
    pub collaborator: RepoCollaborator,
    pub changed: bool,
}

/// Update a collaborator's permission in one conditional statement.
///
/// The caller's repository-scoping lookup is a separate read. A concurrent
/// delete can therefore remove the row before this statement reaches it;
/// absence is returned as `None`, not SeaORM's `RecordNotUpdated` error.
/// Filtering out an already-equal value also makes `changed` independent of
/// each backend's affected-row convention for no-op updates.
pub async fn update_permission(
    db: &DatabaseConnection,
    id: i64,
    permission: String,
) -> Result<Option<PermissionUpdate>> {
    let result = CollabEntity::update_many()
        .col_expr(
            repo_collaborator::Column::Permission,
            Expr::value(permission.clone()),
        )
        .filter(repo_collaborator::Column::Id.eq(id))
        .filter(repo_collaborator::Column::Permission.ne(permission))
        .exec(db)
        .await
        .context("db: update collaborator")?;

    Ok(find_by_id(db, id)
        .await?
        .map(|collaborator| PermissionUpdate {
            collaborator,
            changed: result.rows_affected == 1,
        }))
}

/// Remove a collaborator by repo and user. Returns whether a row was removed.
///
/// The discarded `rows_affected` here is what let a delete that matched nothing
/// answer `204`: the caller could not tell "the user is no longer a
/// collaborator" from "no such collaborator, nothing happened" — and the path
/// invites exactly that mistake, since `PATCH` on the same URL keys off the
/// `repo_collaborators` row id while this keys off `users.id`.
///
/// "There is no such row" travels in the value rather than as an error for the
/// same reason as [`crate::ops::org_ops::delete_team`]: this crate cannot depend
/// on `rg-core`, so an untyped `anyhow!("…not found")` would be
/// indistinguishable at the HTTP layer from the `.context("db: …")` failure
/// below it. An `Err` from here always means the database itself failed.
pub async fn delete_by_repo_and_user(
    db: &DatabaseConnection,
    repo_id: i64,
    user_id: i64,
) -> Result<bool> {
    use sea_orm::QueryFilter;
    let result = CollabEntity::delete_many()
        .filter(repo_collaborator::Column::RepoId.eq(repo_id))
        .filter(repo_collaborator::Column::UserId.eq(user_id))
        .exec(db)
        .await
        .context("db: delete collaborator by repo and user")?;
    Ok(result.rows_affected > 0)
}
