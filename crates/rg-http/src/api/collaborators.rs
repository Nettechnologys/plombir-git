//! REST API handlers for repository collaborators.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::api::repo_access::{RepoAdmin, RepoRead};
use crate::api::user_ref::UserRef;
use crate::error::AppError;
use crate::AppState;

// ── Request / Response types ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddCollaboratorRequest {
    /// `user_id` / `username` / `email` — see [`UserRef`]. This endpoint was
    /// where the three-way form was first written, and it stayed here alone
    /// long enough for the organization surface to grow the numeric-only twin
    /// the shared type now removes.
    #[serde(flatten)]
    pub user: UserRef,
    /// read / write / admin
    #[serde(default = "default_permission")]
    pub permission: String,
}

fn default_permission() -> String {
    "read".to_string()
}

#[derive(Deserialize)]
pub struct UpdatePermissionRequest {
    pub permission: String,
}

// ── Handlers ──────────────────────────────────────────────────────────

/// List collaborators for a repo.
/// GET /api/v1/repos/:owner/:name/collaborators
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/collaborators",
    tag = "Collaborators",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Access denied", body = serde_json::Value),
    ),
)]
pub async fn list_collaborators(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::collaborator::service::list_collaborators(&state.db, &owner, &repo).await {
        Ok(collaborators) => (StatusCode::OK, Json(collaborators)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Add a collaborator to a repo.
/// POST /api/v1/repos/:owner/:name/collaborators
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/collaborators",
    tag = "Collaborators",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = serde_json::Value),
        (status = 409, description = "That user is already a collaborator", body = serde_json::Value),
    ),
)]
pub async fn add_collaborator(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    // Granting access to a repository is an admin operation on *that*
    // repository. Checking only that a token parses authorizes nothing: it lets
    // any account hand itself `admin` on any repo, private ones included.
    RepoAdmin { .. }: RepoAdmin,
    Json(req): Json<AddCollaboratorRequest>,
) -> impl IntoResponse {
    let user = match req.user.resolve(&state.db).await {
        Ok(user) => user,
        // The resolver types the four ways the request itself can be wrong; the
        // lookups inside it are ours, and a failed one must not come back as
        // "no such user".
        Err(e) => return AppError::from(e).into_response(),
    };

    match rg_core::collaborator::service::add_collaborator(
        &state.db,
        &owner,
        &repo,
        user.id,
        req.permission,
    )
    .await
    {
        Ok(collab) => (StatusCode::CREATED, Json(collab)).into_response(),
        // An unknown permission or an already-listed user is 400; an unknown
        // repository is 404; a failed insert is a 5xx.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Update a collaborator's permission.
/// PATCH /api/v1/repos/:owner/:name/collaborators/:id
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/collaborators/{id}",
    tag = "Collaborators",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "repo_collaborators row id of the membership to \
          update (repo_collaborators.id — unlike DELETE on this path, which takes the \
          collaborator's users.id)"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Collaborator not found", body = serde_json::Value),
    ),
)]
pub async fn update_permission(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    // Both path segments used to be discarded, which left `id` — a global
    // `repo_collaborators` primary key — as the only thing the handler acted on:
    // no repository was resolved, so nothing was authorized and nothing tied the
    // row to the repo in the URL. The repo the caller holds admin on is what
    // scopes the update.
    RepoAdmin { repo, .. }: RepoAdmin,
    Json(req): Json<UpdatePermissionRequest>,
) -> impl IntoResponse {
    match rg_core::collaborator::service::update_permission(&state.db, repo.id, id, req.permission)
        .await
    {
        Ok(collab) => (StatusCode::OK, Json(collab)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Remove a collaborator from a repo.
/// DELETE /api/v1/repos/:owner/:name/collaborators/:id
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/collaborators/{id}",
    tag = "Collaborators",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "user id of the collaborator to remove (users.id — \
          unlike PATCH on this path, which takes the repo_collaborators row id)"),
    ),
    responses(
        (status = 204, description = "Removed"),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Collaborator not found", body = serde_json::Value),
    ),
)]
pub async fn remove_collaborator(
    State(state): State<AppState>,
    Path((_, _, user_id)): Path<(String, String, i64)>,
    // Revoking access is the same admin operation as granting it — without this
    // any account could strip the collaborators off someone else's repository.
    // The repo the check was about is also what scopes the delete, exactly as in
    // `update_permission`; re-resolving it from the path would be a second
    // lookup that could disagree with the one the authorization used.
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match rg_core::collaborator::service::remove_collaborator(&state.db, repo.id, user_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        // A delete that matched nothing is a 404, not a 204: this path takes a
        // `users.id` while `PATCH` on the identical URL takes the row id, and
        // passing the wrong one has to be visible. Everything else here is
        // ours — the repo was resolved and authorized above — so a failed
        // delete stays a 5xx rather than an accusation aimed at the client.
        Err(e) => AppError::from(e).into_response(),
    }
}
