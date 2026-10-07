//! A repository's LFS store as its administrators see it: the size, the
//! objects, and removing the ones nothing needs (card_9e4dd3f8330c).
//!
//! All four routes are administrative. The reference scan behind the orphan
//! list and the removal reads every tree of every ref's history, which is why
//! it runs only when an administrator asks; the listing and the size are plain
//! queries.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::repo_access::RepoAdmin;
use crate::error::AppError;
use crate::AppState;
use rg_core::lfs::gc::{self, LfsObjectView, LfsUsage, PruneOutcome};

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LfsObjectsQuery {
    /// The `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Page size (default 100, at most 1000).
    pub limit: Option<u64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct LfsObjectPage {
    #[schema(value_type = Vec<Object>)]
    pub objects: Vec<LfsObjectView>,
    /// Empty on the last page.
    pub next_cursor: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct LfsOrphans {
    /// Objects no ref's history points at, older than the grace period.
    #[schema(value_type = Vec<Object>)]
    pub objects: Vec<LfsObjectView>,
    /// How old an unreferenced object must be before it is listed here.
    pub grace_hours: i64,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PruneRequest {
    /// The objects to remove, from the orphan list.
    pub oids: Vec<String>,
}

fn repo_path(state: &AppState, owner: &str, name: &str) -> Result<std::path::PathBuf, AppError> {
    rg_core::platform::validate_repo_path(owner)
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    rg_core::platform::validate_repo_path(name)
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    let path = state.repo_root.join(format!("{owner}/{name}.git"));
    crate::error::ensure_repository_storage(&path).map_err(AppError::from)?;
    Ok(path)
}

/// LFS store size: GET /repos/:owner/:name/lfs/usage
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/lfs/usage",
    tag = "LFS",
    params(("owner" = String, Path), ("name" = String, Path)),
    responses(
        (status = 200, description = "`{object_count, total_bytes}` of the stored objects", body = serde_json::Value),
        (status = 403, description = "Not a repository administrator", body = serde_json::Value),
    ),
)]
pub async fn usage(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match gc::usage(&state.db, repo.id).await {
        Ok(usage) => (StatusCode::OK, Json::<LfsUsage>(usage)).into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// LFS objects: GET /repos/:owner/:name/lfs/objects
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/lfs/objects",
    tag = "LFS",
    params(("owner" = String, Path), ("name" = String, Path), LfsObjectsQuery),
    responses(
        (status = 200, description = "One page of the store, oldest first", body = LfsObjectPage),
        (status = 400, description = "Malformed cursor or limit", body = serde_json::Value),
        (status = 403, description = "Not a repository administrator", body = serde_json::Value),
    ),
)]
pub async fn list_objects(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
    Query(query): Query<LfsObjectsQuery>,
) -> impl IntoResponse {
    match gc::list_objects(&state.db, repo.id, query.cursor.as_deref(), query.limit).await {
        Ok((objects, next)) => (
            StatusCode::OK,
            Json(LfsObjectPage {
                objects,
                next_cursor: next.unwrap_or_default(),
            }),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// Unreferenced LFS objects: GET /repos/:owner/:name/lfs/orphans
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/lfs/orphans",
    tag = "LFS",
    params(("owner" = String, Path), ("name" = String, Path)),
    responses(
        (status = 200, description = "Objects no ref's history points at and old enough to remove", body = LfsOrphans),
        (status = 403, description = "Not a repository administrator", body = serde_json::Value),
    ),
)]
pub async fn list_orphans(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    let path = match repo_path(&state, &owner, &name) {
        Ok(path) => path,
        Err(error) => return error.into_response(),
    };
    match gc::find_orphans(&state.db, repo.id, &path, chrono::Utc::now()).await {
        Ok(objects) => (
            StatusCode::OK,
            Json(LfsOrphans {
                objects,
                grace_hours: gc::ORPHAN_GRACE.num_hours(),
            }),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// Remove unreferenced LFS objects: POST /repos/:owner/:name/lfs/orphans/prune
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/lfs/orphans/prune",
    tag = "LFS",
    params(("owner" = String, Path), ("name" = String, Path)),
    request_body = PruneRequest,
    responses(
        (status = 200, description = "`{deleted, kept}` — every object asked for, removed or kept with the reason", body = serde_json::Value),
        (status = 400, description = "Malformed request", body = serde_json::Value),
        (status = 403, description = "Not a repository administrator", body = serde_json::Value),
    ),
)]
pub async fn prune(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
    Json(request): Json<PruneRequest>,
) -> impl IntoResponse {
    if request.oids.len() > 10_000 {
        return AppError::bad_request("at most 10000 objects can be removed at once")
            .into_response();
    }
    if let Some(bad) = request
        .oids
        .iter()
        .find(|oid| !rg_core::lfs::service::is_valid_oid(oid))
    {
        return AppError::bad_request(format!("`{bad}` is not an LFS object id")).into_response();
    }
    let path = match repo_path(&state, &owner, &name) {
        Ok(path) => path,
        Err(error) => return error.into_response(),
    };
    // The store keys are spelled from the repository's own namespace, not
    // from however the URL spelled it.
    let (namespace, repo_name) =
        match rg_core::repo::service::repository_identity(&state.db, repo.id).await {
            Ok(identity) => identity,
            Err(error) => return AppError::from(error).into_response(),
        };
    match gc::prune(
        &state.db,
        state.blob_storage.as_ref(),
        &state.repo_root,
        rg_core::lfs::service::LfsRepository {
            id: repo.id,
            owner: &namespace,
            name: &repo_name,
        },
        &path,
        &request.oids,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(outcome) => (StatusCode::OK, Json::<PruneOutcome>(outcome)).into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}
