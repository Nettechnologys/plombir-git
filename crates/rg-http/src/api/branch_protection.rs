//! REST API handlers for branch protection rules.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::access_audit::{grant_actor, named_grant_list, record_grant};
use crate::api::repo_access::{RepoAdmin, RepoRead};
use crate::api::user_ref::{name_allow_list, resolve_allow_list, AllowedUser};
use crate::error::AppError;
use crate::AppState;

// ── Request / Response types ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateProtectionRequest {
    pub branch_name: String,
    #[serde(default)]
    pub require_pr: bool,
    #[serde(default)]
    pub require_status_check: bool,
    #[serde(default)]
    pub required_status_checks: Option<Vec<String>>,
    #[serde(default)]
    pub require_approval: bool,
    #[serde(default)]
    pub required_approvals: Option<i64>,
    #[serde(default)]
    pub allow_force_push: bool,
    #[serde(default)]
    pub require_signed_commits: bool,
    /// The direct-push exceptions as ids — what a client written before names
    /// were accepted still sends. See [`allowed_push_users`](Self::allowed_push_users).
    #[serde(default)]
    pub allowed_push_user_ids: Option<Vec<i64>>,
    /// The same exceptions, named: a `username` or a bare id (an e-mail is refused, see `user_ref`), one
    /// entry per person. This is the field the settings form fills, because the
    /// number the other one wants is not something the owner of a repository
    /// can look up anywhere on this instance.
    #[serde(default)]
    pub allowed_push_users: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct UpdateProtectionRequest {
    #[serde(default)]
    pub require_pr: Option<bool>,
    #[serde(default)]
    pub require_status_check: Option<bool>,
    #[serde(default)]
    pub required_status_checks: Option<Vec<String>>,
    #[serde(default)]
    pub require_approval: Option<bool>,
    #[serde(default)]
    pub required_approvals: Option<i64>,
    #[serde(default)]
    pub allow_force_push: Option<bool>,
    #[serde(default)]
    pub require_signed_commits: Option<bool>,
    /// See [`CreateProtectionRequest::allowed_push_user_ids`].
    #[serde(default)]
    pub allowed_push_user_ids: Option<Vec<i64>>,
    /// See [`CreateProtectionRequest::allowed_push_users`].
    #[serde(default)]
    pub allowed_push_users: Option<Vec<String>>,
}

/// A branch protection rule, with the people on its allow-list named.
///
/// The rule is flattened in whole, so `allowed_push_user_ids` — the stored JSON
/// mirror — keeps arriving exactly as it did; `allowed_push_users` is the field
/// a screen can render, and the one that ends the settings page's `42, 108`.
#[derive(Serialize)]
struct ProtectionResponse {
    #[serde(flatten)]
    rule: rg_db::entities::protected_branch::Model,
    allowed_push_users: Vec<AllowedUser>,
}

/// Name the allow-list of one rule.
///
/// The ids come from `load_verified`, not from the JSON column, so the page
/// shows the list the push gate actually enforces: the two are written in one
/// transaction and a disagreement between them is a broken row, which this
/// reports as the 5xx it is rather than rendering whichever copy it happened to
/// read.
async fn named_response(
    db: &rg_db::DatabaseConnection,
    rule: rg_db::entities::protected_branch::Model,
) -> anyhow::Result<ProtectionResponse> {
    let user_ids = rg_db::user_grants::load_verified(
        db,
        rg_db::user_grants::Target::ProtectedBranch(rule.id),
        rule.allowed_push_user_ids.as_deref(),
    )
    .await?;
    let allowed_push_users = name_allow_list(db, &user_ids).await?;
    Ok(ProtectionResponse {
        rule,
        allowed_push_users,
    })
}

/// What the journal records about a branch protection rule.
///
/// The allow-list is written out whole and by name rather than as the delta this
/// request carried: the question a reader brings to the journal is "who could
/// push to `main` on the 14th", and a list of edits only answers it after every
/// one of them has been replayed.
fn protection_details(response: &ProtectionResponse) -> serde_json::Value {
    serde_json::json!({
        "branch": response.rule.branch_name,
        "require_pr": response.rule.require_pr,
        "require_approval": response.rule.require_approval,
        "required_approvals": response.rule.required_approvals,
        "allow_force_push": response.rule.allow_force_push,
        "require_signed_commits": response.rule.require_signed_commits,
        "allowed_push_users": named_grant_list(&response.allowed_push_users),
    })
}

// ── Handlers ──────────────────────────────────────────────────────────

/// List branch protection rules for a repo.
/// GET /api/v1/repos/:owner/:name/branches/protection
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/branches/protection",
    tag = "Branch Protection",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_protections(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    let protections =
        match rg_core::branch_protection::service::list_protections(&state.db, &owner, &repo).await
        {
            Ok(protections) => protections,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "list_protections failed");
                return AppError::from(e).into_response();
            }
        };
    let mut named = Vec::with_capacity(protections.len());
    for protection in protections {
        match named_response(&state.db, protection).await {
            Ok(response) => named.push(response),
            Err(e) => return AppError::from(e).into_response(),
        }
    }
    (StatusCode::OK, Json(named)).into_response()
}

/// Create a branch protection rule.
/// POST /api/v1/repos/:owner/:name/branches/protection
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/branches/protection",
    tag = "Branch Protection",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "That branch is already protected", body = serde_json::Value),
    ),
)]
pub async fn create_protection(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    RepoAdmin {
        repo: repository,
        actor_id,
    }: RepoAdmin,
    headers: HeaderMap,
    Json(req): Json<CreateProtectionRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let allowed_push_user_ids = match resolve_allow_list(
        &state.db,
        req.allowed_push_users.as_deref(),
        req.allowed_push_user_ids,
    )
    .await
    {
        Ok(ids) => ids,
        // A name that matches no account is the caller's to fix, and
        // `UserRef::resolve` already says which one it was.
        Err(e) => return AppError::from(e).into_response(),
    };
    match rg_core::branch_protection::service::create_protection(
        &state.db,
        &owner,
        &repo,
        req.branch_name,
        req.require_pr,
        req.require_status_check,
        req.required_status_checks,
        req.require_approval,
        req.required_approvals,
        req.allow_force_push,
        req.require_signed_commits,
        allowed_push_user_ids,
    )
    .await
    {
        Ok(protection) => match named_response(&state.db, protection).await {
            Ok(response) => {
                record_grant(
                    &state,
                    &audit_actor,
                    "repo.branch_protection_create",
                    &owner,
                    &repository,
                    &headers,
                    protection_details(&response),
                )
                .await;
                (StatusCode::CREATED, Json(response)).into_response()
            }
            Err(e) => AppError::from(e).into_response(),
        },
        // An already-protected branch stays 400 (typed in the service), an
        // unknown repository is the 404 `resolve_repo` reports, and the insert
        // failing is a 5xx — all three were 400.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Get a branch protection rule by ID.
/// GET /api/v1/repos/:owner/:name/branches/protection/:id
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/branches/protection/{id}",
    tag = "Branch Protection",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_protection(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::branch_protection::service::get_protection_for_repo(&state.db, &owner, &repo, id)
        .await
    {
        Ok(protection) => match named_response(&state.db, protection).await {
            Ok(response) => (StatusCode::OK, Json(response)).into_response(),
            Err(e) => AppError::from(e).into_response(),
        },
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Update a branch protection rule.
/// PATCH /api/v1/repos/:owner/:name/branches/protection/:id
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/branches/protection/{id}",
    tag = "Branch Protection",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_protection(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    RepoAdmin {
        repo: repository,
        actor_id,
    }: RepoAdmin,
    headers: HeaderMap,
    Json(req): Json<UpdateProtectionRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let allowed_push_user_ids = match resolve_allow_list(
        &state.db,
        req.allowed_push_users.as_deref(),
        req.allowed_push_user_ids,
    )
    .await
    {
        Ok(ids) => ids,
        Err(e) => return AppError::from(e).into_response(),
    };
    match rg_core::branch_protection::service::update_protection_for_repo(
        &state.db,
        &owner,
        &repo,
        id,
        req.require_pr,
        req.require_status_check,
        req.required_status_checks,
        req.require_approval,
        req.required_approvals,
        req.allow_force_push,
        req.require_signed_commits,
        allowed_push_user_ids,
    )
    .await
    {
        Ok(protection) => match named_response(&state.db, protection).await {
            Ok(response) => {
                record_grant(
                    &state,
                    &audit_actor,
                    "repo.branch_protection_update",
                    &owner,
                    &repository,
                    &headers,
                    protection_details(&response),
                )
                .await;
                (StatusCode::OK, Json(response)).into_response()
            }
            Err(e) => AppError::from(e).into_response(),
        },
        // The scoped lookup already reports a missing (or foreign) rule as
        // `NotFound`; matching `get_protection` above, that is a 404 here rather
        // than a bad request, and a failed update is a 5xx.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Delete a branch protection rule.
/// DELETE /api/v1/repos/:owner/:name/branches/protection/:id
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/branches/protection/{id}",
    tag = "Branch Protection",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_protection(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    RepoAdmin {
        repo: repository,
        actor_id,
    }: RepoAdmin,
    headers: HeaderMap,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    // Read before the delete, because the branch the rule protected is the whole
    // content of the entry: "a protection rule was removed" says nothing an
    // incident review can use, and after the delete the name is gone. The scoped
    // lookup is the same one the delete performs, so a rule that is missing or
    // another repository's is refused here with the same 404.
    let removed = match rg_core::branch_protection::service::get_protection_for_repo(
        &state.db, &owner, &repo, id,
    )
    .await
    {
        Ok(protection) => protection,
        Err(e) => return AppError::from(e).into_response(),
    };
    let branch_name = removed.branch_name.clone();
    match rg_core::branch_protection::service::delete_protection_for_repo(
        &state.db, &owner, &repo, id,
    )
    .await
    {
        Ok(()) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.branch_protection_delete",
                &owner,
                &repository,
                &headers,
                serde_json::json!({ "branch": branch_name }),
            )
            .await;
            (StatusCode::NO_CONTENT, Json(serde_json::json!({}))).into_response()
        }
        // Same split as the update above: absent rule → 404, failed delete → 5xx.
        Err(e) => AppError::from(e).into_response(),
    }
}
