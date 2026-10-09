//! REST API handlers for organizations and teams.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthUser;
use crate::api::user_ref::{accounts_by_id, UserRef};
use crate::error::AppError;
use crate::AppState;

// ── Response types ───────────────────────────────────────────

#[derive(Serialize)]
struct OrgResponse {
    id: i64,
    name: String,
    display_name: Option<String>,
    description: Option<String>,
    owner_id: i64,
    visibility: String,
    created_at: String,
    updated_at: String,
}

/// A membership row, named.
///
/// `user_id` alone is what the page had to render, and it rendered it as
/// "User #3" — a member list that cannot tell its reader who is in the
/// organization (card_cb9f71672b11). The name travels with the row rather than
/// being looked up per entry by the client, which has no endpoint to look it
/// up with.
///
/// `username` is `Option` because the row, not the account, is the thing this
/// endpoint is listing: an id that resolves to nothing is a membership that
/// exists and must stay visible, unnamed, instead of quietly vanishing from
/// the list that is supposed to answer "who has access".
#[derive(Serialize)]
struct OrgMemberResponse {
    id: i64,
    org_id: i64,
    user_id: i64,
    username: Option<String>,
    display_name: Option<String>,
    role: String,
    created_at: String,
}

#[derive(Serialize)]
struct TeamResponse {
    id: i64,
    org_id: i64,
    name: String,
    description: Option<String>,
    permission: String,
    created_at: String,
    updated_at: String,
}

/// A team membership row, named. Same reasoning as [`OrgMemberResponse`].
#[derive(Serialize)]
struct TeamMemberResponse {
    id: i64,
    team_id: i64,
    user_id: i64,
    username: Option<String>,
    display_name: Option<String>,
    role: String,
    created_at: String,
}

#[derive(Deserialize)]
pub struct CreateOrgRequest {
    name: String,
    display_name: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateOrgRequest {
    display_name: Option<String>,
    description: Option<String>,
    visibility: Option<String>,
}

/// Who to let into the organization, and as what.
///
/// The account is named through [`UserRef`], so `username` and `email` work
/// here exactly as they already did on the repository collaborator endpoint.
/// A body that sends `user_id` still works — this widened what the endpoint
/// accepts, it did not replace it.
#[derive(Deserialize)]
pub struct AddOrgMemberRequest {
    #[serde(flatten)]
    user: UserRef,
    role: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateTeamRequest {
    name: String,
    description: Option<String>,
    permission: Option<String>,
}

/// Who to put on the team, and as what. Same three names as
/// [`AddOrgMemberRequest`].
#[derive(Deserialize)]
pub struct AddTeamMemberRequest {
    #[serde(flatten)]
    user: UserRef,
    role: Option<String>,
}

// ── Organization handlers ────────────────────────────────────

/// POST /api/v1/orgs
#[utoipa::path(
    post,
    path = "/orgs",
    tag = "Organizations",
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "That organization name is already taken", body = serde_json::Value),
    ),
)]
pub async fn create_org(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Json(body): Json<CreateOrgRequest>,
) -> impl IntoResponse {
    // The organization is the *resource*, never the actor. This file used to
    // pass `&org.name` (and, on create, `&body.name`) into the actor column —
    // one call site even said so: "username not available, use org name". The
    // admin journal renders that column as who acted, so `acme-corp (#3)` read
    // as a person doing things (card_fcc07f8d1505). The org's name is recorded
    // where it belongs, as `resource_name`, a few lines below.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, user_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    let visibility = body.visibility.as_deref().unwrap_or("public");

    match rg_core::org::create_org(
        &state.db,
        &body.name,
        body.display_name.as_deref(),
        body.description.as_deref(),
        user_id,
        visibility,
    )
    .await
    {
        Ok(org) => {
            // Record audit log
            let details = serde_json::json!({
                "name": body.name,
                "display_name": body.display_name,
                "visibility": visibility
            });
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "org.create",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                Some(&headers),
                Some(details),
            )
            .await;

            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": org.id,
                    "name": org.name,
                    "display_name": org.display_name,
                    "visibility": org.visibility,
                })),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/orgs/:name
#[utoipa::path(
    get,
    path = "/orgs/{name}",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_org(OrgRead { org, .. }: OrgRead) -> impl IntoResponse {
    Json(org_to_response(&org)).into_response()
}

/// GET /api/v1/orgs
/// List organizations for the authenticated user.
#[utoipa::path(
    get,
    path = "/orgs",
    tag = "Organizations",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_orgs(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> impl IntoResponse {
    match rg_core::org::list_user_orgs(&state.db, user_id).await {
        Ok(orgs) => {
            let resp: Vec<OrgResponse> = orgs.iter().map(org_to_response).collect();
            Json(resp).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// PATCH /api/v1/orgs/:name
#[utoipa::path(
    patch,
    path = "/orgs/{name}",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
    ),
)]
pub async fn update_org(
    State(state): State<AppState>,
    // Ahead of `Json<_>` because it has to run ahead of it: the gate used to be
    // the first three statements of this body, which put it *behind* the
    // deserializer, and an anonymous caller was told its payload was malformed
    // instead of being turned away — a rejection that doubles as a schema
    // oracle. As an argument it runs first, and forgetting it is a compile error.
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
    Json(body): Json<UpdateOrgRequest>,
) -> impl IntoResponse {
    // The organization is the *resource*, never the actor. This file used to
    // pass `&org.name` (and, on create, `&body.name`) into the actor column —
    // one call site even said so: "username not available, use org name". The
    // admin journal renders that column as who acted, so `acme-corp (#3)` read
    // as a person doing things (card_fcc07f8d1505). The org's name is recorded
    // where it belongs, as `resource_name`, a few lines below.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::org::update_org(
        &state.db,
        org.id,
        body.display_name.as_deref(),
        body.description.as_deref(),
        body.visibility.as_deref(),
    )
    .await
    {
        Ok(updated) => {
            let details = serde_json::json!({
                "name": org.name,
                "display_name": body.display_name,
                "visibility": body.visibility
            });
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "org.update",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                Some(&headers),
                Some(details),
            )
            .await;
            Json(org_to_response(&updated)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/orgs/:name
#[utoipa::path(
    delete,
    path = "/orgs/{name}",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
    ),
)]
pub async fn delete_org(
    State(state): State<AppState>,
    // `OrgAdmin` is the level the route declares — the floor. Deleting an
    // organization is owner-only, and the owner is a *role* on a membership row:
    // `rg_core::org::delete_org` used to compare `org.owner_id` instead, which
    // let the creator delete an organization they had been removed from while
    // refusing the members who actually held the role (security audit #5).
    // Membership is decided in this module alone, so the rule is this extractor.
    OrgOwner { org, actor_id }: OrgOwner,
    headers: HeaderMap,
) -> impl IntoResponse {
    // The organization is the *resource*, never the actor. This file used to
    // pass `&org.name` (and, on create, `&body.name`) into the actor column —
    // one call site even said so: "username not available, use org name". The
    // admin journal renders that column as who acted, so `acme-corp (#3)` read
    // as a person doing things (card_fcc07f8d1505). The org's name is recorded
    // where it belongs, as `resource_name`, a few lines below.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::org::delete_org(
        &state.db,
        &state.repo_root,
        state.blob_storage.as_ref(),
        state.oci_storage.as_ref(),
        org.id,
        rg_core::org::OrgDeleteActor::Owner(actor_id),
    )
    .await
    {
        Ok(()) => {
            let details = serde_json::json!({"name": org.name});
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "org.delete",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                Some(&headers),
                Some(details),
            )
            .await;
            Json(serde_json::json!({"deleted": true})).into_response()
        }
        // The refusal happened in the extractor; what is left is a lost race
        // (typed `NotFound`) or a failure of ours, and neither must be reported
        // as "you are not allowed to delete this organization".
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Who the organization is handed to. Same [`UserRef`] as
/// [`AddOrgMemberRequest`]: `username` is the spelling the page sends.
#[derive(Deserialize)]
pub struct TransferOwnershipRequest {
    #[serde(flatten)]
    user: UserRef,
}

/// POST /api/v1/orgs/:name/transfer-ownership
///
/// Hands the organization to another *member*, who is raised to the `owner`
/// role; the caller keeps theirs. One transaction rewrites the membership row,
/// `organizations.owner_id` and the `owner_id` of every repository of the
/// organization that still mirrored the previous owner — see
/// `rg_db::ops::org_ops::transfer_ownership` for why the three move together.
#[utoipa::path(
    post,
    path = "/orgs/{name}/transfer-ownership",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Ownership transferred; the organization as it now reads", body = serde_json::Value),
        (status = 400, description = "The named account does not exist", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Only an owner-role member may transfer ownership", body = serde_json::Value),
        (status = 404, description = "Organization not found", body = serde_json::Value),
        (status = 409, description = "The target is not a member, or already the owner", body = serde_json::Value),
    ),
)]
pub async fn transfer_ownership(
    State(state): State<AppState>,
    OrgOwner { org, actor_id }: OrgOwner,
    headers: HeaderMap,
    Json(body): Json<TransferOwnershipRequest>,
) -> impl IntoResponse {
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    // Resolved first so an unknown name is a 400 here rather than a membership
    // miss reported as a 409.
    let target = match body.user.resolve(&state.db).await {
        Ok(user) => user,
        Err(e) => return AppError::from(e).into_response(),
    };

    match rg_core::org::transfer_ownership(&state.db, org.id, target.id).await {
        Ok(updated) => {
            let details = serde_json::json!({
                "org_name": org.name,
                "previous_owner_id": org.owner_id,
                "new_owner_id": target.id,
                "new_owner_username": target.username,
            });
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "org.transfer_ownership",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                Some(&headers),
                Some(details),
            )
            .await;
            Json(org_to_response(&updated)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Organization Member handlers ────────────────────────────

/// GET /api/v1/orgs/:name/members
#[utoipa::path(
    get,
    path = "/orgs/{name}/members",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_org_members(
    State(state): State<AppState>,
    OrgRead { org, .. }: OrgRead,
) -> impl IntoResponse {
    match rg_core::org::list_org_members(&state.db, org.id).await {
        Ok(members) => {
            let ids: Vec<i64> = members.iter().map(|m| m.user_id).collect();
            let named = match accounts_by_id(&state.db, &ids).await {
                Ok(named) => named,
                Err(e) => return AppError::from(e).into_response(),
            };
            let resp: Vec<OrgMemberResponse> = members
                .into_iter()
                .map(|m| {
                    let user = named.get(&m.user_id);
                    OrgMemberResponse {
                        id: m.id,
                        org_id: m.org_id,
                        user_id: m.user_id,
                        username: user.map(|u| u.username.clone()),
                        display_name: user.and_then(|u| u.display_name.clone()),
                        role: m.role,
                        created_at: m.created_at.to_string(),
                    }
                })
                .collect();
            Json(resp).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/orgs/:name/members
#[utoipa::path(
    post,
    path = "/orgs/{name}/members",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
    ),
)]
pub async fn add_org_member(
    State(state): State<AppState>,
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
    Json(body): Json<AddOrgMemberRequest>,
) -> impl IntoResponse {
    // The organization is the *resource*, never the actor. This file used to
    // pass `&org.name` (and, on create, `&body.name`) into the actor column —
    // one call site even said so: "username not available, use org name". The
    // admin journal renders that column as who acted, so `acme-corp (#3)` read
    // as a person doing things (card_fcc07f8d1505). The org's name is recorded
    // where it belongs, as `resource_name`, a few lines below.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    let role = body.role.as_deref().unwrap_or("member");
    // Resolving before the insert is also what keeps a mistyped identifier a
    // 400: `organization_members.user_id` is a foreign key, so an id naming
    // nobody used to reach the database and come back as a constraint failure.
    let member_user = match body.user.resolve(&state.db).await {
        Ok(user) => user,
        Err(e) => return AppError::from(e).into_response(),
    };

    match rg_core::org::add_org_member(&state.db, org.id, member_user.id, role).await {
        Ok(m) => {
            // The journal records the name too: this whole endpoint exists
            // because an owner could not find out who `#3` was, and an audit
            // entry that only says `added_user_id: 3` has the same problem.
            let details = serde_json::json!({
                "org_name": org.name,
                "added_user_id": member_user.id,
                "added_username": member_user.username,
                "role": role
            });
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "org.add_member",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                Some(&headers),
                Some(details),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": m.id,
                    "org_id": m.org_id,
                    "user_id": m.user_id,
                    "username": member_user.username,
                    "display_name": member_user.display_name,
                    "role": m.role,
                })),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/orgs/:name/members/:user_id
#[utoipa::path(
    delete,
    path = "/orgs/{name}/members/{user_id}",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
        ("user_id" = i64, Path, description = "user_id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Organization member not found", body = serde_json::Value),
    ),
)]
pub async fn remove_org_member(
    State(state): State<AppState>,
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
    Path((_name, user_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    // The organization is the *resource*, never the actor. This file used to
    // pass `&org.name` (and, on create, `&body.name`) into the actor column —
    // one call site even said so: "username not available, use org name". The
    // admin journal renders that column as who acted, so `acme-corp (#3)` read
    // as a person doing things (card_fcc07f8d1505). The org's name is recorded
    // where it belongs, as `resource_name`, a few lines below.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::org::remove_org_member(&state.db, org.id, user_id).await {
        Ok(()) => {
            let details = serde_json::json!({
                "org_name": org.name,
                "removed_user_id": user_id
            });
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "org.remove_member",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                Some(&headers),
                Some(details),
            )
            .await;
            Json(serde_json::json!({"removed": true})).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Team handlers ────────────────────────────────────────────

/// POST /api/v1/orgs/:name/teams
#[utoipa::path(
    post,
    path = "/orgs/{name}/teams",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
    ),
)]
pub async fn create_team(
    State(state): State<AppState>,
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
    Json(body): Json<CreateTeamRequest>,
) -> impl IntoResponse {
    let permission = body.permission.as_deref().unwrap_or("read");
    // The team surface hands out access to the organization's private
    // repositories — `require_org_admin` says so itself, and
    // `rg_core::org::add_team_member` flushes the permission cache precisely
    // because it can. `org.add_member` was audited and this, the operation that
    // actually grants the repository permission, was not (card_cb0d1ca78d57).
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::org::create_team(
        &state.db,
        org.id,
        &body.name,
        body.description.as_deref(),
        permission,
    )
    .await
    {
        Ok(team) => {
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "team.create",
                Some("team"),
                Some(team.id),
                Some(&team.name),
                Some(&headers),
                Some(serde_json::json!({
                    "org": org.name,
                    "org_id": org.id,
                    "permission": team.permission,
                })),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": team.id,
                    "org_id": team.org_id,
                    "name": team.name,
                    "permission": team.permission,
                })),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/orgs/:name/teams
#[utoipa::path(
    get,
    path = "/orgs/{name}/teams",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_org_teams(
    State(state): State<AppState>,
    OrgRead { org, .. }: OrgRead,
) -> impl IntoResponse {
    match rg_core::org::list_org_teams(&state.db, org.id).await {
        Ok(teams) => {
            let resp: Vec<TeamResponse> = teams
                .into_iter()
                .map(|t| TeamResponse {
                    id: t.id,
                    org_id: t.org_id,
                    name: t.name,
                    description: t.description,
                    permission: t.permission,
                    created_at: t.created_at.to_string(),
                    updated_at: t.updated_at.to_string(),
                })
                .collect();
            Json(resp).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/orgs/:name/teams/:team_id
#[utoipa::path(
    get,
    path = "/orgs/{name}/teams/{team_id}",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
        ("team_id" = i64, Path, description = "team_id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_team(
    State(state): State<AppState>,
    OrgRead { org, .. }: OrgRead,
    Path((_name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };

    Json(TeamResponse {
        id: team.id,
        org_id: team.org_id,
        name: team.name,
        description: team.description,
        permission: team.permission,
        created_at: team.created_at.to_string(),
        updated_at: team.updated_at.to_string(),
    })
    .into_response()
}

/// DELETE /api/v1/orgs/:name/teams/:team_id
#[utoipa::path(
    delete,
    path = "/orgs/{name}/teams/{team_id}",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
        ("team_id" = i64, Path, description = "team_id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
    ),
)]
pub async fn delete_team(
    State(state): State<AppState>,
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
    Path((_name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };
    // The team surface hands out access to the organization's private
    // repositories — `require_org_admin` says so itself, and
    // `rg_core::org::add_team_member` flushes the permission cache precisely
    // because it can. `org.add_member` was audited and this, the operation that
    // actually grants the repository permission, was not (card_cb0d1ca78d57).
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::org::delete_team(&state.db, team.id).await {
        Ok(()) => {
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "team.delete",
                Some("team"),
                Some(team.id),
                Some(&team.name),
                Some(&headers),
                Some(serde_json::json!({
                    "org": org.name,
                    "org_id": org.id,
                    "permission": team.permission,
                })),
            )
            .await;
            Json(serde_json::json!({"deleted": true})).into_response()
        }
        // Only the typed `NotFound` the service raises for an absent team may
        // become a `404` here; a failed delete is ours and stays a 5xx (its
        // `db: …` context would otherwise reach the body verbatim — a `404` is
        // not sanitized in `IntoResponse`).
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/orgs/:name/teams/:team_id/members
#[utoipa::path(
    get,
    path = "/orgs/{name}/teams/{team_id}/members",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
        ("team_id" = i64, Path, description = "team_id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_team_members(
    State(state): State<AppState>,
    OrgRead { org, .. }: OrgRead,
    Path((_name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };

    match rg_core::org::list_team_members(&state.db, team.id).await {
        Ok(members) => {
            let ids: Vec<i64> = members.iter().map(|m| m.user_id).collect();
            let named = match accounts_by_id(&state.db, &ids).await {
                Ok(named) => named,
                Err(e) => return AppError::from(e).into_response(),
            };
            let resp: Vec<TeamMemberResponse> = members
                .into_iter()
                .map(|m| {
                    let user = named.get(&m.user_id);
                    TeamMemberResponse {
                        id: m.id,
                        team_id: m.team_id,
                        user_id: m.user_id,
                        username: user.map(|u| u.username.clone()),
                        display_name: user.and_then(|u| u.display_name.clone()),
                        role: m.role,
                        created_at: m.created_at.to_string(),
                    }
                })
                .collect();
            Json(resp).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/orgs/:name/teams/:team_id/members
#[utoipa::path(
    post,
    path = "/orgs/{name}/teams/{team_id}/members",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
        ("team_id" = i64, Path, description = "team_id"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
    ),
)]
pub async fn add_team_member(
    State(state): State<AppState>,
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
    Path((_name, team_id)): Path<(String, i64)>,
    Json(body): Json<AddTeamMemberRequest>,
) -> impl IntoResponse {
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };

    let role = body.role.as_deref().unwrap_or("member");
    // The team surface hands out access to the organization's private
    // repositories — `require_org_admin` says so itself, and
    // `rg_core::org::add_team_member` flushes the permission cache precisely
    // because it can. `org.add_member` was audited and this, the operation that
    // actually grants the repository permission, was not (card_cb0d1ca78d57).
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    let member_user = match body.user.resolve(&state.db).await {
        Ok(user) => user,
        Err(e) => return AppError::from(e).into_response(),
    };

    match rg_core::org::add_team_member(&state.db, team.id, member_user.id, role).await {
        Ok(m) => {
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "team.add_member",
                Some("team"),
                Some(team.id),
                Some(&team.name),
                Some(&headers),
                Some(serde_json::json!({
                    "org": org.name,
                    "org_id": org.id,
                    "member_user_id": member_user.id,
                    "member_username": member_user.username,
                    "role": role,
                    "team_permission": team.permission,
                })),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": m.id,
                    "team_id": m.team_id,
                    "user_id": m.user_id,
                    "username": member_user.username,
                    "display_name": member_user.display_name,
                    "role": m.role,
                })),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/orgs/:name/teams/:team_id/members/:user_id
#[utoipa::path(
    delete,
    path = "/orgs/{name}/teams/{team_id}/members/{user_id}",
    tag = "Organizations",
    params(
        ("name" = String, Path, description = "name"),
        ("team_id" = i64, Path, description = "team_id"),
        ("user_id" = i64, Path, description = "user_id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Team member not found", body = serde_json::Value),
    ),
)]
pub async fn remove_team_member(
    State(state): State<AppState>,
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
    Path((_name, team_id, user_id)): Path<(String, i64, i64)>,
) -> impl IntoResponse {
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };
    // The team surface hands out access to the organization's private
    // repositories — `require_org_admin` says so itself, and
    // `rg_core::org::add_team_member` flushes the permission cache precisely
    // because it can. `org.add_member` was audited and this, the operation that
    // actually grants the repository permission, was not (card_cb0d1ca78d57).
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::org::remove_team_member(&state.db, team.id, user_id).await {
        Ok(()) => {
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "team.remove_member",
                Some("team"),
                Some(team.id),
                Some(&team.name),
                Some(&headers),
                Some(serde_json::json!({
                    "org": org.name,
                    "org_id": org.id,
                    "member_user_id": user_id,
                    "team_permission": team.permission,
                })),
            )
            .await;
            Json(serde_json::json!({"removed": true})).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Helpers ──────────────────────────────────────────────────

/// Authentication gate for the handlers below.
///
/// The `/api/v1` router carries no mandatory-auth layer: `pat_auth_middleware`
/// only *translates* a PAT into a JWT and lets a request with no credentials
/// through untouched. Requiring a caller is therefore each handler's own job —
/// a handler that forgets this is anonymous, not merely unauthorized.
fn require_user(headers: &HeaderMap, jwt_secret: &str) -> Result<i64, AppError> {
    super::auth::extract_user_id(headers, jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))
}

/// Resolve the organization named in the path.
async fn resolve_org(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> Result<rg_db::entities::organization::Model, AppError> {
    match rg_core::org::get_org_by_name(db, name).await {
        Ok(Some(org)) => Ok(org),
        Ok(None) => Err(AppError::not_found("organization not found")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Whether `user_id` may administer `org`: a member holding the `owner` or
/// `admin` role.
///
/// `org.owner_id` is deliberately not consulted — here, in
/// [`require_org_visible`], or anywhere else a right is decided. The column
/// names the account that created the organization (or last received it through
/// `transfer_ownership`); it stays on the row when that account is removed from
/// the organization, and reading it as a grant kept the removed creator in full
/// control while the members actually holding the `owner` role were refused
/// (security audit #5). Rights come from membership rows and nothing else.
async fn is_org_admin(
    db: &sea_orm::DatabaseConnection,
    org: &rg_db::entities::organization::Model,
    user_id: i64,
) -> anyhow::Result<bool> {
    Ok(rg_core::org::find_org_member(db, org.id, user_id)
        .await?
        .is_some_and(|m| m.role == "owner" || m.role == "admin"))
}

/// Whether `user_id` *owns* `org`: a member holding the `owner` role. The
/// `admin` role is not enough — see [`is_org_admin`] for the column that is not
/// consulted either.
async fn is_org_owner(
    db: &sea_orm::DatabaseConnection,
    org: &rg_db::entities::organization::Model,
    user_id: i64,
) -> anyhow::Result<bool> {
    Ok(rg_core::org::find_org_member(db, org.id, user_id)
        .await?
        .is_some_and(|m| m.role == "owner"))
}

/// Authorization gate for the two things only an owner may do with an
/// organization: delete it, and hand its ownership to somebody else.
async fn require_org_owner(
    db: &sea_orm::DatabaseConnection,
    org: &rg_db::entities::organization::Model,
    user_id: i64,
) -> Result<(), AppError> {
    match is_org_owner(db, org, user_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::forbidden("only organization owners can do this")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Authorization gate for every org mutation — membership changes and the whole
/// team surface, both of which hand out access to the organization's private
/// repositories (`rg_core::org::add_team_member` flushes the permission cache
/// precisely because it can).
async fn require_org_admin(
    db: &sea_orm::DatabaseConnection,
    org: &rg_db::entities::organization::Model,
    user_id: i64,
) -> Result<(), AppError> {
    match is_org_admin(db, org, user_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::forbidden(
            "only organization owners and admins can do this",
        )),
        // A failed membership lookup is ours, not a refusal: answering `403`
        // would tell the caller they lack a permission we never managed to read.
        Err(e) => Err(AppError::from(e)),
    }
}

/// Visibility gate for org-scoped routes. A public org is world-readable; a
/// private one answers `404` — not `403` — to everyone outside it, so no
/// org-scoped route confirms its members, teams, settings or repositories.
///
/// The name itself is not what the `404` protects, and this comment used to say
/// otherwise ("so the endpoint cannot be used to enumerate private
/// organizations by name"). Organization names live in one global namespace
/// with usernames, so `POST /orgs` answers `organization name 'x' is already
/// taken` to any account that can log in — an existence check on any name, one
/// request, no `404` involved (card_2179245d41db). What this gate is worth is
/// the *contents*: an outsider who already knows a private org is there still
/// cannot read a member list off it, and the `404` keeps a name they guessed
/// from being confirmed on the read path too.
///
/// Every org gate runs it, reading and mutating alike: masking that only one
/// level performs is not masking, because the caller picks the level by picking
/// the verb.
async fn require_org_visible(
    db: &sea_orm::DatabaseConnection,
    org: &rg_db::entities::organization::Model,
    user_id: Option<i64>,
) -> Result<(), AppError> {
    if org.visibility != "private" {
        return Ok(());
    }
    let Some(user_id) = user_id else {
        return Err(AppError::not_found("organization not found"));
    };
    // Membership only — not `org.owner_id`, for the reason on `is_org_admin`.
    match rg_core::org::is_org_member(db, org.id, user_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::not_found("organization not found")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Resolve a team *inside* the organization named in the path.
///
/// The `org_id` comparison is the whole point: without it `{name}` is decorative
/// and any org admin can name another organization's `team_id` under their own
/// path. A team belonging elsewhere is a `404` rather than a `403` — a `403`
/// would confirm that the id exists.
async fn resolve_team_in_org(
    db: &sea_orm::DatabaseConnection,
    org_id: i64,
    team_id: i64,
) -> Result<rg_db::entities::team::Model, AppError> {
    match rg_core::org::get_team(db, team_id).await {
        Ok(Some(team)) if team.org_id == org_id => Ok(team),
        Ok(_) => Err(AppError::not_found("team not found")),
        Err(e) => Err(AppError::from(e)),
    }
}

// ── The org-admin gate as a handler argument ─────────────────
//
// `require_user` + `resolve_org` + `require_org_admin` is the same three-step
// rule in every mutating handler, and a handler that also takes a body ran it
// *after* `Json<_>`: axum runs every `FromRequestParts` before the single
// `FromRequest`, so an anonymous caller was told its JSON was malformed rather
// than being turned away, and a well-formed-looking rejection doubled as a
// schema oracle. Stating the gate in the signature runs it first and makes
// forgetting it a compile error rather than a review miss.

/// A caller allowed to read the organization named by `{name}`.
///
/// Same steps, same order, same answers as the hand-written prologue it
/// replaces: a public organization resolves for anybody, a private one is a
/// `404` — not a `403` — to everyone outside it, so the route reveals nothing
/// about the organization it refuses to show. Not the same thing as hiding the
/// name: see [`require_org_visible`] for what the `404` does and does not buy.
///
/// The viewer is `None` for an anonymous caller, and that is precisely why this
/// is its own extractor rather than a weaker rung of [`OrgAdmin`]: the level
/// admits a caller with no identity to report, so there is no `actor_id` to hand
/// over and no scale the two levels share. It stays inside the extractor's
/// `from_request_parts` — deciding visibility is this extractor's whole job, and
/// every one of the five handlers destructures
/// `OrgRead { org, .. }`. Carrying it out as a `pub` field only offered a second,
/// ungated way to identify the caller.
pub struct OrgRead {
    pub org: rg_db::entities::organization::Model,
}

impl axum::extract::FromRequestParts<AppState> for OrgRead {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let viewer = super::auth::extract_user_id(&parts.headers, &state.jwt_secret);
        let name = org_name_in_path(parts, state).await?;
        let org = resolve_org(&state.db, &name).await?;
        require_org_visible(&state.db, &org, viewer).await?;
        Ok(Self { org })
    }
}

/// An authenticated administrator of the organization named by `{name}`.
///
/// Same steps, same order, same answers as the hand-written prologue it
/// replaces: no session is `401`, an unknown organization is `404`, and a
/// caller who is neither its owner nor an admin is `403`.
///
/// It runs [`require_org_visible`] before [`require_org_admin`] for the reason
/// that gate exists at all: without it a private organization answered `403`
/// here while an unknown one answered `404`, so the masking [`OrgRead`] provides
/// came off by changing the verb on a neighbouring route — `PATCH /orgs/{name}`
/// confirmed by name what `GET /orgs/{name}` refuses to. Visibility first makes
/// both answers `404`. A *member* of the private organization still gets `403`:
/// they can already see it, so refusing them by permission leaks nothing.
pub struct OrgAdmin {
    pub org: rg_db::entities::organization::Model,
    pub actor_id: i64,
}

impl axum::extract::FromRequestParts<AppState> for OrgAdmin {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let actor_id = require_user(&parts.headers, &state.jwt_secret)?;
        let name = org_name_in_path(parts, state).await?;
        let org = resolve_org(&state.db, &name).await?;
        require_org_visible(&state.db, &org, Some(actor_id)).await?;
        require_org_admin(&state.db, &org, actor_id).await?;
        Ok(Self { org, actor_id })
    }
}

/// An authenticated *owner* of the organization named by `{name}` — a member
/// holding the `owner` role.
///
/// The rung above [`OrgAdmin`], for the two operations that dispose of the
/// organization rather than run it: `DELETE /orgs/{name}` and
/// `POST /orgs/{name}/transfer-ownership`. Same order and same answers as
/// [`OrgAdmin`] up to the last step — `401` without a session, `404` for an
/// unknown or private-and-foreign organization — and then `403` for a member
/// who is not an owner, admins included. The route table declares these rows
/// `OrgAdmin`, which is the floor the sweep measures a stranger against; this
/// extractor is the stricter rule the handler actually runs.
pub struct OrgOwner {
    pub org: rg_db::entities::organization::Model,
    pub actor_id: i64,
}

impl axum::extract::FromRequestParts<AppState> for OrgOwner {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let actor_id = require_user(&parts.headers, &state.jwt_secret)?;
        let name = org_name_in_path(parts, state).await?;
        let org = resolve_org(&state.db, &name).await?;
        require_org_visible(&state.db, &org, Some(actor_id)).await?;
        require_org_owner(&state.db, &org, actor_id).await?;
        Ok(Self { org, actor_id })
    }
}

/// Pull the `{name}` capture an org-scoped route carries.
///
/// Read through `Path<HashMap<_, _>>` rather than a positional `Path<_>` so the
/// one gate serves `/orgs/{name}/members` and
/// `/orgs/{name}/teams/{team_id}/members` alike — a tuple would demand the
/// exact arity of each route. Extracting it here does not consume it: axum
/// reads the captures out of the request extensions, so the handler can still
/// take its own `Path<...>`.
async fn org_name_in_path(
    parts: &mut axum::http::request::Parts,
    state: &AppState,
) -> Result<String, AppError> {
    use axum::extract::FromRequestParts as _;

    let Path(params) =
        Path::<std::collections::HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|_| {
                AppError::internal("route carries no path parameters to authorize against")
            })?;

    params
        .get("name")
        .cloned()
        .ok_or_else(|| AppError::internal("route is not organization-scoped: no {name} capture"))
}

fn org_to_response(org: &rg_db::entities::organization::Model) -> OrgResponse {
    OrgResponse {
        id: org.id,
        name: org.name.clone(),
        display_name: org.display_name.clone(),
        description: org.description.clone(),
        owner_id: org.owner_id,
        visibility: org.visibility.clone(),
        created_at: org.created_at.to_string(),
        updated_at: org.updated_at.to_string(),
    }
}
