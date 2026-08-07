//! Repository REST API.
//!
//! POST /api/v1/repos              — create repo (auth required)
//! GET  /api/v1/repos/:owner       — list repos by owner (user or org)
//! GET  /api/v1/repos/:owner/:name — get single repo
//!
//! Starring and watching are read-scoped actions, so they go through
//! [`crate::api::repo_access::require_authenticated_read`] rather than merely
//! authenticating the caller. `watch` in particular subscribes the caller to
//! notifications that carry the repository's content (PR titles, branch and
//! milestone names), so an ungated subscribe is a content leak, not just an
//! existence oracle telling `200` from `404 repository not found`.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::repo_access::{
    NamespaceCreate, RepoAuthRead, RepoOwner, RepoRead, RepoWrite, TargetOwner, TargetOwnerOrSelf,
};
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::{
    api::auth::extract_user_id,
    openapi::{PaginatedExploreRepoResponse, PaginatedRepoResponse},
    AppState,
};

/// POST /api/v1/repos
#[derive(Deserialize, ToSchema)]
pub struct CreateRepoRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_private: Option<bool>,
    /// Organization name — if provided, create repo under this org
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// Auto-initialize the repo with template files (README, LICENSE, .gitignore)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_init: Option<bool>,
    /// Default branch name (default: "main")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// .gitignore template key (e.g., "go", "rust", "python")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gitignores: Option<String>,
    /// LICENSE template key (e.g., "mit", "apache-2.0", "gpl-3.0")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// README template key (e.g., "default")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme: Option<String>,
    /// Default issue label set ("none", "default", "scrum")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_labels: Option<String>,
}

/// The namespace is the optional `org` field, and its absence means "under my
/// own account" — the half-open shape [`TargetOwnerOrSelf`] exists for. Whether
/// the caller may create there is [`NamespaceCreate`]'s answer, not this
/// handler's.
impl TargetOwnerOrSelf for CreateRepoRequest {
    fn target_owner_or_self(&self) -> Option<&str> {
        self.org.as_deref()
    }
}

/// Repository response (matches DB model fields exposed to API).
#[derive(serde::Serialize, ToSchema)]
pub struct RepoResponse {
    pub id: i64,
    pub owner_id: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_private: bool,
    pub default_branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_id: Option<i64>,
    pub stars_count: i64,
    pub forks_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_repo_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Public repository row as it appears in the explore listing.
///
/// This is intentionally not [`RepoResponse`]: explore exposes an owner's
/// display name so clients can build the repository URL, while withholding
/// fields that are irrelevant to the public catalogue.
#[derive(serde::Serialize, ToSchema)]
pub struct ExploreRepoResponse {
    pub id: i64,
    pub owner_id: i64,
    pub owner_name: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub stars_count: i64,
    pub forks_count: i64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[utoipa::path(
    post,
    path = "/repos",
    tag = "Repositories",
    request_body = CreateRepoRequest,
    responses(
        (status = 201, description = "Repository created", body = RepoResponse),
        (status = 400, description = "Invalid input", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden (org membership required)", body = serde_json::Value),
        (status = 409, description = "A repository of that name already exists in the namespace", body = serde_json::Value),
    )
)]
pub async fn create_repo(
    State(state): State<AppState>,
    headers: HeaderMap,
    // The namespace this repository lands in is named by the *body* — the
    // optional `org` field — so the gate over it is the body extractor, and the
    // organization id it resolved comes back with it. The handler used to ask
    // the question itself, with a copy of the membership rule that had drifted
    // twice over: its `_` arm answered `403 you are not a member of this
    // organization` to a failed *lookup*, and an unknown organization got a
    // `404` that told an outsider the account exists (card_1e1ed1ee06f1).
    //
    // Being a body extractor it has to come last.
    NamespaceCreate {
        actor_id: owner_id,
        org_id,
        body,
    }: NamespaceCreate<CreateRepoRequest>,
) -> impl IntoResponse {
    // Get owner identity for template substitution and initial commit author.
    let owner_user = match rg_db::ops::user_ops::find_by_id(&state.db, owner_id).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            return AppError::unauthorized("invalid token subject".to_string()).into_response()
        }
        Err(e) => return AppError::from(e).into_response(),
    };
    // The name comes from the account row rather than from the token's
    // `username` claim: a session minted before a rename still carries the old
    // spelling, and this name ends up in the commit author and the audit entry.
    let username = owner_user.username.clone();
    // The actor is resolved through the one type that may fill the actor column;
    // `username` below is the *namespace* the repository lands in, which is a
    // different thing that merely happens to match for a personal repository.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, owner_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };
    let owner_display = owner_user
        .display_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| username.clone());

    let opts = rg_core::repo::service::CreateRepoOptions {
        owner_id,
        name: body.name.clone(),
        description: body.description.clone(),
        is_private: body.is_private.unwrap_or(false),
        org_id,
        default_branch: body.default_branch.clone(),
        auto_init: body.auto_init.unwrap_or(false),
        gitignores: body.gitignores.clone(),
        license: body.license.clone(),
        readme: body.readme.clone(),
        issue_labels: body.issue_labels.clone(),
        owner_display_name: owner_display,
        git_author_name: Some(username.clone()),
        git_author_email: Some(owner_user.email.clone()),
    };

    match rg_core::repo::service::create_repo_with_opts(&state.db, opts, state.repo_root.as_path())
        .await
    {
        Ok(repo) => {
            // Record audit log
            let details = serde_json::json!({
                "name": body.name,
                "is_private": body.is_private.unwrap_or(false),
                "org": body.org.as_deref(),
                "auto_init": body.auto_init.unwrap_or(false),
                "default_branch": body.default_branch.as_deref(),
                "gitignores": body.gitignores.as_deref(),
                "license": body.license.as_deref(),
                "readme": body.readme.as_deref(),
                "issue_labels": body.issue_labels.as_deref(),
            });
            let resource_name = format!(
                "{}/{}",
                body.org.as_deref().unwrap_or(&username),
                &body.name
            );
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "repo.create",
                Some("repo"),
                Some(repo.id),
                Some(&resource_name),
                Some(&headers),
                Some(details),
            )
            .await;

            // `repo_created` is recorded inside
            // `rg_core::repo::service::create_repo_with_opts` so the REST path
            // and the import subsystem both count through one site.
            (StatusCode::CREATED, Json(serde_json::json!(repo))).into_response()
        }
        // A rejected name and a name already in use are typed `InvalidRequest`
        // and stay 400. The `gix init`, the `create_dir_all` under `repo_root`
        // and the insert are ours: a bind-mounted repo root the process cannot
        // write used to be reported as the caller's malformed request, with the
        // errno and the server-side path in the body (H-05).
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner
/// Lists repos for either a user or an organization.
#[derive(Deserialize)]
pub struct ListReposQuery {
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[utoipa::path(
    get,
    path = "/repos/{owner}",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "Username or organization name"),
        ("page" = Option<u64>, Query, description = "Page number (1-based)"),
        ("per_page" = Option<u64>, Query, description = "Items per page (1-100)"),
    ),
    responses(
        (status = 200, description = "List of repositories", body = PaginatedRepoResponse),
        (status = 404, description = "Owner not found", body = serde_json::Value),
        (status = 500, description = "Internal server error", body = serde_json::Value),
    )
)]
pub async fn list_repos(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(owner): Path<String>,
    Query(params): Query<ListReposQuery>,
) -> impl IntoResponse {
    let pagination = params.pagination.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();
    // An owner's shop window is not the same page for everyone: the listing has
    // to be filtered to what the caller may read, and in SQL, so that page size
    // and `total` stay honest. `explore` next door already did this by only ever
    // querying public repos.
    let viewer_id = extract_user_id(&headers, &state.jwt_secret);

    // Try user first. A failed lookup is not evidence that the owner is absent:
    // preserve the DB error so connection outages stay retryable server errors.
    let user = match rg_db::ops::user_ops::find_by_username(&state.db, &owner).await {
        Ok(user) => user,
        Err(error) => return AppError::from(error).into_response(),
    };
    if let Some(user) = user {
        match rg_db::ops::repo_ops::list_personal_by_owner_visible_to(
            &state.db, user.id, viewer_id, offset, limit,
        )
        .await
        {
            Ok((data, total)) => {
                return (
                    StatusCode::OK,
                    Json(PaginatedResponse::new(data, &pagination, total as u64)),
                )
                    .into_response()
            }
            Err(e) => return AppError::from(e).into_response(),
        }
    }

    // Try organization only after a successful user lookup that found no row.
    let org = match rg_db::ops::org_ops::get_org_by_name(&state.db, &owner).await {
        Ok(org) => org,
        Err(error) => return AppError::from(error).into_response(),
    };
    if let Some(org) = org {
        match rg_db::ops::repo_ops::list_by_org_visible_to(
            &state.db, org.id, viewer_id, offset, limit,
        )
        .await
        {
            Ok((data, total)) => {
                return (
                    StatusCode::OK,
                    Json(PaginatedResponse::new(data, &pagination, total as u64)),
                )
                    .into_response()
            }
            Err(e) => return AppError::from(e).into_response(),
        }
    }

    AppError::not_found("owner not found (neither user nor organization)".to_string())
        .into_response()
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "Username or organization name"),
        ("name" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Repository details", body = RepoResponse),
        (status = 401, description = "Authentication required", body = serde_json::Value),
        (status = 403, description = "Access denied", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = serde_json::Value),
        (status = 500, description = "Internal server error", body = serde_json::Value),
    )
)]
/// GET /api/v1/repos/:owner/:name
/// Gets a single repo, supporting both user and org owners.
pub async fn get_repo(RepoRead { repo }: RepoRead) -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!(repo))).into_response()
}

// ── Star/Watch/Delete handlers ───────────────────────────────────────────────

/// Request body for watch state.
#[derive(serde::Deserialize, ToSchema)]
pub struct WatchRequest {
    pub state: String,
}

/// PUT /api/v1/repos/:owner/:name/star
#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/star",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn star_repo(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAuthRead {
        repo,
        actor_id: user_id,
    }: RepoAuthRead,
) -> impl IntoResponse {
    // Read-scoped: see the module note on starring/watching a private repo.

    match rg_core::repo::service::toggle_star(&state.db, user_id, repo.id).await {
        Ok(starred) => {
            if starred {
                crate::metrics::recorder::star_given();
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({ "starred": starred })),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/starred
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/starred",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_starred_status(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAuthRead {
        repo,
        actor_id: user_id,
    }: RepoAuthRead,
) -> impl IntoResponse {
    // Read-scoped: see the module note on starring/watching a private repo.

    match rg_core::repo::service::is_starred(&state.db, user_id, repo.id).await {
        Ok(starred) => (
            StatusCode::OK,
            Json(serde_json::json!({ "starred": starred })),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/stargazers
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/stargazers",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        PaginationParams,
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_stargazers(
    State(state): State<AppState>,
    RepoRead { repo }: RepoRead,
    Path((_, _)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    let pagination = params.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

    match rg_core::repo::service::list_stargazers(&state.db, repo.id, offset, limit).await {
        Ok((stargazers, total)) => (
            StatusCode::OK,
            Json(PaginatedResponse::new(
                stargazers,
                &pagination,
                total as u64,
            )),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/watch
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/watch",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_watch_status(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAuthRead {
        repo,
        actor_id: user_id,
    }: RepoAuthRead,
) -> impl IntoResponse {
    // Read-scoped: see the module note on starring/watching a private repo.

    match rg_core::repo::service::get_watch(&state.db, user_id, repo.id).await {
        Ok(watch_state) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "watch_state": watch_state.unwrap_or_else(|| "not_watching".to_string())
            })),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// PUT /api/v1/repos/:owner/:name/watch
#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/watch",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 400, description = "Unknown watch state", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn watch_repo(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAuthRead {
        repo,
        actor_id: user_id,
    }: RepoAuthRead,
    Json(body): Json<WatchRequest>,
) -> impl IntoResponse {
    // Read-scoped: see the module note on starring/watching a private repo.

    match rg_core::repo::service::set_watch(&state.db, user_id, repo.id, &body.state).await {
        Ok(watch_state) => (
            StatusCode::OK,
            Json(serde_json::json!({ "watch_state": watch_state })),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/repos/:owner/:name/watch
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/watch",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn unwatch_repo(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAuthRead {
        repo,
        actor_id: user_id,
    }: RepoAuthRead,
) -> impl IntoResponse {
    // Read-scoped: see the module note on starring/watching a private repo.

    let unwatched = rg_core::repo::service::WatchState::NotWatching;
    match rg_core::repo::service::set_watch(&state.db, user_id, repo.id, unwatched.as_str()).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({ "watch_state": unwatched.as_str() })),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/repos/:owner/:name
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "Repository still has active CI pipelines", body = serde_json::Value),
    ),
)]
pub async fn delete_repo_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    RepoOwner {
        repo,
        actor_id: user_id,
    }: RepoOwner,
) -> impl IntoResponse {
    // Only the audit trail's actor name; the ownership decision above it is the
    // extractor's and is not re-litigated here. A missing row is the server's
    // inconsistency, not the caller's fault — the id came from a gate that
    // resolved it against a repository this account owns — so it is a `500`.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, user_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::repo::service::delete_repo(
        &state.db,
        &state.repo_root,
        state.blob_storage.as_ref(),
        state.oci_storage.as_ref(),
        &repo,
    )
    .await
    {
        Ok(()) => {
            // Record audit log
            let resource_name = format!("{}/{}", owner, name);
            let details = serde_json::json!({
                "owner": owner,
                "name": name
            });
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "repo.delete",
                Some("repo"),
                Some(repo.id),
                Some(&resource_name),
                Some(&headers),
                Some(details),
            )
            .await;

            crate::metrics::recorder::repo_deleted();
            (StatusCode::OK, Json(serde_json::json!({ "deleted": true }))).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Fork handlers ──────────────────────────────────────────────────────

#[derive(serde::Deserialize, ToSchema)]
pub struct ForkRequest {
    pub org: Option<String>,
}

/// POST /api/v1/repos/:owner/:name/fork
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/fork",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Source repository not readable by this caller", body = serde_json::Value),
        (status = 404, description = "No such source repository", body = serde_json::Value),
        (status = 409, description = "The forker already owns a repository of that name", body = serde_json::Value),
    ),
)]
pub async fn fork_repo_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    // Forking reads the source, so the level the route table declares is the
    // level the signature takes. The handler used to take no repository gate at
    // all and let `rg_core::repo::service::fork_repo` decide, off its own
    // `can_read_repo` — a copy of the rule one crate away from
    // `api::repo_access`, where no guard in this crate could see it
    // (card_b38bfb0f2b40). The extractor also widens what counts as a session:
    // the old in-body `extract_bearer_claims` accepted only
    // `Authorization: Bearer`, so the browser — which holds the HttpOnly
    // `forgekeep_token` cookie and no header — got a `401` from the fork button.
    RepoAuthRead {
        repo: source,
        actor_id: user_id,
    }: RepoAuthRead,
    Path((owner, name)): Path<(String, String)>,
) -> impl IntoResponse {
    match rg_core::repo::service::fork_repo(&state.db, user_id, &owner, &source, &state.repo_root)
        .await
    {
        Ok(rg_core::repo::service::ForkedRepo {
            repo,
            owner_username,
        }) => {
            // Record audit log. The fork is named the way every other record in
            // this module names a repository — `owner/name` — not `<user id>/name`:
            // an audit row is read by a human looking for a path that exists.
            // The name is the one the fork actually landed under, taken from the
            // account row rather than from the session's `username` claim: a
            // session minted before a rename still carries the old spelling.
            let details = serde_json::json!({
                "source_owner": owner,
                "source_name": name,
                "fork_owner": owner_username
            });
            let resource_name = format!("{owner_username}/{}", repo.name);
            let audit_actor =
                rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, user_id).await;
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "repo.fork",
                Some("repo"),
                Some(repo.id),
                Some(&resource_name),
                Some(&headers),
                Some(details),
            )
            .await;

            crate::metrics::recorder::repo_forked();
            // `201`, not `202`: the clone and the row are both done by the time
            // this returns, and the body is the created repository. The route
            // has always *declared* `201` above — nothing could notice the
            // disagreement while every fork was a 500.
            (StatusCode::CREATED, Json(serde_json::json!(repo))).into_response()
        }
        // Absent source → 404 and a private source the caller may not read →
        // 403, both from the extractor above; a name already taken in their
        // account → 400 from here. The `git clone --bare` behind them is ours
        // and reports as a 5xx instead of handing the client the git command
        // line in a 400 body.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/forks
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/forks",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        PaginationParams,
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_forks_handler(
    State(state): State<AppState>,
    RepoRead { .. }: RepoRead,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    let pagination = params.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

    match rg_core::repo::service::list_forks(&state.db, &owner, &name, offset, limit).await {
        Ok((forks, total)) => (
            StatusCode::OK,
            Json(PaginatedResponse::new(forks, &pagination, total as u64)),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Transfer handler ──────────────────────────────────────────────────

#[derive(serde::Deserialize, ToSchema)]
pub struct TransferRequest {
    pub new_owner: String,
}

/// The destination namespace is named by the body, so the gate over it is a
/// body extractor — see [`NamespaceCreate`]. The repository's *name* is not in
/// the payload: a transfer keeps it and takes it from the route.
impl TargetOwner for TransferRequest {
    fn target_owner(&self) -> &str {
        &self.new_owner
    }
}

/// POST /api/v1/repos/:owner/:name/transfer
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/transfer",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body = TransferRequest,
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden (source not owned, or destination namespace is someone else's)", body = serde_json::Value),
        (status = 409, description = "The destination namespace already holds a repository of that name", body = serde_json::Value),
    ),
)]
pub async fn transfer_repo_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    // Two namespaces, two gates. `RepoOwner` gates the *source*: `transfer_repo`
    // checks ownership too, and keeps doing so — the service is reachable from
    // elsewhere — but the extractor makes the route state the level it needs
    // instead of the route table asserting `RepoOwner` while the only
    // enforcement lives three calls down.
    RepoOwner { .. }: RepoOwner,
    // `NamespaceCreate` gates the *destination*, which is the half nobody was
    // asking about: `new_owner` arrives in the payload, so no path extractor
    // reaches it, and a transfer into a stranger's account went through with a
    // `200`. Being a body extractor it has to come last.
    NamespaceCreate {
        actor_id: user_id,
        body,
        // The resolved destination organization id is not needed here: the
        // service resolves the destination itself when it rewrites the row.
        ..
    }: NamespaceCreate<TransferRequest>,
) -> impl IntoResponse {
    // Only the audit trail's actor name; both access decisions above it are the
    // extractors' and are not re-litigated here, for the reason spelled out on
    // `delete_repo` above.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, user_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::repo::service::transfer_repo(
        &state.db,
        user_id,
        &owner,
        &name,
        &body.new_owner,
        &state.repo_root,
        state.blob_storage.as_ref(),
        state.oci_storage.as_ref(),
    )
    .await
    {
        Ok(repo) => {
            // Record audit log
            let details = serde_json::json!({
                "old_owner": owner,
                "new_owner": body.new_owner,
                "name": name
            });
            let resource_name = format!("{}/{}", body.new_owner, name);
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "repo.transfer",
                Some("repo"),
                Some(repo.id),
                Some(&resource_name),
                Some(&headers),
                Some(details),
            )
            .await;

            (StatusCode::OK, Json(serde_json::json!(repo))).into_response()
        }
        // Unknown repository → 404, non-owner → 403, unknown destination owner or
        // a name already taken there → 400. The directory rename that moves the
        // repository on disk is ours and no longer masquerades as a bad request.
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Commit Status handlers ─────────────────────────────────────────────

#[derive(serde::Deserialize, ToSchema)]
pub struct CreateCommitStatusRequest {
    pub state: String,
    pub context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
}

/// POST /api/v1/repos/:owner/:name/statuses/:sha
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/statuses/{sha}",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("sha" = String, Path, description = "sha"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn create_commit_status(
    State(state): State<AppState>,
    Path((_, _, sha)): Path<(String, String, String)>,
    RepoWrite {
        repo,
        actor_id: user_id,
    }: RepoWrite,
    Json(body): Json<CreateCommitStatusRequest>,
) -> impl IntoResponse {
    match rg_core::repo::service::create_commit_status(
        &state.db,
        repo.id,
        &sha,
        &body.state,
        &body.context,
        body.description.as_deref(),
        body.target_url.as_deref(),
        user_id,
    )
    .await
    {
        Ok(status) => (StatusCode::CREATED, Json(serde_json::json!(status))).into_response(),
        // An unknown status state is the only thing the caller can get wrong here
        // and keeps its 400; the upsert behind it is ours.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/commits/:sha/statuses
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/commits/{sha}/statuses",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("sha" = String, Path, description = "sha"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_commit_statuses(
    State(state): State<AppState>,
    Path((owner, name, sha)): Path<(String, String, String)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::repo::service::list_commit_statuses(&state.db, &owner, &name, &sha).await {
        Ok(statuses) => (StatusCode::OK, Json(serde_json::json!(statuses))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/commits/:sha/status
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/commits/{sha}/status",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("sha" = String, Path, description = "sha"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_combined_status(
    State(state): State<AppState>,
    Path((owner, name, sha)): Path<(String, String, String)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::repo::service::get_combined_status(&state.db, &owner, &name, &sha).await {
        Ok(combined) => (StatusCode::OK, Json(combined)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Template listing endpoints ─────────────────────────────────────────

/// GET /api/v1/repos/templates/gitignores
/// List available .gitignore templates.
#[utoipa::path(
    get,
    path = "/repos/templates/gitignores",
    tag = "Repositories",
    responses(
        (status = 200, description = "List of .gitignore template options", body = serde_json::Value),
    )
)]
pub async fn list_gitignore_templates() -> impl IntoResponse {
    let options = rg_core::repo::templates::gitignore_options();
    (StatusCode::OK, Json(serde_json::json!({ "data": options }))).into_response()
}

/// GET /api/v1/repos/templates/licenses
/// List available LICENSE templates.
#[utoipa::path(
    get,
    path = "/repos/templates/licenses",
    tag = "Repositories",
    responses(
        (status = 200, description = "List of LICENSE template options", body = serde_json::Value),
    )
)]
pub async fn list_license_templates() -> impl IntoResponse {
    let options = rg_core::repo::templates::license_options();
    (StatusCode::OK, Json(serde_json::json!({ "data": options }))).into_response()
}

/// GET /api/v1/repos/templates/readmes
/// List available README templates.
#[utoipa::path(
    get,
    path = "/repos/templates/readmes",
    tag = "Repositories",
    responses(
        (status = 200, description = "List of README template options", body = serde_json::Value),
    )
)]
pub async fn list_readme_templates() -> impl IntoResponse {
    let options = rg_core::repo::templates::readme_options();
    (StatusCode::OK, Json(serde_json::json!({ "data": options }))).into_response()
}

/// GET /api/v1/repos/templates/labels
/// List available default label sets.
#[utoipa::path(
    get,
    path = "/repos/templates/labels",
    tag = "Repositories",
    responses(
        (status = 200, description = "List of label set options", body = serde_json::Value),
    )
)]
pub async fn list_label_sets() -> impl IntoResponse {
    let options = rg_core::repo::templates::label_set_options();
    (StatusCode::OK, Json(serde_json::json!({ "data": options }))).into_response()
}

// ── Public repository listing (Explore) ───────────────────────────────

#[derive(Deserialize)]
pub struct ExploreQuery {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
}

/// GET /api/v1/repos/explore
/// List public repositories for the explore page.
#[utoipa::path(
    get,
    path = "/repos/explore",
    tag = "Repositories",
    params(
        ("page" = Option<u64>, Query, description = "Page number (1-based)"),
        ("per_page" = Option<u64>, Query, description = "Items per page (1-100)"),
    ),
    responses(
        (status = 200, description = "Paginated list of public repositories", body = PaginatedExploreRepoResponse),
    )
)]
pub async fn explore(
    State(state): State<AppState>,
    Query(params): Query<ExploreQuery>,
) -> impl IntoResponse {
    let pagination = PaginationParams {
        page: params.page.unwrap_or(1),
        per_page: params.per_page.unwrap_or(20),
    }
    .clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

    match rg_db::ops::repo_ops::list_public_paginated(&state.db, offset, limit).await {
        Ok((data, total)) => {
            // Enrich with owner names. The owner is not decoration here: the
            // client links to `/{owner_name}/{name}`, so this listing is only
            // usable if the name is the account's own.
            //
            // The two ways the lookup can end are therefore kept apart. A row
            // that is genuinely absent — the account was deleted after the
            // repository was published — answers `null`, which says "there is
            // no owner to name" and lets the client render it as such. A lookup
            // that could not run says nothing about the owner at all, so it
            // fails the response instead of inventing one: a placeholder here
            // ships a `200` the client trusts, never retries, and turns a live
            // account into an unknown one (card_f15f12e055d0).
            let lookups = futures::future::join_all(
                data.iter()
                    .map(|repo| rg_db::ops::user_ops::find_by_id(&state.db, repo.owner_id)),
            )
            .await;

            let mut enriched: Vec<ExploreRepoResponse> = Vec::with_capacity(data.len());
            for (repo, lookup) in data.iter().zip(lookups) {
                let owner_name = match lookup {
                    Ok(owner) => owner.map(|user| user.username),
                    Err(error) => return AppError::from(error).into_response(),
                };
                enriched.push(ExploreRepoResponse {
                    id: repo.id,
                    owner_id: repo.owner_id,
                    owner_name,
                    name: repo.name.clone(),
                    description: repo.description.clone(),
                    stars_count: repo.stars_count,
                    forks_count: repo.forks_count,
                    updated_at: repo.updated_at,
                });
            }

            (
                StatusCode::OK,
                Json(PaginatedResponse::new(enriched, &pagination, total as u64)),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}
