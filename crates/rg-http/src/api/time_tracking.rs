//! Time Tracking REST API.
//!
//! POST   /repos/:owner/:name/issues/:number/time   — add time entry
//! GET    /repos/:owner/:name/issues/:number/time   — list time entries
//! GET    /repos/:owner/:name/issues/:number/time/total — total tracked time
//! DELETE /repos/:owner/:name/issues/:number/time/:id   — delete time entry

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::repo_access::{RepoRead, RepoWrite};
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

/// Resolve an issue *inside* an already-authorized repository.
///
/// The permission check at the call site is about `owner/name`, so the issue it
/// guards has to be the one that lives there — resolving the number against
/// `repo.id` is what keeps the check and the object it protects pointing at the
/// same thing.
async fn issue_in_repo(
    state: &AppState,
    repo_id: i64,
    number: i64,
) -> Result<rg_db::entities::issue::Model, AppError> {
    rg_db::ops::issue_ops::find_by_repo_and_number(&state.db, repo_id, number)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("issue not found"))
}

/// Request body for adding a time entry.
#[derive(Deserialize, ToSchema)]
pub struct AddTimeRequest {
    pub duration_minutes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// POST /api/v1/repos/{owner}/{name}/issues/{number}/time
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/issues/{number}/time",
    tag = "Time Tracking",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "issue number"),
    ),
    request_body = AddTimeRequest,
    responses(
        (status = 201, description = "Time entry created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn add_time(
    State(state): State<AppState>,
    Path((_, _, number)): Path<(String, String, i64)>,
    RepoWrite {
        repo,
        actor_id: user_id,
    }: RepoWrite,
    Json(body): Json<AddTimeRequest>,
) -> impl IntoResponse {
    // Logging time appends a row to the issue tracker of this repository, so it
    // is gated like any other issue mutation. The handler used to stop at "the
    // token parses", which let any account on the instance write hours onto the
    // issues of a private repository it cannot even read.

    let issue = match issue_in_repo(&state, repo.id, number).await {
        Ok(i) => i,
        Err(e) => return e.into_response(),
    };

    match rg_core::time_tracking::service::add_time(
        &state.db,
        issue.id,
        user_id,
        body.duration_minutes,
        body.description,
    )
    .await
    {
        Ok(entry) => (StatusCode::CREATED, Json(serde_json::json!(entry))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/{owner}/{name}/issues/{number}/time
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}/time",
    tag = "Time Tracking",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "issue number"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn list_time_entries(
    State(state): State<AppState>,
    RepoRead { .. }: RepoRead,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    // Time entries carry a free-form description written by collaborators, so
    // they are exactly as private as the repository. Without this gate the
    // handler had no way to see the caller at all.

    let pagination = params.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

    let issue = match rg_core::issue::service::get_issue(&state.db, &owner, &name, number).await {
        Ok(i) => i,
        Err(e) => return AppError::from(e).into_response(),
    };

    match rg_core::time_tracking::service::list_time_entries(&state.db, issue.id, offset, limit)
        .await
    {
        Ok((entries, total)) => (
            StatusCode::OK,
            Json(PaginatedResponse::new(entries, &pagination, total as u64)),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}/time/total",
    tag = "Time Tracking",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "issue number"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn total_time(
    State(state): State<AppState>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    // The aggregate leaks the same thing the listing does — that work happened
    // on this issue, and how much of it — so it gets the same gate.

    let issue = match rg_core::issue::service::get_issue(&state.db, &owner, &name, number).await {
        Ok(i) => i,
        Err(e) => return AppError::from(e).into_response(),
    };

    match rg_core::time_tracking::service::total_time_minutes(&state.db, issue.id).await {
        Ok(minutes) => {
            let formatted = rg_core::time_tracking::service::format_duration(minutes);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "total_minutes": minutes,
                    "total_formatted": formatted,
                })),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/repos/{owner}/{name}/issues/{number}/time/{id}
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/issues/{number}/time/{id}",
    tag = "Time Tracking",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "issue number"),
        ("id" = i64, Path, description = "time entry id"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn delete_time_entry(
    State(state): State<AppState>,
    Path((_, _, number, id)): Path<(String, String, i64, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    // Three quarters of the route used to be discarded — `_owner`, `_name` and
    // `_number` were unbound and the claims were extracted only to be dropped,
    // so any authenticated caller could delete any time entry on the instance
    // by naming a repository of their own and walking `{id}`. The repository is
    // what the permission is about, so it has to be the one that is checked...

    // ...and the issue under it is what re-anchors `{id}`: the service refuses
    // to delete an entry that belongs to a different issue, so write access
    // here can no longer reach another repository's rows.
    let issue = match issue_in_repo(&state, repo.id, number).await {
        Ok(i) => i,
        Err(e) => return e.into_response(),
    };

    match rg_core::time_tracking::service::delete_time_entry(&state.db, issue.id, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}
