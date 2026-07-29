//! REST API handlers for organizations and teams.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthUser;
use crate::error::AppError;
use crate::AppState;

/// Helper to record audit log (fire-and-forget).
#[allow(clippy::too_many_arguments)]
async fn record_audit(
    db: &sea_orm::DatabaseConnection,
    user_id: i64,
    username: &str,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<i64>,
    resource_name: Option<&str>,
    headers: &HeaderMap,
    details: Option<serde_json::Value>,
) {
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);

    let entry = rg_db::entities::audit_log::ActiveModel {
        id: sea_orm::NotSet,
        user_id: sea_orm::Set(Some(user_id)),
        username: sea_orm::Set(Some(username.to_string())),
        action: sea_orm::Set(action.to_string()),
        resource_type: sea_orm::Set(resource_type.map(|s| s.to_string())),
        resource_id: sea_orm::Set(resource_id),
        resource_name: sea_orm::Set(resource_name.map(|s| s.to_string())),
        ip_address: sea_orm::Set(ip_address),
        user_agent: sea_orm::Set(user_agent),
        details: sea_orm::Set(details.map(|v| v.to_string())),
        created_at: sea_orm::Set(chrono::Utc::now()),
    };

    if let Err(e) = rg_db::ops::audit_log_ops::insert(db, entry).await {
        tracing::warn!(error = %format!("{e:#}"), "failed to record audit log");
    }
}

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

#[derive(Serialize)]
struct OrgMemberResponse {
    id: i64,
    org_id: i64,
    user_id: i64,
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

#[derive(Serialize)]
struct TeamMemberResponse {
    id: i64,
    team_id: i64,
    user_id: i64,
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

#[derive(Deserialize)]
pub struct AddOrgMemberRequest {
    user_id: i64,
    role: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateTeamRequest {
    name: String,
    description: Option<String>,
    permission: Option<String>,
}

#[derive(Deserialize)]
pub struct AddTeamMemberRequest {
    user_id: i64,
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
    ),
)]
pub async fn create_org(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Json(body): Json<CreateOrgRequest>,
) -> impl IntoResponse {
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
            record_audit(
                &state.db,
                user_id,
                &body.name, // username not available, use org name
                "org.create",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                &headers,
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
pub async fn get_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let viewer = super::auth::extract_user_id(&headers, &state.jwt_secret);
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_visible(&state.db, &org, viewer).await {
        return e.into_response();
    }

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
pub async fn list_orgs(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required").into_response();
        }
    };

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
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<UpdateOrgRequest>,
) -> impl IntoResponse {
    let user_id = match require_user(&headers, &state.jwt_secret) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_admin(&state.db, &org, user_id).await {
        return e.into_response();
    }

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
            record_audit(
                &state.db,
                user_id,
                &org.name,
                "org.update",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                &headers,
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
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let user_id = match require_user(&headers, &state.jwt_secret) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };

    // No `require_org_admin` here on purpose: deleting an organization is
    // owner-only, and `rg_core::org::delete_org` enforces exactly that.
    match rg_core::org::delete_org(&state.db, org.id, user_id).await {
        Ok(()) => {
            let details = serde_json::json!({"name": org.name});
            record_audit(
                &state.db,
                user_id,
                &org.name,
                "org.delete",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                &headers,
                Some(details),
            )
            .await;
            Json(serde_json::json!({"deleted": true})).into_response()
        }
        // Only the owner-mismatch branch is a refusal; it carries
        // `rg_core::error::Forbidden` and still answers 403. A failed lookup
        // or a dead pool underneath is ours, and must not be reported as "you
        // are not allowed to delete this organization".
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
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let viewer = super::auth::extract_user_id(&headers, &state.jwt_secret);
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_visible(&state.db, &org, viewer).await {
        return e.into_response();
    }

    match rg_core::org::list_org_members(&state.db, org.id).await {
        Ok(members) => {
            let resp: Vec<OrgMemberResponse> = members
                .into_iter()
                .map(|m| OrgMemberResponse {
                    id: m.id,
                    org_id: m.org_id,
                    user_id: m.user_id,
                    role: m.role,
                    created_at: m.created_at.to_string(),
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
    let role = body.role.as_deref().unwrap_or("member");

    match rg_core::org::add_org_member(&state.db, org.id, body.user_id, role).await {
        Ok(m) => {
            let details = serde_json::json!({
                "org_name": org.name,
                "added_user_id": body.user_id,
                "role": role
            });
            record_audit(
                &state.db,
                actor_id,
                &org.name,
                "org.add_member",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                &headers,
                Some(details),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": m.id,
                    "org_id": m.org_id,
                    "user_id": m.user_id,
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
    ),
)]
pub async fn remove_org_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((name, user_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let actor_id = match require_user(&headers, &state.jwt_secret) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_admin(&state.db, &org, actor_id).await {
        return e.into_response();
    }

    match rg_core::org::remove_org_member(&state.db, org.id, user_id).await {
        Ok(()) => {
            let details = serde_json::json!({
                "org_name": org.name,
                "removed_user_id": user_id
            });
            record_audit(
                &state.db,
                actor_id,
                &org.name,
                "org.remove_member",
                Some("org"),
                Some(org.id),
                Some(&org.name),
                &headers,
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
    OrgAdmin { org, .. }: OrgAdmin,
    Json(body): Json<CreateTeamRequest>,
) -> impl IntoResponse {
    let permission = body.permission.as_deref().unwrap_or("read");

    match rg_core::org::create_team(
        &state.db,
        org.id,
        &body.name,
        body.description.as_deref(),
        permission,
    )
    .await
    {
        Ok(team) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": team.id,
                "org_id": team.org_id,
                "name": team.name,
                "permission": team.permission,
            })),
        )
            .into_response(),
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
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let viewer = super::auth::extract_user_id(&headers, &state.jwt_secret);
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_visible(&state.db, &org, viewer).await {
        return e.into_response();
    }

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
    headers: HeaderMap,
    Path((name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let viewer = super::auth::extract_user_id(&headers, &state.jwt_secret);
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_visible(&state.db, &org, viewer).await {
        return e.into_response();
    }
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
    headers: HeaderMap,
    Path((name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let actor_id = match require_user(&headers, &state.jwt_secret) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_admin(&state.db, &org, actor_id).await {
        return e.into_response();
    }
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };

    match rg_core::org::delete_team(&state.db, team.id).await {
        Ok(()) => Json(serde_json::json!({"deleted": true})).into_response(),
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
    headers: HeaderMap,
    Path((name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let viewer = super::auth::extract_user_id(&headers, &state.jwt_secret);
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_visible(&state.db, &org, viewer).await {
        return e.into_response();
    }
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };

    match rg_core::org::list_team_members(&state.db, team.id).await {
        Ok(members) => {
            let resp: Vec<TeamMemberResponse> = members
                .into_iter()
                .map(|m| TeamMemberResponse {
                    id: m.id,
                    team_id: m.team_id,
                    user_id: m.user_id,
                    role: m.role,
                    created_at: m.created_at.to_string(),
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
    OrgAdmin { org, .. }: OrgAdmin,
    Path((_name, team_id)): Path<(String, i64)>,
    Json(body): Json<AddTeamMemberRequest>,
) -> impl IntoResponse {
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };

    let role = body.role.as_deref().unwrap_or("member");

    match rg_core::org::add_team_member(&state.db, team.id, body.user_id, role).await {
        Ok(m) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": m.id,
                "team_id": m.team_id,
                "user_id": m.user_id,
                "role": m.role,
            })),
        )
            .into_response(),
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
    ),
)]
pub async fn remove_team_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((name, team_id, user_id)): Path<(String, i64, i64)>,
) -> impl IntoResponse {
    let actor_id = match require_user(&headers, &state.jwt_secret) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    let org = match resolve_org(&state.db, &name).await {
        Ok(org) => org,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_org_admin(&state.db, &org, actor_id).await {
        return e.into_response();
    }
    let team = match resolve_team_in_org(&state.db, org.id, team_id).await {
        Ok(team) => team,
        Err(e) => return e.into_response(),
    };

    match rg_core::org::remove_team_member(&state.db, team.id, user_id).await {
        Ok(()) => Json(serde_json::json!({"removed": true})).into_response(),
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

/// Whether `user_id` may administer `org`: its owner, or a member holding the
/// `owner` / `admin` role.
async fn is_org_admin(
    db: &sea_orm::DatabaseConnection,
    org: &rg_db::entities::organization::Model,
    user_id: i64,
) -> anyhow::Result<bool> {
    if org.owner_id == user_id {
        return Ok(true);
    }
    Ok(rg_core::org::find_org_member(db, org.id, user_id)
        .await?
        .is_some_and(|m| m.role == "owner" || m.role == "admin"))
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

/// Read gate for org-scoped data. A public org is world-readable; a private one
/// answers `404` — not `403` — to everyone outside it, so the endpoint cannot be
/// used to enumerate private organizations by name.
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
    if org.owner_id == user_id {
        return Ok(());
    }
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

/// An authenticated administrator of the organization named by `{name}`.
///
/// Same three steps, same order, same answers as the hand-written prologue it
/// replaces: no session is `401`, an unknown organization is `404`, and a
/// caller who is neither owner nor admin is `403`.
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
        require_org_admin(&state.db, &org, actor_id).await?;
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
