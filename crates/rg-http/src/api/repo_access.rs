//! Shared repository-scoped authorization helpers for REST handlers.

use axum::http::HeaderMap;

use crate::error::AppError;
use crate::AppState;

/// Resolve a repository from its route owner/name pair.
pub(crate) async fn resolve_repo(
    state: &AppState,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name)
        .await
        // A DB outage here must classify as 503 (retryable), not 500: route the
        // anyhow error through `AppError::from` (which downcasts to `DbErr` and
        // maps connection-level failures to `ServiceUnavailable`) rather than
        // the blanket `AppError::internal`. This is the first DB call of nearly
        // every repository route, so a 500 here would mask the outage.
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("repository not found"))
}

/// Decide read access for an already-resolved repository.
///
/// Public repos stay anonymously readable; a private repo answers `401` to an
/// anonymous caller (a token would help) and `403` to an authenticated
/// outsider (a token would not). A failed check is neither — it propagates as
/// the underlying error instead of being collapsed into a denial.
pub(crate) async fn check_read(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
) -> Result<(), AppError> {
    check_read_inner(state, headers, repo, None).await
}

/// Same as [`check_read`], but a CI job token carrying `ci_scope` for this very
/// repository also passes the gate — used by the endpoints a running job talks
/// to with its job token instead of a user session.
pub(crate) async fn check_read_with_ci(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
    ci_scope: &str,
) -> Result<(), AppError> {
    check_read_inner(state, headers, repo, Some(ci_scope)).await
}

async fn check_read_inner(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
    ci_scope: Option<&str>,
) -> Result<(), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret);

    match rg_core::repo::service::can_read_repo(&state.db, repo, actor_id).await {
        Ok(true) => Ok(()),
        Ok(false) if actor_id.is_none() && ci_job_grants(state, headers, repo, ci_scope) => Ok(()),
        Ok(false) if repo.is_private && actor_id.is_none() => {
            Err(AppError::unauthorized("authentication required"))
        }
        Ok(false) => Err(AppError::forbidden("access denied")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Whether the request carries a CI job token scoped to this repository.
fn ci_job_grants(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
    ci_scope: Option<&str>,
) -> bool {
    match ci_scope {
        Some(scope) => {
            super::auth::extract_ci_job_claims(headers, &state.jwt_secret, repo.id, scope).is_some()
        }
        None => false,
    }
}

/// Require repository read access, while retaining anonymous access to public repos.
pub(crate) async fn require_read(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    let repo = resolve_repo(state, owner, name).await?;
    check_read(state, headers, &repo).await?;
    Ok(repo)
}

/// [`require_read`] with the CI job-token fallback of [`check_read_with_ci`].
pub(crate) async fn require_read_with_ci(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    ci_scope: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    let repo = resolve_repo(state, owner, name).await?;
    check_read_with_ci(state, headers, &repo, ci_scope).await?;
    Ok(repo)
}

/// Require an authenticated user who can read the repository.
pub(crate) async fn require_authenticated_read(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;

    match rg_core::repo::service::can_read_repo(&state.db, &repo, Some(actor_id)).await {
        Ok(true) => Ok((repo, actor_id)),
        Ok(false) => Err(AppError::forbidden("access denied")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Require an authenticated user with repository write access.
pub(crate) async fn require_write(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;

    match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(actor_id)).await {
        Ok(true) => Ok((repo, actor_id)),
        Ok(false) => Err(AppError::forbidden("write access denied")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Require an authenticated repository administrator.
pub(crate) async fn require_admin(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;
    match rg_core::repo::service::can_admin_repo(&state.db, &repo, Some(actor_id)).await {
        Ok(true) => Ok((repo, actor_id)),
        Ok(false) => Err(AppError::forbidden("repository admin access required")),
        Err(error) => Err(AppError::from(error)),
    }
}
