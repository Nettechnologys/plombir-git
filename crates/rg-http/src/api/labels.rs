//! Label REST API.
//!
//! GET    /api/v1/repos/:owner/:name/labels      — list labels
//! POST   /api/v1/repos/:owner/:name/labels      — create label
//! GET    /api/v1/repos/:owner/:name/labels/:id  — get label
//! PATCH  /api/v1/repos/:owner/:name/labels/:id  — update label
//! DELETE /api/v1/repos/:owner/:name/labels/:id  — delete label

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::repo_access::{RepoRead, RepoWrite};
use crate::error::AppError;
use crate::AppState;

/// Request body for creating a label.
#[derive(Deserialize, ToSchema)]
pub struct CreateLabelRequest {
    pub name: String,
    pub color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Request body for updating a label.
#[derive(Deserialize, ToSchema)]
pub struct UpdateLabelRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// GET /api/v1/repos/:owner/:name/labels
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/labels",
    tag = "Labels",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
    ),
)]
pub async fn list_labels(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    // The three mutating handlers below already resolve the repository and check
    // the caller; the read pair did not even take `HeaderMap`, which made the
    // label set of a private repository readable by anyone.
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::label::service::list_labels(&state.db, &owner, &name).await {
        Ok(labels) => (StatusCode::OK, Json(serde_json::json!(labels))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/labels/:id
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/labels/{id}",
    tag = "Labels",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn get_label(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    // `Err(_) => not_found(…)` used to swallow the error whole: a failed query
    // was answered as an absent label, and because the value was dropped rather
    // than converted, nothing reached the operator log either. The service now
    // carries `rg_core::error::NotFound` on the two branches that really mean
    // "no such label", so `From` can keep them apart from an outage.
    match rg_core::label::service::get_label(&state.db, &owner, &name, id).await {
        Ok(label) => (StatusCode::OK, Json(serde_json::json!(label))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/repos/:owner/:name/labels
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/labels",
    tag = "Labels",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn create_label(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    RepoWrite { .. }: RepoWrite,
    Json(body): Json<CreateLabelRequest>,
) -> impl IntoResponse {
    match rg_core::label::service::create_label(
        &state.db,
        &owner,
        &name,
        body.name,
        body.color,
        body.description,
    )
    .await
    {
        Ok(label) => (StatusCode::CREATED, Json(serde_json::json!(label))).into_response(),
        // The service's validation failures carry `InvalidRequest` and still
        // answer 400; a missing repository answers 404 and a failed query stays
        // a 5xx, where the blanket `bad_request` called all three malformed.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// PATCH /api/v1/repos/:owner/:name/labels/:id
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/labels/{id}",
    tag = "Labels",
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
pub async fn update_label(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
    RepoWrite { .. }: RepoWrite,
    Json(body): Json<UpdateLabelRequest>,
) -> impl IntoResponse {
    // The label is looked up *within* `owner/name`, the same repository the
    // write check above was about — a bare id let write access to one repository
    // rename a label in any other. `AppError::from` (not a blanket
    // `bad_request`) is what lets the service's "no such label here" arrive as
    // the 404 it is instead of a 400 that claims the request was malformed.
    match rg_core::label::service::update_label(
        &state.db,
        &owner,
        &name,
        id,
        body.name,
        body.color,
        Some(body.description),
    )
    .await
    {
        Ok(label) => (StatusCode::OK, Json(serde_json::json!(label))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/repos/:owner/:name/labels/:id
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/labels/{id}",
    tag = "Labels",
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
pub async fn delete_label(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
    RepoWrite { .. }: RepoWrite,
) -> impl IntoResponse {
    // Repository-scoped for the same reason as `update_label` above.
    match rg_core::label::service::delete_label(&state.db, &owner, &name, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}
