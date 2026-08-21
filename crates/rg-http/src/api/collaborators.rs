//! REST API handlers for repository collaborators.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::access_audit::{grant_actor, record_grant};
use crate::api::repo_access::{RepoAdmin, RepoRead};
use crate::api::user_ref::{accounts_by_id, UserRef};
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

/// A collaborator row, named.
///
/// The handlers used to serialize `repo_collaborators` itself, which carries a
/// `user_id` and nothing else — so the page that answers "who can push to this
/// repository" listed `#7`, and the issue page's assignee picker offered
/// "User #7" for everyone but the reader (card_73ce6d28518b). The name travels
/// with the row because the client has nowhere to look it up: there is no
/// `/users/{username}` on this instance.
///
/// `username` is `Option`, matching the organization member listing: the row,
/// not the account, is what this endpoint lists, so an id that resolves to
/// nothing has to stay visible and unnamed rather than shorten a list that
/// answers "who has access". Today the schema makes that unreachable —
/// `repo_collaborators.user_id` cascades, so a grant leaves with its account
/// (card_dd3f86fde48e) — which is why this is a shape rather than a branch
/// with a test of its own.
#[derive(Serialize)]
struct CollaboratorResponse {
    id: i64,
    repo_id: i64,
    user_id: i64,
    username: Option<String>,
    display_name: Option<String>,
    permission: String,
    created_at: String,
}

impl CollaboratorResponse {
    fn new(
        row: rg_db::entities::repo_collaborator::Model,
        user: Option<&rg_db::entities::user::Model>,
    ) -> Self {
        Self {
            id: row.id,
            repo_id: row.repo_id,
            user_id: row.user_id,
            username: user.map(|u| u.username.clone()),
            display_name: user.and_then(|u| u.display_name.clone()),
            permission: row.permission,
            created_at: row.created_at.to_string(),
        }
    }
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
        Ok(collaborators) => {
            let ids: Vec<i64> = collaborators.iter().map(|row| row.user_id).collect();
            let named = match accounts_by_id(&state.db, &ids).await {
                Ok(named) => named,
                Err(e) => return AppError::from(e).into_response(),
            };
            let response: Vec<CollaboratorResponse> = collaborators
                .into_iter()
                .map(|row| {
                    let user = named.get(&row.user_id);
                    CollaboratorResponse::new(row, user)
                })
                .collect();
            (StatusCode::OK, Json(response)).into_response()
        }
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
    RepoAdmin {
        repo: repository,
        actor_id,
    }: RepoAdmin,
    headers: HeaderMap,
    Json(req): Json<AddCollaboratorRequest>,
) -> impl IntoResponse {
    // Named before the grant, so a failed lookup is a 500 from a request that
    // handed out nothing — see `access_audit`.
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
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
        // The account was resolved above, so the created row is named without a
        // second lookup — and the client that just added someone by username
        // gets the same shape back that the listing carries.
        Ok(collab) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.add_collaborator",
                &owner,
                &repository,
                &headers,
                serde_json::json!({
                    "added_user_id": user.id,
                    "added_username": user.username,
                    "permission": collab.permission,
                }),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(CollaboratorResponse::new(collab, Some(&user))),
            )
                .into_response()
        }
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
    Path((owner, _, id)): Path<(String, String, i64)>,
    // Both path segments used to be discarded, which left `id` — a global
    // `repo_collaborators` primary key — as the only thing the handler acted on:
    // no repository was resolved, so nothing was authorized and nothing tied the
    // row to the repo in the URL. The repo the caller holds admin on is what
    // scopes the update.
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(req): Json<UpdatePermissionRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    match rg_core::collaborator::service::update_permission(&state.db, repo.id, id, req.permission)
        .await
    {
        Ok(collab) => {
            // One lookup for the one row this answers with. A failure here is
            // ours, not the caller's — the permission change already happened.
            let named = match accounts_by_id(&state.db, &[collab.user_id]).await {
                Ok(named) => named,
                Err(e) => return AppError::from(e).into_response(),
            };
            let user = named.get(&collab.user_id);
            // A permission change is a grant change: `write` on a repository is
            // what the push gate reads, so moving someone from `read` to `write`
            // hands out exactly what `add_collaborator` does.
            record_grant(
                &state,
                &audit_actor,
                "repo.update_collaborator",
                &owner,
                &repo,
                &headers,
                serde_json::json!({
                    "user_id": collab.user_id,
                    "username": user.map(|user| user.username.clone()),
                    "permission": collab.permission,
                }),
            )
            .await;
            (
                StatusCode::OK,
                Json(CollaboratorResponse::new(collab, user)),
            )
                .into_response()
        }
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
    Path((owner, _, user_id)): Path<(String, String, i64)>,
    // Revoking access is the same admin operation as granting it — without this
    // any account could strip the collaborators off someone else's repository.
    // The repo the check was about is also what scopes the delete, exactly as in
    // `update_permission`; re-resolving it from the path would be a second
    // lookup that could disagree with the one the authorization used.
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    // Named *before* the delete: this path takes a `users.id` and nothing else,
    // so once the membership row is gone the journal would have only the number
    // back — which is the half of the entry a reader actually needs.
    let removed = match accounts_by_id(&state.db, &[user_id]).await {
        Ok(named) => named.get(&user_id).map(|user| user.username.clone()),
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::collaborator::service::remove_collaborator(&state.db, repo.id, user_id).await {
        Ok(()) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.remove_collaborator",
                &owner,
                &repo,
                &headers,
                serde_json::json!({
                    "removed_user_id": user_id,
                    "removed_username": removed,
                }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        // A delete that matched nothing is a 404, not a 204: this path takes a
        // `users.id` while `PATCH` on the identical URL takes the row id, and
        // passing the wrong one has to be visible. Everything else here is
        // ours — the repo was resolved and authorized above — so a failed
        // delete stays a 5xx rather than an accusation aimed at the client.
        Err(e) => AppError::from(e).into_response(),
    }
}
