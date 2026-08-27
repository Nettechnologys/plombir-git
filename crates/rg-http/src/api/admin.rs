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

use super::access_audit::{grant_actor, record_instance_credential, InstanceResource};
use super::auth::extract_user_id;
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

// ── Request / Response types ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    // `null` clears the field; an absent key leaves it alone. See
    // `crate::api::clearable` for why the attribute is load-bearing.
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub bio: Option<Option<String>>,
    pub is_admin: Option<bool>,
    pub is_active: Option<bool>,
}

// ── Admin middleware: require is_admin ────────────────────────────────

/// Extract the current user ID from cookie or Bearer token and verify is_admin=true.
///
/// Missing authentication, a negative admin decision and a failed database
/// lookup are deliberately three different outcomes: the first is `401`, the
/// second is `403`, and the third remains a server-side error.
///
/// Private to this module on purpose: the rule is reachable from a handler only
/// through [`InstanceAdmin`], so "which extractor gates this route" is a
/// question the compiler answers. A handler in another module cannot call the
/// rule from inside its own body even if it wants to — which is the only way
/// the ordering below stays true for handlers nobody has written yet.
///
/// The visibility is load-bearing rather than tidy, so it is asserted:
/// `authz_extractor_guard::the_instance_admin_gate_is_module_private` is what
/// fails if a `pub` lands here. It is also why this name is absent from that
/// file's `GATES` list — the compiler covers what the grep there would only
/// notice, and widening this signature means bringing the name under a grep in
/// the same commit.
async fn require_instance_admin(state: &AppState, headers: &HeaderMap) -> Result<i64, AppError> {
    let user_id = extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::unauthorized("invalid token subject"))?;
    if user.is_admin {
        Ok(user_id)
    } else {
        Err(AppError::forbidden("admin required"))
    }
}

/// The instance-admin gate as a handler *argument*.
///
/// [`require_instance_admin`] states the rule; this states *where* it runs. A
/// handler that also takes a body used to call the rule from inside its own
/// body, which put it behind axum's `Json<_>` extractor: an anonymous caller
/// was told its JSON was malformed instead of being turned away, because
/// `FromRequest` runs after every `FromRequestParts`. Taking the gate as an
/// argument puts it back in front of the body — and in front of the schema
/// oracle a rejected body hands out.
///
/// The body is not the only extractor that overtakes an in-body gate. `Query<_>`
/// and `Path<_>` are `FromRequestParts` too, and axum runs the handler's
/// arguments left to right: a gate written as the first statement of the
/// function body still runs *after* every one of them. `GET /admin/users?per_page=abc`
/// answered an anonymous caller `400` with the serde error naming the parameter
/// and its type, rather than `401`. Declaring the gate as the argument before
/// them is what puts it first.
///
/// Rejections preserve the gate's actual outcome: no session is `401`, a
/// non-admin is `403`, and a failed account lookup is a server error.
pub struct InstanceAdmin(pub i64);

impl axum::extract::FromRequestParts<AppState> for InstanceAdmin {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        require_instance_admin(state, &parts.headers)
            .await
            .map(Self)
    }
}

// ── User management endpoints ─────────────────────────────────────────

/// GET /api/v1/admin/users
#[utoipa::path(
    get,
    path = "/admin/users",
    tag = "Admin",
    params(PaginationParams),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_users(
    State(state): State<AppState>,
    _admin: InstanceAdmin,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    let params = params.clamp();
    match rg_core::user::service::list_users_admin(&state.db, params.offset(), params.limit()).await
    {
        Ok(paginated) => {
            let resp = PaginatedResponse::new(paginated.users, &params, paginated.total as u64);
            (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
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
    _admin: InstanceAdmin,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    match rg_core::user::service::get_user_by_id(&state.db, user_id).await {
        Ok(Some(user)) => (StatusCode::OK, Json(serde_json::json!(user))).into_response(),
        Ok(None) => AppError::not_found("user not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
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
    InstanceAdmin(current_id): InstanceAdmin,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(body): Json<UpdateUserRequest>,
) -> impl IntoResponse {
    // The actor column used to get `""` here: an admin action with a known
    // `user_id` and a blank author, which `/admin/audit` renders as a blank
    // actor — indistinguishable from a name that failed to load
    // (card_fcc07f8d1505). Resolved before the mutation, so a database failure
    // refuses the request rather than recording an anonymous one.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, current_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    let display_name_for_audit = body.display_name.clone();
    // Passed through as they arrived: `map(Some)` used to fold an explicit
    // `null` into "leave it alone", because serde had already collapsed it into
    // the absent-field `None` one layer up (card_a156a521ca3b).
    let display_name = body.display_name;
    let bio = body.bio;
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
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "admin.update_user",
                Some("user"),
                Some(user_id),
                Some(user.username.as_str()),
                Some(&headers),
                Some(details),
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!(user))).into_response()
        }
        // The service reports an unknown target user as `NotFound`, so that is a
        // 404 here; the update itself is ours and a failed one is a 5xx. Both used
        // to answer 400 — with the raw `db: update user by admin` chain in the
        // body (H-05).
        Err(e) => AppError::from(e).into_response(),
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
    InstanceAdmin(current_id): InstanceAdmin,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let target = match rg_db::ops::user_ops::find_by_id(&state.db, user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return AppError::not_found("user not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    // Resolved before the reset, and with the failure propagated. This used to
    // be `.ok().flatten().unwrap_or_default()`, which folded "the query failed"
    // and "no such account" into `""` — and then reset the target's login
    // failures anyway, leaving the only record of who unlocked an account with a
    // blank author that reads as entirely routine
    // (card_86f40189bc71, card_e2bd7026c87d). The target above is resolved with
    // the three answers kept apart; the actor gets the same treatment.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, current_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_db::ops::user_ops::reset_login_failures(&state.db, user_id).await {
        Ok(updated) => {
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "admin.unlock_user",
                Some("user"),
                Some(user_id),
                Some(&target.username),
                Some(&headers),
                Some(serde_json::json!({
                    "previous_login_attempts": target.login_attempts,
                    "previous_locked_until": target.locked_until,
                })),
            )
            .await;
            let response: rg_core::user::service::UserInfo = updated.into();
            (StatusCode::OK, Json(serde_json::json!(response))).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
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
        (status = 400, description = "Cannot delete the current account", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Admin required", body = serde_json::Value),
        (status = 404, description = "User not found", body = serde_json::Value),
        (status = 409, description = "Account still owns organizations or organization repositories", body = serde_json::Value),
    ),
)]
pub async fn delete_user(
    State(state): State<AppState>,
    InstanceAdmin(current_id): InstanceAdmin,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    if current_id == user_id {
        return AppError::bad_request("cannot delete your own account").into_response();
    }
    // The actor column used to get `""` here: an admin action with a known
    // `user_id` and a blank author, which `/admin/audit` renders as a blank
    // actor — indistinguishable from a name that failed to load
    // (card_fcc07f8d1505). Resolved before the mutation, so a database failure
    // refuses the request rather than recording an anonymous one.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, current_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    // The account's repositories are deleted with it, so this needs the same
    // three storage handles a routed repository deletion does — the row alone
    // is not what the account owns.
    match rg_core::user::service::delete_user(
        &state.db,
        &state.repo_root,
        state.blob_storage.as_ref(),
        state.oci_storage.as_ref(),
        user_id,
    )
    .await
    {
        Ok(()) => {
            let details = serde_json::json!({"deleted_user_id": user_id});
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "admin.delete_user",
                Some("user"),
                Some(user_id),
                None,
                Some(&headers),
                Some(details),
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!({"deleted": true}))).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Organization management endpoints ────────────────────────────────

/// GET /api/v1/admin/orgs
#[utoipa::path(
    get,
    path = "/admin/orgs",
    tag = "Admin",
    params(PaginationParams),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_orgs(
    State(state): State<AppState>,
    _admin: InstanceAdmin,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    let params = params.clamp();
    match rg_db::ops::org_ops::list_all_orgs(&state.db, params.offset(), params.limit()).await {
        Ok((orgs, total)) => {
            // One round-trip for the page, not one per row.
            let owner_ids: Vec<i64> = orgs.iter().map(|org| org.owner_id).collect();
            let owners = match crate::api::user_ref::accounts_by_id(&state.db, &owner_ids).await {
                Ok(owners) => owners,
                Err(e) => return AppError::from(e).into_response(),
            };
            let resp: Vec<_> = orgs
                .iter()
                .map(|org| org_response(org, owners.get(&org.owner_id)))
                .collect();
            let page = PaginatedResponse::new(resp, &params, total as u64);
            (StatusCode::OK, Json(serde_json::to_value(page).unwrap())).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
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
    _admin: InstanceAdmin,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match rg_core::org::get_org_by_name(&state.db, &name).await {
        Ok(Some(org)) => {
            let owner = match rg_db::ops::user_ops::find_by_id(&state.db, org.owner_id).await {
                Ok(owner) => owner,
                Err(e) => return AppError::from(e).into_response(),
            };
            (
                StatusCode::OK,
                Json(serde_json::json!(org_response(&org, owner.as_ref()))),
            )
                .into_response()
        }
        Ok(None) => AppError::not_found("organization not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
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
    InstanceAdmin(current_id): InstanceAdmin,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    // The actor column used to get `""` here: an admin action with a known
    // `user_id` and a blank author, which `/admin/audit` renders as a blank
    // actor — indistinguishable from a name that failed to load
    // (card_fcc07f8d1505). Resolved before the mutation, so a database failure
    // refuses the request rather than recording an anonymous one.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, current_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::org::get_org_by_name(&state.db, &name).await {
        // Instance admins delete organizations they do not own — that is the
        // point of the route, and `InstanceAdmin` above is the gate that says
        // so. Naming the actor keeps it a decision instead of the accident it
        // was: this call used to pass `org.id` into the actor position, and it
        // only worked while the owner's user id and the org id happened to
        // match.
        Ok(Some(org)) => match rg_core::org::delete_org(
            &state.db,
            &state.repo_root,
            state.blob_storage.as_ref(),
            state.oci_storage.as_ref(),
            org.id,
            rg_core::org::OrgDeleteActor::InstanceAdmin,
        )
        .await
        {
            Ok(()) => {
                let details = serde_json::json!({"org_name": org.name});
                rg_core::audit::record(
                    &state.db,
                    &audit_actor,
                    "admin.delete_org",
                    Some("org"),
                    Some(org.id),
                    Some(&org.name),
                    Some(&headers),
                    Some(details),
                )
                .await;
                (StatusCode::OK, Json(serde_json::json!({"deleted": true}))).into_response()
            }
            Err(e) => AppError::from(e).into_response(),
        },
        Ok(None) => AppError::not_found("organization not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
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
    _admin: InstanceAdmin,
) -> impl IntoResponse {
    match rg_db::ops::sso_provider_ops::list_all(&state.db).await {
        Ok(providers) => {
            let list: Vec<_> = providers.iter().map(sso_provider_response).collect();
            (StatusCode::OK, Json(serde_json::json!(list))).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
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
    _admin: InstanceAdmin,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match rg_db::ops::sso_provider_ops::find_by_id(&state.db, id).await {
        Ok(Some(p)) => (StatusCode::OK, Json(sso_provider_response(&p))).into_response(),
        Ok(None) => AppError::not_found("SSO provider not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
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
    /// May a first login through this provider create an account?
    ///
    /// `Option` on purpose, and the two handlers read the absence differently:
    /// a **create** without it starts the provider at `false`, so "this
    /// provider may hand out accounts" is something an operator states rather
    /// than inherits; an **update** without it keeps whatever the provider has,
    /// so a client that predates the field cannot switch a working directory
    /// off by not mentioning it.
    pub auto_provision: Option<bool>,
    /// Comma-separated email domains this provider may provision accounts for.
    /// Absent leaves the stored list alone; an empty string clears it.
    pub allowed_email_domains: Option<String>,
    pub icon_url: Option<String>,
}

/// Refuse a provider the login page would offer and no one could log in through.
///
/// The completeness question used to be asked for LDAP only: the validator's
/// first line was `if body.provider_type != "ldap" { return Ok(()) }`, so the
/// OAuth2/OIDC half of the very same form could be saved *enabled* with no
/// `client_id` at all. The API answered `201`, the provider appeared on the
/// login page, and the first login went out with `client_id=""` — leaving the
/// operator reading the IdP's refusal as the IdP's fault, with nothing in our
/// own log to say a field was empty.
///
/// Both families answer here now, and an unknown `provider_type` is a bad
/// request rather than a row that silently behaves like `oauth2`.
///
/// `provider_type` is the *effective* type (the handlers default an empty one
/// to `oauth2`), not `body.provider_type`.
fn validate_sso_provider_request(
    body: &UpsertSsoProviderRequest,
    provider_type: &str,
    has_stored_ldap_password: bool,
) -> Result<(), String> {
    match provider_type {
        "ldap" => validate_ldap_provider_request(body, has_stored_ldap_password),
        "oauth2" | "oidc" => validate_oauth2_provider_request(body, provider_type),
        other => Err(format!(
            "unknown SSO provider type '{other}': expected one of oauth2, oidc, ldap"
        )),
    }
}

/// The OAuth2/OIDC half of [`validate_sso_provider_request`].
///
/// A disabled provider is a draft an operator is still filling in, exactly as
/// on the LDAP side — the completeness questions start once it is switched on.
///
/// The client *secret* is deliberately not required: a public PKCE client
/// legitimately has none, and `provider_config` already treats a stored secret
/// that will not decrypt as our 500. A missing `client_id` has no such
/// legitimate reading.
fn validate_oauth2_provider_request(
    body: &UpsertSsoProviderRequest,
    provider_type: &str,
) -> Result<(), String> {
    if !body.enabled {
        return Ok(());
    }
    if body
        .client_id
        .as_deref()
        .is_none_or(|client_id| client_id.trim().is_empty())
    {
        return Err("client ID is required when the provider is enabled".into());
    }
    if !rg_core::auth::sso::has_resolvable_endpoints(
        provider_type,
        &body.slug,
        body.discovery_url.as_deref(),
    ) {
        return Err(if provider_type == "oidc" {
            format!(
                "OIDC provider '{}' has no built-in endpoints: a discovery URL is required when the provider is enabled",
                body.slug
            )
        } else {
            format!(
                "no built-in OAuth2 endpoints for slug '{}': use provider type 'oidc' with a discovery URL",
                body.slug
            )
        });
    }
    Ok(())
}

fn validate_ldap_provider_request(
    body: &UpsertSsoProviderRequest,
    has_stored_password: bool,
) -> Result<(), String> {
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
        (status = 400, description = "Incomplete or unknown provider configuration"),
        (status = 401, description = "Unauthorized"),
        (status = 409, description = "A provider with that slug already exists"),
    ),
)]
pub async fn create_sso_provider(
    State(state): State<AppState>,
    InstanceAdmin(admin_id): InstanceAdmin,
    headers: HeaderMap,
    Json(body): Json<UpsertSsoProviderRequest>,
) -> impl IntoResponse {
    let pt = if body.provider_type.is_empty() {
        "oauth2"
    } else {
        &body.provider_type
    };
    if let Err(error) = validate_sso_provider_request(&body, pt, false) {
        return AppError::bad_request(error).into_response();
    }
    let allowed_email_domains = match body
        .allowed_email_domains
        .as_deref()
        .map(rg_core::user::provisioning::normalize_email_domains)
        .transpose()
    {
        Ok(domains) => domains.flatten(),
        Err(error) => return AppError::bad_request(error).into_response(),
    };

    // `sso_providers.slug` is UNIQUE, and the insert below is the only place that
    // ever noticed: the constraint violation came back as a `DbErr` that the
    // handler relabelled a bad request, raw text and all. Check it here so a
    // duplicate slug says so, and the insert's own failures are free to be the
    // 5xx they are.
    //
    // `conflict`, not `bad_request`: the body is valid and an existing provider
    // holds the slug. The admin renames one of the two or deletes the other —
    // there is nothing in the request to fix.
    match rg_db::ops::sso_provider_ops::find_by_slug(&state.db, &body.slug).await {
        Ok(Some(_)) => {
            return AppError::conflict(format!(
                "an SSO provider with slug '{}' already exists",
                body.slug
            ))
            .into_response();
        }
        Ok(None) => {}
        Err(error) => return AppError::from(error).into_response(),
    }

    // Encrypt secrets before storing
    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let client_secret_enc = match body
        .client_secret
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| rg_core::auth::encryption::encrypt(s, &enc_key))
        .transpose()
    {
        Ok(secret) => secret,
        Err(error) => return AppError::from(error).into_response(),
    };
    let ldap_password_enc = match body
        .ldap_bind_password
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| rg_core::auth::encryption::encrypt(s, &enc_key))
        .transpose()
    {
        Ok(secret) => secret,
        Err(error) => return AppError::from(error).into_response(),
    };

    // Before the row exists, per the rule in `access_audit`: a failed name
    // lookup afterwards would leave the instance's login door with a blank
    // author, and this way it is a 5xx from a request that stored nothing.
    let actor = match grant_actor(&state, admin_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };

    match rg_db::ops::sso_provider_ops::create(
        &state.db,
        rg_db::ops::sso_provider_ops::SsoProviderInput {
            name: &body.name,
            slug: &body.slug,
            provider_type: pt,
            client_id: body.client_id.as_deref(),
            client_secret_enc: client_secret_enc.as_deref(),
            discovery_url: body.discovery_url.as_deref(),
            scopes: body.scopes.as_deref(),
            ldap_host: body.ldap_host.as_deref(),
            ldap_port: body.ldap_port,
            ldap_bind_dn: body.ldap_bind_dn.as_deref(),
            ldap_bind_password_enc: ldap_password_enc.as_deref(),
            ldap_base_dn: body.ldap_base_dn.as_deref(),
            ldap_user_filter: body.ldap_user_filter.as_deref(),
            enabled: body.enabled,
            // A brand-new provider provisions nobody until someone says
            // otherwise. The migration default is the opposite (`true`) for the
            // opposite reason: it must not change how a running instance
            // behaves, while a provider being wired up right now has an
            // operator present to answer the question.
            auto_provision: body.auto_provision.unwrap_or(false),
            allowed_email_domains: allowed_email_domains.as_deref(),
            icon_url: body.icon_url.as_deref(),
        },
    )
    .await
    {
        Ok(provider) => {
            record_instance_credential(
                &state,
                &actor,
                "admin.create_sso_provider",
                InstanceResource {
                    kind: "sso_provider",
                    id: provider.id,
                    name: &provider.slug,
                },
                &headers,
                sso_provider_audit_details(&provider, client_secret_enc.is_some(), None),
            )
            .await;
            (StatusCode::CREATED, Json(sso_provider_response(&provider))).into_response()
        }
        // A concurrent request can cross the pre-check above and lose the
        // UNIQUE race here. It is the same outcome in the same words — a code
        // of its own would make the response a side channel for "you lost the
        // race". Every other database failure remains a 5xx through the normal
        // error funnel.
        Err(error) if rg_db::is_unique_violation(&error) => AppError::conflict(format!(
            "an SSO provider with slug '{}' already exists",
            body.slug
        ))
        .into_response(),
        // Everything the caller could get wrong was checked above, so what is
        // left is the insert: a dead pool is a retryable 503 and a statement
        // failure a 500, neither of them the admin's bad request.
        Err(e) => AppError::from(e).into_response(),
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
        (status = 400, description = "Incomplete or unknown provider configuration"),
        (status = 401, description = "Unauthorized"),
        (status = 409, description = "Another provider already holds that slug"),
    ),
)]
pub async fn update_sso_provider(
    State(state): State<AppState>,
    InstanceAdmin(admin_id): InstanceAdmin,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<UpsertSsoProviderRequest>,
) -> impl IntoResponse {
    let pt = if body.provider_type.is_empty() {
        "oauth2"
    } else {
        &body.provider_type
    };

    let existing_provider = match rg_db::ops::sso_provider_ops::find_by_id(&state.db, id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => return AppError::not_found("SSO provider not found").into_response(),
        Err(e) => return AppError::from(e).into_response(),
    };
    if let Err(error) = validate_sso_provider_request(
        &body,
        pt,
        existing_provider.ldap_bind_password_enc.is_some(),
    ) {
        return AppError::bad_request(error).into_response();
    }
    let existing_auto_provision = existing_provider.auto_provision;
    // Absent means "leave the policy alone", present-but-empty means "clear the
    // allowlist". A client that has never heard of the field must not be able
    // to widen who this provider provisions for by staying silent about it.
    let allowed_email_domains = match body.allowed_email_domains.as_deref() {
        Some(raw) => match rg_core::user::provisioning::normalize_email_domains(raw) {
            Ok(domains) => domains,
            Err(error) => return AppError::bad_request(error).into_response(),
        },
        None => existing_provider.allowed_email_domains.clone(),
    };

    // Same UNIQUE constraint as on create — but here the row may legitimately
    // keep its own slug, so only a *different* provider holding it is a conflict.
    if body.slug != existing_provider.slug {
        match rg_db::ops::sso_provider_ops::find_by_slug(&state.db, &body.slug).await {
            Ok(Some(_)) => {
                return AppError::conflict(format!(
                    "an SSO provider with slug '{}' already exists",
                    body.slug
                ))
                .into_response();
            }
            Ok(None) => {}
            Err(error) => return AppError::from(error).into_response(),
        }
    }

    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let client_secret_enc = match body
        .client_secret
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| rg_core::auth::encryption::encrypt(s, &enc_key))
        .transpose()
    {
        Ok(secret) => secret.or(existing_provider.client_secret_enc),
        Err(error) => return AppError::from(error).into_response(),
    };
    let ldap_password_enc = match body
        .ldap_bind_password
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| rg_core::auth::encryption::encrypt(s, &enc_key))
        .transpose()
    {
        Ok(secret) => secret.or(existing_provider.ldap_bind_password_enc),
        Err(error) => return AppError::from(error).into_response(),
    };

    // Read off the REQUEST, not off the row: AES-GCM ciphertext differs on
    // every write of the same value, so a before/after comparison of the stored
    // column reports "replaced" for an edit that touched nothing
    // (card_c0a0339b7191). The request is the only place the intent is legible.
    let replaced = SsoSecretsReplaced {
        client_secret: body.client_secret.as_ref().is_some_and(|s| !s.is_empty()),
        ldap_bind_password: body
            .ldap_bind_password
            .as_ref()
            .is_some_and(|s| !s.is_empty()),
    };
    let stores_client_secret = client_secret_enc.is_some();

    let actor = match grant_actor(&state, admin_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };

    match rg_db::ops::sso_provider_ops::update_settings(
        &state.db,
        id,
        rg_db::ops::sso_provider_ops::SsoProviderInput {
            name: &body.name,
            slug: &body.slug,
            provider_type: pt,
            client_id: body.client_id.as_deref(),
            client_secret_enc: client_secret_enc.as_deref(),
            discovery_url: body.discovery_url.as_deref(),
            scopes: body.scopes.as_deref(),
            ldap_host: body.ldap_host.as_deref(),
            ldap_port: body.ldap_port,
            ldap_bind_dn: body.ldap_bind_dn.as_deref(),
            ldap_bind_password_enc: ldap_password_enc.as_deref(),
            ldap_base_dn: body.ldap_base_dn.as_deref(),
            ldap_user_filter: body.ldap_user_filter.as_deref(),
            enabled: body.enabled,
            auto_provision: body.auto_provision.unwrap_or(existing_auto_provision),
            allowed_email_domains: allowed_email_domains.as_deref(),
            icon_url: body.icon_url.as_deref(),
        },
    )
    .await
    {
        Ok(Some(provider)) => {
            record_instance_credential(
                &state,
                &actor,
                "admin.update_sso_provider",
                InstanceResource {
                    kind: "sso_provider",
                    id: provider.id,
                    name: &provider.slug,
                },
                &headers,
                sso_provider_audit_details(&provider, stores_client_secret, Some(replaced)),
            )
            .await;
            (StatusCode::OK, Json(sso_provider_response(&provider))).into_response()
        }
        // A concurrent delete removed the provider between the lookup above and
        // this write. The conditional UPDATE matched nothing and — unlike the
        // read-then-insert it replaced — put nothing back, so the resource is
        // simply gone: the same 404 the lookup itself would have produced a
        // moment earlier, and no audit line for a write that never landed.
        Ok(None) => AppError::not_found("SSO provider not found").into_response(),
        // The slug pre-check above is a separate statement, so a provider
        // created in the meantime can still take the name this PATCH is moving
        // to. Same outcome in the same words as on create; every other database
        // failure stays a 5xx through the normal error funnel.
        Err(error) if rg_db::is_unique_violation(&error) => AppError::conflict(format!(
            "an SSO provider with slug '{}' already exists",
            body.slug
        ))
        .into_response(),
        // The provider was resolved and the request validated above; the update
        // itself is ours, so its failures are a 5xx.
        Err(e) => AppError::from(e).into_response(),
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
        (status = 400, description = "Provider is not LDAP, or its stored LDAP configuration is incomplete", body = serde_json::Value),
        (status = 403, description = "Admin required", body = serde_json::Value),
        (status = 404, description = "SSO provider not found", body = serde_json::Value),
        (status = 502, description = "The LDAP directory refused, was unreachable, or did not answer", body = serde_json::Value),
    ),
)]
pub async fn test_sso_provider_connection(
    State(state): State<AppState>,
    _admin: InstanceAdmin,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let provider = match rg_db::ops::sso_provider_ops::find_by_id(&state.db, id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => return AppError::not_found("SSO provider not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    if provider.provider_type != "ldap" {
        return AppError::bad_request("connection testing is only supported for LDAP providers")
            .into_response();
    }
    match rg_core::user::service::test_ldap_provider_connection(&provider, &state.encryption_key)
        .await
    {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "message": "LDAP connection and service bind succeeded"
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(
                provider_id = provider.id,
                error = %format!("{error:#}"),
                "LDAP provider connection test failed"
            );
            // The button was pressed to find out *which* of the two happened, so
            // it must not answer both with the same code: a row that cannot be
            // turned into a bindable config is the admin's form to fix (`400`),
            // a directory that refused or never answered is not (`502`). The
            // service tags each one, and this is the funnel that reads the tag.
            AppError::from(error).into_response()
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
    InstanceAdmin(admin_id): InstanceAdmin,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let provider = match rg_db::ops::sso_provider_ops::find_by_id(&state.db, id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => return AppError::not_found("SSO provider not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
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
        Err(error) => return AppError::from(error).into_response(),
    }
    let actor = match grant_actor(&state, admin_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };

    match rg_db::ops::sso_provider_ops::delete_by_id(&state.db, id).await {
        Ok(true) => {
            // Details read off the row before it went: `sso_provider #4 was
            // removed` names nothing an incident review can start from.
            record_instance_credential(
                &state,
                &actor,
                "admin.delete_sso_provider",
                InstanceResource {
                    kind: "sso_provider",
                    id: provider.id,
                    name: &provider.slug,
                },
                &headers,
                sso_provider_audit_details(&provider, provider.client_secret_enc.is_some(), None),
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!({"deleted": true}))).into_response()
        }
        // `{"deleted": true}` is a claim about what this request did, and the
        // lookup that found the provider is a statement of its own: a
        // concurrent delete can take the row in between. Only the request that
        // removed it may make the claim.
        Ok(false) => AppError::not_found("SSO provider not found").into_response(),
        // The two client-side outcomes (unknown provider, provider still linked)
        // were answered above; a failed DELETE is ours.
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── SSO Helpers ──────────────────────────────────────────────────

/// Which of the provider's two secrets this request replaced.
struct SsoSecretsReplaced {
    client_secret: bool,
    ldap_bind_password: bool,
}

/// What a journal entry about an SSO provider may say.
///
/// The sharp edge, and the reason this is one function rather than three
/// literals: `client_secret` is the door every account on this instance logs in
/// through, and `ldap_bind_password` is a read account in somebody else's
/// directory. Neither may appear here in any form — not the value, not the
/// ciphertext the row carries, not a hash of either — or the journal an
/// operator reads over the admin API becomes a second place to steal them
/// from. What may appear is what identifies the provider and whether a secret
/// is now set: an admin reading this row needs to tell "the login door was
/// re-pointed" from "somebody renamed it".
///
/// `replaced` is `Some` only for an update, where "the secret was replaced" and
/// "something else about this provider changed" are different events and the
/// row is the only place they are told apart.
fn sso_provider_audit_details(
    provider: &rg_db::entities::sso_provider::Model,
    has_client_secret: bool,
    replaced: Option<SsoSecretsReplaced>,
) -> serde_json::Value {
    let mut details = serde_json::json!({
        "slug": provider.slug,
        "name": provider.name,
        "provider_type": provider.provider_type,
        "enabled": provider.enabled,
        "auto_provision": provider.auto_provision,
        "has_client_secret": has_client_secret,
        "has_ldap_bind_password": provider.ldap_bind_password_enc.is_some(),
    });
    if let Some(replaced) = replaced {
        details["client_secret"] = if replaced.client_secret {
            "replaced".into()
        } else {
            "unchanged".into()
        };
        details["ldap_bind_password"] = if replaced.ldap_bind_password {
            "replaced".into()
        } else {
            "unchanged".into()
        };
    }
    details
}

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
        "auto_provision": p.auto_provision,
        "allowed_email_domains": p.allowed_email_domains,
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
pub async fn get_settings(
    State(state): State<AppState>,
    _admin: InstanceAdmin,
) -> impl IntoResponse {
    let settings = state.instance_settings.get(&state.db).await;
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
        (status = 500, description = "Settings could not be persisted"),
    ),
)]
pub async fn update_settings(
    State(state): State<AppState>,
    _admin: InstanceAdmin,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let updated = state
        .instance_settings
        .update(&state.db, |s| {
            if let Some(mm) = body.get("maintenance_mode").and_then(|v| v.as_bool()) {
                s.maintenance_mode = mm;
            }
            if let Some(msg) = body.get("banner_message") {
                s.banner_message = msg.as_str().filter(|m| !m.is_empty()).map(String::from);
            }
            if let Some(bt) = body.get("banner_type").and_then(|v| v.as_str()) {
                s.banner_type = bt.to_string();
            }
        })
        .await;

    // A settings change that cannot be stored has not happened. Reporting it as
    // applied is how maintenance mode used to survive exactly until the next
    // restart, so the failure is surfaced instead.
    match updated {
        Ok(settings) => (StatusCode::OK, Json(serde_json::json!(settings))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to persist instance settings");
            AppError::internal("failed to persist instance settings").into_response()
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

/// One organization as the admin surface reports it.
///
/// `owner` is the account name behind `owner_id`, resolved by the caller and
/// passed in. The column is headed "Owner" and used to render `#4`: an
/// instance admin *can* find out who that is, unlike the organization owner in
/// card_cb9f71672b11, but only by going to a second page and matching numbers
/// by hand (card_c6f108d0a896). `None` when the id resolves to nothing, which
/// the page renders as the number — an organization whose owner row is gone
/// still has to appear in the list.
fn org_response(
    org: &rg_db::entities::organization::Model,
    owner: Option<&rg_db::entities::user::Model>,
) -> serde_json::Value {
    serde_json::json!({
        "id": org.id,
        "name": org.name,
        "display_name": org.display_name,
        "description": org.description,
        "owner_id": org.owner_id,
        "owner_username": owner.map(|user| user.username.clone()),
        "owner_display_name": owner.and_then(|user| user.display_name.clone()),
        "visibility": org.visibility,
        "created_at": org.created_at.to_string(),
        "updated_at": org.updated_at.to_string(),
    })
}
