//! Admin REST API handlers.
//!
//! All endpoints require is_admin=true on the authenticated user.
//!
//! GET    /api/v1/admin/users          -- list all users (paginated)
//! GET    /api/v1/admin/users/:id      -- get a single user
//! PATCH  /api/v1/admin/users/:id      -- update user (display_name, bio, is_admin, is_active)
//! DELETE /api/v1/admin/users/:id      -- delete a user
//! GET    /api/v1/admin/orgs           -- list all organizations
//! GET    /api/v1/admin/orgs/:name     -- get an organization
//! DELETE /api/v1/admin/orgs/:name     -- delete an organization

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use super::auth::extract_user_id;
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
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

// ── Request / Response types ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub is_admin: Option<bool>,
    pub is_active: Option<bool>,
}

// ── Admin middleware: require is_admin ────────────────────────────────

/// Extract the current user ID from cookie or Bearer token and verify is_admin=true.
/// Returns None if not authenticated or not an admin.
pub(crate) async fn require_admin(state: &AppState, headers: &HeaderMap) -> Option<i64> {
    let user_id = extract_user_id(headers, &state.jwt_secret)?;
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .ok()??;
    if user.is_admin {
        Some(user_id)
    } else {
        None
    }
}

// ── User management endpoints ─────────────────────────────────────────

/// GET /api/v1/admin/users
#[utoipa::path(
    get,
    path = "/admin/users",
    tag = "Admin",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    let params = params.clamp();
    match rg_core::user::service::list_users_admin(&state.db, params.offset(), params.limit()).await
    {
        Ok(paginated) => {
            let resp = PaginatedResponse::new(paginated.users, &params, paginated.total as u64);
            (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
        }
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

/// GET /api/v1/admin/users/:id
#[utoipa::path(
    get,
    path = "/admin/users/{id}",
    tag = "Admin",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    match rg_core::user::service::get_user_by_id(&state.db, user_id).await {
        Ok(Some(user)) => (StatusCode::OK, Json(serde_json::json!(user))).into_response(),
        Ok(None) => AppError::not_found("user not found").into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

/// PATCH /api/v1/admin/users/:id
#[utoipa::path(
    patch,
    path = "/admin/users/{id}",
    tag = "Admin",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(body): Json<UpdateUserRequest>,
) -> impl IntoResponse {
    let current_id = match require_admin(&state, &headers).await {
        Some(id) => id,
        None => return AppError::forbidden("admin required").into_response(),
    };
    let display_name_for_audit = body.display_name.clone();
    let display_name = body.display_name.map(Some);
    let bio = body.bio.map(Some);
    let is_admin = body.is_admin;
    let is_active = body.is_active;
    match rg_core::user::service::update_user_admin(
        &state.db,
        user_id,
        display_name,
        bio,
        is_admin,
        is_active,
    )
    .await
    {
        Ok(user) => {
            let details = serde_json::json!({
                "target_user_id": user_id,
                "display_name": display_name_for_audit,
                "is_admin": is_admin,
                "is_active": is_active
            });
            record_audit(
                &state.db,
                current_id,
                "",
                "admin.update_user",
                Some("user"),
                Some(user_id),
                Some(user.username.as_str()),
                &headers,
                Some(details),
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!(user))).into_response()
        }
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

/// POST /api/v1/admin/users/:id/unlock
#[utoipa::path(
    post,
    path = "/admin/users/{id}/unlock",
    tag = "Admin",
    params(("id" = i64, Path, description = "User ID")),
    responses(
        (status = 200, description = "Login failures and lock cleared", body = serde_json::Value),
        (status = 403, description = "Admin required", body = serde_json::Value),
        (status = 404, description = "User not found", body = serde_json::Value),
    ),
)]
pub async fn unlock_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let current_id = match require_admin(&state, &headers).await {
        Some(id) => id,
        None => return AppError::forbidden("admin required").into_response(),
    };
    let target = match rg_db::ops::user_ops::find_by_id(&state.db, user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return AppError::not_found("user not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let actor_username = rg_db::ops::user_ops::find_by_id(&state.db, current_id)
        .await
        .ok()
        .flatten()
        .map(|user| user.username)
        .unwrap_or_default();
    match rg_db::ops::user_ops::reset_login_failures(&state.db, user_id).await {
        Ok(updated) => {
            record_audit(
                &state.db,
                current_id,
                &actor_username,
                "admin.unlock_user",
                Some("user"),
                Some(user_id),
                Some(&target.username),
                &headers,
                Some(serde_json::json!({
                    "previous_login_attempts": target.login_attempts,
                    "previous_locked_until": target.locked_until,
                })),
            )
            .await;
            let response: rg_core::user::service::UserInfo = updated.into();
            (StatusCode::OK, Json(serde_json::json!(response))).into_response()
        }
        Err(error) => AppError::internal(error).into_response(),
    }
}

/// DELETE /api/v1/admin/users/:id
#[utoipa::path(
    delete,
    path = "/admin/users/{id}",
    tag = "Admin",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let current_id = match require_admin(&state, &headers).await {
        Some(id) => id,
        None => return AppError::forbidden("admin required").into_response(),
    };
    if current_id == user_id {
        return AppError::bad_request("cannot delete your own account").into_response();
    }
    match rg_core::user::service::delete_user(&state.db, user_id).await {
        Ok(()) => {
            let details = serde_json::json!({"deleted_user_id": user_id});
            record_audit(
                &state.db,
                current_id,
                "",
                "admin.delete_user",
                Some("user"),
                Some(user_id),
                None,
                &headers,
                Some(details),
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!({"deleted": true}))).into_response()
        }
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

// ── Organization management endpoints ────────────────────────────────

/// GET /api/v1/admin/orgs
#[utoipa::path(
    get,
    path = "/admin/orgs",
    tag = "Admin",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_orgs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    let params = params.clamp();
    match rg_db::ops::org_ops::list_all_orgs(&state.db, params.offset(), params.limit()).await {
        Ok((orgs, total)) => {
            let resp: Vec<_> = orgs.iter().map(org_response).collect();
            let page = PaginatedResponse::new(resp, &params, total as u64);
            (StatusCode::OK, Json(serde_json::to_value(page).unwrap())).into_response()
        }
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

/// GET /api/v1/admin/orgs/:name
#[utoipa::path(
    get,
    path = "/admin/orgs/{name}",
    tag = "Admin",
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
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(org)) => {
            (StatusCode::OK, Json(serde_json::json!(org_response(&org)))).into_response()
        }
        Ok(None) => AppError::not_found("organization not found").into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

/// DELETE /api/v1/admin/orgs/:name
#[utoipa::path(
    delete,
    path = "/admin/orgs/{name}",
    tag = "Admin",
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
    let current_id = match require_admin(&state, &headers).await {
        Some(id) => id,
        None => return AppError::forbidden("admin required").into_response(),
    };
    match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(org)) => match rg_core::org::delete_org(&state.db, org.id, org.id).await {
            Ok(()) => {
                let details = serde_json::json!({"org_name": org.name});
                record_audit(
                    &state.db,
                    current_id,
                    "",
                    "admin.delete_org",
                    Some("org"),
                    Some(org.id),
                    Some(&org.name),
                    &headers,
                    Some(details),
                )
                .await;
                (StatusCode::OK, Json(serde_json::json!({"deleted": true}))).into_response()
            }
            Err(e) => AppError::internal(e.to_string()).into_response(),
        },
        Ok(None) => AppError::not_found("organization not found").into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

// ── SSO Provider Management ─────────────────────────────────────

/// GET /api/v1/admin/sso/providers
#[utoipa::path(
    get,
    path = "/admin/sso/providers",
    tag = "Admin",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized"),
    ),
)]
pub async fn list_sso_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    match rg_db::ops::sso_provider_ops::list_all(&state.db).await {
        Ok(providers) => {
            let list: Vec<_> = providers.iter().map(sso_provider_response).collect();
            (StatusCode::OK, Json(serde_json::json!(list))).into_response()
        }
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

/// GET /api/v1/admin/sso/providers/{id}
#[utoipa::path(
    get,
    path = "/admin/sso/providers/{id}",
    tag = "Admin",
    params(("id" = i64, Path)),
    responses(
        (status = 200, description = "Success"),
        (status = 401, description = "Unauthorized"),
    ),
)]
pub async fn get_sso_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    match rg_db::ops::sso_provider_ops::find_by_id(&state.db, id).await {
        Ok(Some(p)) => (StatusCode::OK, Json(sso_provider_response(&p))).into_response(),
        Ok(None) => AppError::not_found("SSO provider not found").into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpsertSsoProviderRequest {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub provider_type: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub discovery_url: Option<String>,
    pub scopes: Option<String>,
    pub ldap_host: Option<String>,
    pub ldap_port: Option<i32>,
    pub ldap_bind_dn: Option<String>,
    pub ldap_bind_password: Option<String>,
    pub ldap_base_dn: Option<String>,
    pub ldap_user_filter: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    pub icon_url: Option<String>,
}

fn validate_ldap_provider_request(
    body: &UpsertSsoProviderRequest,
    has_stored_password: bool,
) -> Result<(), String> {
    if body.provider_type != "ldap" {
        return Ok(());
    }
    if body
        .ldap_port
        .is_some_and(|port| !(1..=65_535).contains(&port))
    {
        return Err("LDAP port must be between 1 and 65535".into());
    }
    if body
        .ldap_user_filter
        .as_deref()
        .is_some_and(|filter| !filter.contains("{username}"))
    {
        return Err("LDAP user filter must contain '{username}'".into());
    }
    if !body.enabled {
        return Ok(());
    }
    for (value, name) in [
        (body.ldap_host.as_deref(), "host"),
        (body.ldap_bind_dn.as_deref(), "bind DN"),
        (body.ldap_base_dn.as_deref(), "base DN"),
    ] {
        if value.is_none_or(|value| value.trim().is_empty()) {
            return Err(format!(
                "LDAP {name} is required when the provider is enabled"
            ));
        }
    }
    let supplied_password = body
        .ldap_bind_password
        .as_deref()
        .is_some_and(|password| !password.is_empty());
    if !has_stored_password && !supplied_password {
        return Err("LDAP bind password is required when the provider is enabled".into());
    }
    Ok(())
}

/// POST /api/v1/admin/sso/providers
#[utoipa::path(
    post,
    path = "/admin/sso/providers",
    tag = "Admin",
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created"),
        (status = 401, description = "Unauthorized"),
    ),
)]
pub async fn create_sso_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpsertSsoProviderRequest>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }

    let pt = if body.provider_type.is_empty() {
        "oauth2"
    } else {
        &body.provider_type
    };
    if let Err(error) = validate_ldap_provider_request(&body, false) {
        return AppError::bad_request(error).into_response();
    }

    // Encrypt secrets before storing
    let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let client_secret_enc = match body
        .client_secret
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| rg_core::auth::encryption::encrypt(s, &enc_key))
        .transpose()
    {
        Ok(secret) => secret,
        Err(error) => return AppError::internal(error).into_response(),
    };
    let ldap_password_enc = match body
        .ldap_bind_password
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| rg_core::auth::encryption::encrypt(s, &enc_key))
        .transpose()
    {
        Ok(secret) => secret,
        Err(error) => return AppError::internal(error).into_response(),
    };

    match rg_db::ops::sso_provider_ops::upsert(
        &state.db,
        None,
        &body.name,
        &body.slug,
        pt,
        body.client_id.as_deref(),
        client_secret_enc.as_deref(),
        body.discovery_url.as_deref(),
        body.scopes.as_deref(),
        body.ldap_host.as_deref(),
        body.ldap_port,
        body.ldap_bind_dn.as_deref(),
        ldap_password_enc.as_deref(),
        body.ldap_base_dn.as_deref(),
        body.ldap_user_filter.as_deref(),
        body.enabled,
        body.icon_url.as_deref(),
    )
    .await
    {
        Ok(provider) => {
            (StatusCode::CREATED, Json(sso_provider_response(&provider))).into_response()
        }
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

/// PATCH /api/v1/admin/sso/providers/{id}
#[utoipa::path(
    patch,
    path = "/admin/sso/providers/{id}",
    tag = "Admin",
    params(("id" = i64, Path)),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated"),
        (status = 401, description = "Unauthorized"),
    ),
)]
pub async fn update_sso_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<UpsertSsoProviderRequest>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }

    let pt = if body.provider_type.is_empty() {
        "oauth2"
    } else {
        &body.provider_type
    };

    let existing_provider = match rg_db::ops::sso_provider_ops::find_by_id(&state.db, id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => return AppError::not_found("SSO provider not found").into_response(),
        Err(e) => return AppError::internal(e.to_string()).into_response(),
    };
    if let Err(error) =
        validate_ldap_provider_request(&body, existing_provider.ldap_bind_password_enc.is_some())
    {
        return AppError::bad_request(error).into_response();
    }

    let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let client_secret_enc = match body
        .client_secret
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| rg_core::auth::encryption::encrypt(s, &enc_key))
        .transpose()
    {
        Ok(secret) => secret.or(existing_provider.client_secret_enc),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let ldap_password_enc = match body
        .ldap_bind_password
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| rg_core::auth::encryption::encrypt(s, &enc_key))
        .transpose()
    {
        Ok(secret) => secret.or(existing_provider.ldap_bind_password_enc),
        Err(error) => return AppError::internal(error).into_response(),
    };

    match rg_db::ops::sso_provider_ops::upsert(
        &state.db,
        Some(id),
        &body.name,
        &body.slug,
        pt,
        body.client_id.as_deref(),
        client_secret_enc.as_deref(),
        body.discovery_url.as_deref(),
        body.scopes.as_deref(),
        body.ldap_host.as_deref(),
        body.ldap_port,
        body.ldap_bind_dn.as_deref(),
        ldap_password_enc.as_deref(),
        body.ldap_base_dn.as_deref(),
        body.ldap_user_filter.as_deref(),
        body.enabled,
        body.icon_url.as_deref(),
    )
    .await
    {
        Ok(provider) => (StatusCode::OK, Json(sso_provider_response(&provider))).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

/// POST /api/v1/admin/sso/providers/{id}/test
#[utoipa::path(
    post,
    path = "/admin/sso/providers/{id}/test",
    tag = "Admin",
    params(("id" = i64, Path)),
    responses(
        (status = 200, description = "Provider connection succeeded", body = serde_json::Value),
        (status = 400, description = "Provider is not LDAP or connection failed", body = serde_json::Value),
        (status = 403, description = "Admin required", body = serde_json::Value),
    ),
)]
pub async fn test_sso_provider_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    let provider = match rg_db::ops::sso_provider_ops::find_by_id(&state.db, id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => return AppError::not_found("SSO provider not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    if provider.provider_type != "ldap" {
        return AppError::bad_request("connection testing is only supported for LDAP providers")
            .into_response();
    }
    match rg_core::user::service::test_ldap_provider_connection(&provider, &state.jwt_secret).await
    {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "message": "LDAP connection and service bind succeeded"
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(provider_id = provider.id, %error, "LDAP provider connection test failed");
            AppError::bad_request("LDAP connection test failed; check server logs for details")
                .into_response()
        }
    }
}

/// DELETE /api/v1/admin/sso/providers/{id}
#[utoipa::path(
    delete,
    path = "/admin/sso/providers/{id}",
    tag = "Admin",
    params(("id" = i64, Path)),
    responses(
        (status = 200, description = "Deleted"),
        (status = 401, description = "Unauthorized"),
    ),
)]
pub async fn delete_sso_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    let provider = match rg_db::ops::sso_provider_ops::find_by_id(&state.db, id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => return AppError::not_found("SSO provider not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let linked_identities = if provider.provider_type == "ldap" {
        rg_db::ops::user_ops::count_by_ldap_provider(&state.db, id).await
    } else {
        rg_db::ops::oauth_account_ops::count_by_provider(&state.db, &provider.slug)
            .await
            .map_err(anyhow::Error::from)
    };
    match linked_identities {
        Ok(0) => {}
        Ok(_) => {
            return AppError::bad_request(
                "provider has linked identities; disable it instead of deleting it",
            )
            .into_response();
        }
        Err(error) => return AppError::internal(error).into_response(),
    }
    match rg_db::ops::sso_provider_ops::delete_by_id(&state.db, id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"deleted": true}))).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

// ── SSO Helpers ──────────────────────────────────────────────────

fn sso_provider_response(p: &rg_db::entities::sso_provider::Model) -> serde_json::Value {
    serde_json::json!({
        "id": p.id,
        "name": p.name,
        "slug": p.slug,
        "provider_type": p.provider_type,
        "client_id": p.client_id,
        "discovery_url": p.discovery_url,
        "scopes": p.scopes,
        "ldap_host": p.ldap_host,
        "ldap_port": p.ldap_port,
        "ldap_bind_dn": p.ldap_bind_dn,
        "ldap_base_dn": p.ldap_base_dn,
        "ldap_user_filter": p.ldap_user_filter,
        "enabled": p.enabled,
        "icon_url": p.icon_url,
        "created_at": p.created_at.to_string(),
        "updated_at": p.updated_at.to_string(),
    })
}

/// GET /api/v1/admin/settings — returns current instance settings.
#[utoipa::path(
    get,
    path = "/admin/settings",
    tag = "Admin",
    responses(
        (status = 200, description = "Current instance settings", body = serde_json::Value),
        (status = 401, description = "Admin access required"),
    ),
)]
pub async fn get_settings(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    let settings = crate::instance::get_settings();
    (StatusCode::OK, Json(serde_json::json!(settings))).into_response()
}

/// PATCH /api/v1/admin/settings — update instance settings (maintenance mode, banner).
#[utoipa::path(
    patch,
    path = "/admin/settings",
    tag = "Admin",
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Settings updated", body = serde_json::Value),
        (status = 401, description = "Admin access required"),
    ),
)]
pub async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin required").into_response();
    }
    crate::instance::update_settings(|s| {
        if let Some(mm) = body.get("maintenance_mode").and_then(|v| v.as_bool()) {
            s.maintenance_mode = mm;
        }
        if let Some(msg) = body.get("banner_message") {
            s.banner_message = msg.as_str().filter(|m| !m.is_empty()).map(String::from);
        }
        if let Some(bt) = body.get("banner_type").and_then(|v| v.as_str()) {
            s.banner_type = bt.to_string();
        }
    });
    let settings = crate::instance::get_settings();
    (StatusCode::OK, Json(serde_json::json!(settings))).into_response()
}

// ── Helpers ──────────────────────────────────────────────────────────

fn org_response(org: &rg_db::entities::organization::Model) -> serde_json::Value {
    serde_json::json!({
        "id": org.id,
        "name": org.name,
        "display_name": org.display_name,
        "description": org.description,
        "owner_id": org.owner_id,
        "visibility": org.visibility,
        "created_at": org.created_at.to_string(),
        "updated_at": org.updated_at.to_string(),
    })
}
