//! Time Tracking REST API.
//!
//! POST   /repos/:owner/:name/issues/:number/time   — add time entry
//! GET    /repos/:owner/:name/issues/:number/time   — list time entries
//! GET    /repos/:owner/:name/issues/:number/time/total — total tracked time
//! DELETE /repos/:owner/:name/issues/:number/time/:id   — delete time entry

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::auth::extract_bearer_claims;
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

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
    ),
)]
pub async fn add_time(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Json(body): Json<AddTimeRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    let issue = match rg_core::issue::service::get_issue(&state.db, &owner, &name, number).await {
        Ok(i) => i,
        Err(_) => return AppError::not_found("issue not found").into_response(),
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
        Err(e) => AppError::bad_request(e).into_response(),
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
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn list_time_entries(
    State(state): State<AppState>,
    Path((owner, name, number)): Path<(String, String, i64)>,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    let pagination = params.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

    let issue = match rg_core::issue::service::get_issue(&state.db, &owner, &name, number).await {
        Ok(i) => i,
        Err(_) => return AppError::not_found("issue not found").into_response(),
    };

    match rg_core::time_tracking::service::list_time_entries(&state.db, issue.id, offset, limit)
        .await
    {
        Ok((entries, total)) => (
            StatusCode::OK,
            Json(PaginatedResponse::new(entries, &pagination, total as u64)),
        )
            .into_response(),
        Err(e) => AppError::internal(e).into_response(),
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
    ),
)]
pub async fn total_time(
    State(state): State<AppState>,
    Path((owner, name, number)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let issue = match rg_core::issue::service::get_issue(&state.db, &owner, &name, number).await {
        Ok(i) => i,
        Err(_) => return AppError::not_found("issue not found").into_response(),
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
        Err(e) => AppError::internal(e).into_response(),
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
    ),
)]
pub async fn delete_time_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((_owner, _name, _number, id)): Path<(String, String, i64, i64)>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let _ = claims;

    match rg_core::time_tracking::service::delete_time_entry(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}
