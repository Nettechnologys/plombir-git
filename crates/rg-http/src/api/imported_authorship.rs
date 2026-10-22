//! Taking back imported history you wrote (card_2a006cf365f6).
//!
//! `GET` lists, per repository, what an import brought that was written by a
//! GitHub or GitLab account this account has signed in with; `POST …/claim`
//! moves the repositories the person confirmed. Nothing moves on sign-in by
//! itself: the owner's decision was that a change of author needs the person
//! to see it and ask for it. The rules about which ids can be trusted live in
//! [`rg_core::import::reattribution`].

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::repo_access;
use crate::api::auth::AuthUser;
use crate::error::AppError;
use crate::AppState;
use rg_core::import::reattribution::{self, ClaimOutcome, ClaimableRepository};

/// What could be taken back in one repository.
#[derive(Serialize, ToSchema)]
pub struct ClaimableRepositoryResponse {
    /// Repository id — what `POST …/claim` takes.
    pub repo_id: i64,
    /// `owner/name`.
    pub repository: String,
    /// `github` (github.com) or `gitlab` (gitlab.com).
    pub service: String,
    /// The login on that service the sign-in vouched for.
    pub login: String,
    pub issues: u64,
    pub comments: u64,
    pub pull_requests: u64,
    pub reviews: u64,
    pub releases: u64,
}

#[derive(Deserialize, ToSchema)]
pub struct ClaimRequest {
    /// The repositories the person confirmed, by `repo_id` from the listing.
    pub repositories: Vec<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ClaimResponse {
    pub issues: u64,
    pub comments: u64,
    pub pull_requests: u64,
    pub reviews: u64,
    pub releases: u64,
}

impl From<ClaimOutcome> for ClaimResponse {
    fn from(outcome: ClaimOutcome) -> Self {
        Self {
            issues: outcome.issues,
            comments: outcome.comments,
            pull_requests: outcome.pull_requests,
            reviews: outcome.reviews,
            releases: outcome.releases,
        }
    }
}

/// The rows the caller may be shown: a repository they cannot read stays out,
/// so a count does not reveal that it exists.
async fn readable(
    state: &AppState,
    user_id: i64,
    rows: Vec<ClaimableRepository>,
) -> Result<Vec<(ClaimableRepository, String)>, AppError> {
    let mut readable = Vec::new();
    for row in rows {
        let Some(repo) = rg_db::ops::repo_ops::find_by_id(&state.db, row.repo_id)
            .await
            .map_err(AppError::from)?
        else {
            continue;
        };
        if !repo_access::may_read(state, &repo, Some(user_id)).await? {
            continue;
        }
        let owner = rg_core::repo::service::repository_namespace_name(
            &state.db,
            repo.owner_id,
            repo.org_id,
        )
        .await
        .map_err(AppError::from)?;
        readable.push((row, format!("{owner}/{}", repo.name)));
    }
    Ok(readable)
}

/// GET /users/me/imported-authorship
#[utoipa::path(
    get,
    path = "/users/me/imported-authorship",
    tag = "Imports",
    responses(
        (status = 200, description = "Imported history written by a source account this account signs in with, per repository it can read", body = Vec<ClaimableRepositoryResponse>),
        (status = 401, description = "Authentication required"),
    ),
)]
pub async fn list_claimable(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<ClaimableRepositoryResponse>>, AppError> {
    let identities = reattribution::signed_in_identities(&state.db, user_id)
        .await
        .map_err(AppError::from)?;
    let rows = reattribution::claimable(&state.db, user_id, &identities)
        .await
        .map_err(AppError::from)?;
    let rows = readable(&state, user_id, rows).await?;
    Ok(Json(
        rows.into_iter()
            .filter(|(row, _)| row.total() > 0)
            .map(|(row, repository)| ClaimableRepositoryResponse {
                login: identities
                    .iter()
                    .find(|identity| {
                        identity.service == row.service && identity.account_id == row.account_id
                    })
                    .map(|identity| identity.login.clone())
                    .unwrap_or_default(),
                repo_id: row.repo_id,
                repository,
                service: row.service.to_string(),
                issues: row.issues,
                comments: row.comments,
                pull_requests: row.pull_requests,
                reviews: row.reviews,
                releases: row.releases,
            })
            .collect(),
    ))
}

/// POST /users/me/imported-authorship/claim
#[utoipa::path(
    post,
    path = "/users/me/imported-authorship/claim",
    tag = "Imports",
    request_body = ClaimRequest,
    responses(
        (status = 200, description = "How many rows became the caller's, by kind", body = ClaimResponse),
        (status = 400, description = "No repository named"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "A named repository does not exist or cannot be read"),
    ),
)]
pub async fn claim(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Json(body): Json<ClaimRequest>,
) -> Result<Json<ClaimResponse>, AppError> {
    if body.repositories.is_empty() {
        return Err(AppError::bad_request(
            "name at least one repository to take your imported history back in",
        ));
    }
    let mut repo_ids = body.repositories;
    repo_ids.sort_unstable();
    repo_ids.dedup();
    // The same gate as the listing, asked again: the confirmation is the
    // person's, the repositories are whatever the request names. One they
    // cannot read is answered as one that does not exist.
    for repo_id in &repo_ids {
        let readable = match rg_db::ops::repo_ops::find_by_id(&state.db, *repo_id)
            .await
            .map_err(AppError::from)?
        {
            Some(repo) => repo_access::may_read(&state, &repo, Some(user_id)).await?,
            None => false,
        };
        if !readable {
            return Err(AppError::not_found("repository not found"));
        }
    }
    let identities = reattribution::signed_in_identities(&state.db, user_id)
        .await
        .map_err(AppError::from)?;
    let outcome = reattribution::claim(&state.db, user_id, &identities, &repo_ids)
        .await
        .map_err(AppError::from)?;

    let actor = rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, user_id).await;
    rg_core::audit::record(
        &state.db,
        &actor,
        "import.authorship_claimed",
        Some("user"),
        Some(user_id),
        actor.name(),
        Some(&headers),
        Some(serde_json::json!({
            "repositories": repo_ids,
            "identities": identities
                .iter()
                .map(|identity| serde_json::json!({
                    "service": identity.service,
                    "account_id": identity.account_id,
                }))
                .collect::<Vec<_>>(),
            "moved": &outcome,
        })),
    )
    .await;

    Ok(Json(outcome.into()))
}
