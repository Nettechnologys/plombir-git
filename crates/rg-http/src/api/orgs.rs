//! REST API handlers for organizations and teams.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

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
        tracing::warn!(error = %e, "failed to record audit log");
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
    headers: HeaderMap,
    Json(body): Json<CreateOrgRequest>,
) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required").into_response();
        }
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
        Err(e) => AppError::bad_request(e).into_response(),
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
pub async fn get_org(State(state): State<AppState>, Path(name): Path<String>) -> impl IntoResponse {
    match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(org)) => Json(org_to_response(&org)).into_response(),
        Ok(None) => AppError::not_found("organization not found").into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
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
        Err(e) => AppError::internal(e).into_response(),
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
    ),
)]
pub async fn update_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<UpdateOrgRequest>,
) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required").into_response();
        }
    };
    let org = match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return AppError::not_found("organization not found").into_response();
        }
        Err(e) => {
            return AppError::internal(e).into_response();
        }
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
        Err(e) => AppError::bad_request(e).into_response(),
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
    ),
)]
pub async fn delete_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required").into_response();
        }
    };
    let org = match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return AppError::not_found("organization not found").into_response();
        }
        Err(e) => {
            return AppError::internal(e).into_response();
        }
    };

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
        Err(e) => AppError::forbidden(e).into_response(),
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
    Path(name): Path<String>,
) -> impl IntoResponse {
    let org = match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return AppError::not_found("organization not found").into_response();
        }
        Err(e) => {
            return AppError::internal(e).into_response();
        }
    };

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
        Err(e) => AppError::internal(e).into_response(),
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
    ),
)]
pub async fn add_org_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<AddOrgMemberRequest>,
) -> impl IntoResponse {
    let actor_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required").into_response();
        }
    };
    let org = match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return AppError::not_found("organization not found").into_response();
        }
        Err(e) => {
            return AppError::internal(e).into_response();
        }
    };

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
        Err(e) => AppError::bad_request(e).into_response(),
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
    ),
)]
pub async fn remove_org_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((name, user_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    let actor_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required").into_response();
        }
    };
    let org = match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return AppError::not_found("organization not found").into_response();
        }
        Err(e) => {
            return AppError::internal(e).into_response();
        }
    };

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
        Err(e) => AppError::bad_request(e).into_response(),
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
    ),
)]
pub async fn create_team(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<CreateTeamRequest>,
) -> impl IntoResponse {
    let org = match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return AppError::not_found("organization not found").into_response();
        }
        Err(e) => {
            return AppError::internal(e).into_response();
        }
    };

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
        Err(e) => AppError::bad_request(e).into_response(),
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
    Path(name): Path<String>,
) -> impl IntoResponse {
    let org = match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return AppError::not_found("organization not found").into_response();
        }
        Err(e) => {
            return AppError::internal(e).into_response();
        }
    };

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
        Err(e) => AppError::internal(e).into_response(),
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
    Path((_name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    match rg_core::org::get_team(&state.db, team_id).await {
        Ok(Some(t)) => Json(TeamResponse {
            id: t.id,
            org_id: t.org_id,
            name: t.name,
            description: t.description,
            permission: t.permission,
            created_at: t.created_at.to_string(),
            updated_at: t.updated_at.to_string(),
        })
        .into_response(),
        Ok(None) => AppError::not_found("team not found").into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
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
    ),
)]
pub async fn delete_team(
    State(state): State<AppState>,
    Path((_name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    match rg_core::org::delete_team(&state.db, team_id).await {
        Ok(()) => Json(serde_json::json!({"deleted": true})).into_response(),
        Err(e) => AppError::not_found(e).into_response(),
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
    Path((_name, team_id)): Path<(String, i64)>,
) -> impl IntoResponse {
    match rg_core::org::list_team_members(&state.db, team_id).await {
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
        Err(e) => AppError::internal(e).into_response(),
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
    ),
)]
pub async fn add_team_member(
    State(state): State<AppState>,
    Path((_name, team_id)): Path<(String, i64)>,
    Json(body): Json<AddTeamMemberRequest>,
) -> impl IntoResponse {
    let role = body.role.as_deref().unwrap_or("member");

    match rg_core::org::add_team_member(&state.db, team_id, body.user_id, role).await {
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
        Err(e) => AppError::bad_request(e).into_response(),
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
    ),
)]
pub async fn remove_team_member(
    State(state): State<AppState>,
    Path((_name, team_id, user_id)): Path<(String, i64, i64)>,
) -> impl IntoResponse {
    match rg_core::org::remove_team_member(&state.db, team_id, user_id).await {
        Ok(()) => Json(serde_json::json!({"removed": true})).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

// ── Helpers ──────────────────────────────────────────────────

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
