//! Import REST API — repository migration endpoints.
//!
//! POST   /api/v1/imports       — start a new import
//! GET    /api/v1/imports/{id}   — check import status
//! GET    /api/v1/imports        — list user's imports
//! DELETE /api/v1/imports/{id}   — cancel/delete an import

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::auth::AuthUser;
use crate::api::repo_access::{NamespaceWrite, TargetNamespace, TargetOwner};
use crate::error::AppError;
use crate::AppState;

/// Request body for starting a new import.
#[derive(Deserialize, ToSchema)]
pub struct StartImportRequest {
    /// Source platform: "github", "gitlab", "gitea", or "git"
    pub platform: String,
    /// Source repository URL (e.g., https://github.com/user/repo)
    pub source_url: String,
    /// Target owner in ForgeKeep
    pub target_owner: String,
    /// Target repository name (defaults to source repo name)
    #[serde(default)]
    pub target_name: Option<String>,
    /// API access token for the source platform
    pub auth_token: Option<String>,
    /// Whether to import the repository itself
    #[serde(default = "default_true")]
    pub import_repo: bool,
    /// Whether to import issues
    #[serde(default = "default_true")]
    pub import_issues: bool,
    /// Whether to import pull/merge requests
    #[serde(default = "default_true")]
    pub import_pull_requests: bool,
    /// Whether to import wiki pages
    #[serde(default)]
    pub import_wiki: bool,
    /// Whether to import releases
    #[serde(default = "default_true")]
    pub import_releases: bool,
    /// Whether to import labels
    #[serde(default = "default_true")]
    pub import_labels: bool,
    /// Whether to import milestones
    #[serde(default = "default_true")]
    pub import_milestones: bool,
}

fn default_true() -> bool {
    true
}

impl StartImportRequest {
    /// The repository name the import will write to.
    ///
    /// The field is optional on the wire: leaving it out means "name it after
    /// the source repository". The gate and the handler must agree on the
    /// answer — a target the gate did not see is a target nobody authorized —
    /// so the defaulting lives here rather than in either of them.
    fn resolved_target_name(&self) -> String {
        match self.target_name {
            Some(ref n) if !n.is_empty() => n.clone(),
            _ => {
                let url = self
                    .source_url
                    .trim_end_matches('/')
                    .trim_end_matches(".git");
                url.split('/')
                    .next_back()
                    .unwrap_or("imported-repo")
                    .to_string()
            }
        }
    }
}

impl TargetOwner for StartImportRequest {
    fn target_owner(&self) -> &str {
        &self.target_owner
    }
}

impl TargetNamespace for StartImportRequest {
    fn target_name(&self) -> String {
        self.resolved_target_name()
    }
}

/// POST /api/v1/imports
///
/// Start a new import from GitHub, GitLab, Gitea, or a generic Git remote.
#[utoipa::path(
    post,
    path = "/imports",
    tag = "Imports",
    request_body = StartImportRequest,
    responses(
        (status = 201, description = "Import started", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden (target namespace is someone else's)", body = serde_json::Value),
    ),
)]
pub async fn start_import(
    State(state): State<AppState>,
    // The target namespace arrives in the body, so the gate is a body
    // extractor: `target_owner` is decided by `api::repo_access` before this
    // function runs. Without it any authenticated user could pour an import —
    // issues, pull requests, releases, branches — into someone else's
    // `owner/name`, or have a repository created under their account.
    NamespaceWrite {
        actor_id: user_id,
        body,
    }: NamespaceWrite<StartImportRequest>,
) -> impl IntoResponse {
    // Validate platform
    if !matches!(
        body.platform.as_str(),
        "github" | "gitlab" | "gitea" | "git"
    ) {
        return AppError::bad_request("platform must be 'github', 'gitlab', 'gitea', or 'git'")
            .into_response();
    }

    // SSRF fast-feedback: reject an obviously-internal or non-git-transport
    // source URL up front (DNS-free). The background clone path re-checks with a
    // full DNS-resolving guard, but this returns 400 immediately for file://,
    // ext::, and internal IP-literal hosts instead of a later async failure.
    if let Err(e) = state
        .trusted_import_origins
        .check_url_static(&body.source_url)
    {
        return AppError::bad_request(format!("invalid source URL: {e}")).into_response();
    }

    // The same name the gate authorized above.
    let target_name = body.resolved_target_name();

    match rg_core::import::service::start_import(
        &state.db,
        &state.import_workers,
        user_id,
        body.platform,
        body.source_url,
        body.target_owner,
        target_name,
        body.auth_token,
        body.import_repo,
        body.import_issues,
        body.import_pull_requests,
        body.import_wiki,
        body.import_releases,
        body.import_labels,
        body.import_milestones,
        &state.trusted_import_origins,
        &state.repo_root,
    )
    .await
    {
        Ok(task) => (StatusCode::CREATED, Json(serde_json::json!(task))).into_response(),
        // Both request-shaped checks already ran above. Everything left here is
        // the row insert, so a failure is ours — not a bad request the caller
        // could fix by editing the payload.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/imports/{id}
///
/// Check the status of an import task.
#[utoipa::path(
    get,
    path = "/imports/{id}",
    tag = "Imports",
    params(
        ("id" = i64, Path, description = "Import task ID"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn get_import_status(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match import_task_of_user(&state, user_id, id).await {
        Ok(task) => (StatusCode::OK, Json(serde_json::json!(task))).into_response(),
        Err(e) => e.into_response(),
    }
}

/// GET /api/v1/imports
///
/// List the current user's import tasks.
#[utoipa::path(
    get,
    path = "/imports",
    tag = "Imports",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_imports(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> impl IntoResponse {
    match rg_db::ops::import_task_ops::find_by_user(&state.db, user_id, 20).await {
        Ok(tasks) => (StatusCode::OK, Json(serde_json::json!(tasks))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/imports/{id}
///
/// Cancel and delete an import task.
#[utoipa::path(
    delete,
    path = "/imports/{id}",
    tag = "Imports",
    params(
        ("id" = i64, Path, description = "Import task ID"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 409, description = "Import is still running on another worker", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn delete_import(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(e) = import_task_of_user(&state, user_id, id).await {
        return e.into_response();
    }
    // The row is the worker's progress channel, so it must outlive the future
    // that writes through it. Waiting here also makes the 204 a real boundary:
    // once the client sees it, this process cannot publish more import data.
    let canceled = match state.import_workers.cancel_and_wait(id).await {
        Ok(canceled) => canceled,
        Err(e) => return AppError::from(e).into_response(),
    };
    if !canceled {
        // The registry is intentionally process-local. A running row without a
        // local handle can belong to another server (or the CLI), so deleting
        // it would recreate the original bug across process boundaries. Read
        // after the cancellation attempt to distinguish that case from a
        // worker which just completed and unregistered itself.
        let task = match import_task_of_user(&state, user_id, id).await {
            Ok(task) => task,
            Err(e) => return e.into_response(),
        };
        if rg_db::ops::import_task_ops::RUNNING_STATUSES.contains(&task.status.as_str()) {
            return AppError::Conflict(
                "import task is still running and cannot be canceled by this server".to_string(),
            )
            .into_response();
        }
    }
    // That lookup and this `DELETE` are two statements, so a concurrent delete
    // can land in between; the 204 therefore comes from `rows_affected` rather
    // than from the row having existed a moment ago.
    match rg_db::ops::import_task_ops::delete_by_id(&state.db, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => AppError::not_found("import task not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Fetch an import task and re-anchor it to the account that authenticated.
///
/// `{id}` is a global `import_tasks` primary key and the route's only gate is
/// `AuthUser` — being *some* account must not reach another one's imports,
/// which carry the source-forge auth token the import was started with. The
/// owner here plays the part the repository plays elsewhere in this family: the
/// row names its scope, and the handler has to compare it.
///
/// A mismatch answers 404 rather than 403, for the same reason the repository
/// anchors do: a 403 confirms the id exists.
///
/// `get_import_status` and `delete_import` each spelled the comparison inline.
/// A named helper is the form `global_id_anchor_guard` can read.
async fn import_task_of_user(
    state: &AppState,
    user_id: i64,
    task_id: i64,
) -> Result<rg_db::entities::import_task::Model, AppError> {
    match rg_db::ops::import_task_ops::find_by_id(&state.db, task_id).await {
        Ok(Some(task)) if task.user_id == user_id => Ok(task),
        Ok(_) => Err(AppError::not_found("import task not found")),
        Err(e) => Err(AppError::from(e)),
    }
}
