//! External CI/CD webhook receiver.
//!
//! Receives webhook events from Jenkins, GitHub Actions, or any generic CI/CD
//! system and re-publishes them as commit status updates.
//!
//! POST /api/v1/repos/{owner}/{name}/webhooks/external/ci
//!
//! Expected JSON body:
//! ```json
//! {
//!   "context": "jenkins/my-pipeline",
//!   "state": "success" | "failure" | "pending" | "error",
//!   "description": "Build #42 passed",
//!   "target_url": "https://jenkins.example.com/job/42"
//! }
//! ```

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ExternalCiWebhook {
    pub context: String,
    pub state: String, // "success", "failure", "pending", "error"
    pub description: Option<String>,
    pub target_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalCiResponse {
    pub id: i64,
    pub status: String,
}

/// POST /api/v1/repos/{owner}/{name}/webhooks/external/ci
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/webhooks/external/ci",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
    ),
    request_body = ExternalCiWebhook,
    responses(
        (status = 200, description = "Commit status created", body = ExternalCiResponse),
        (status = 400, description = "Invalid state or input"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "Repository not found"),
    ),
)]
pub async fn external_ci_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<ExternalCiWebhook>,
) -> impl IntoResponse {
    // Authenticate
    let Some(user_id) = crate::api::auth::extract_user_id(&headers, &state.jwt_secret) else {
        return crate::error::AppError::unauthorized("authentication required").into_response();
    };

    // Validate state
    let valid_states = ["success", "failure", "pending", "error"];
    if !valid_states.contains(&body.state.as_str()) {
        return crate::error::AppError::bad_request(format!(
            "invalid state '{}': must be one of {:?}",
            body.state, valid_states
        ))
        .into_response();
    }

    // Resolve repo
    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return crate::error::AppError::not_found("repository not found").into_response()
        }
        Err(e) => return crate::error::AppError::internal(e.to_string()).into_response(),
    };

    // Create commit status (without sha — will be associated later via push)
    // Clone fields before moving into ActiveModel
    let context = body.context.clone();
    let state_val = body.state.clone();
    let description = body.description.clone();
    let target_url = body.target_url.clone();

    let status = match rg_db::ops::commit_status_ops::create_or_update(
        &state.db,
        repo.id,
        "", // empty sha — callers should set via a follow-up webhook or API
        &body.context,
        rg_db::entities::commit_status::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: sea_orm::Set(repo.id),
            sha: sea_orm::Set(String::new()),
            context: sea_orm::Set(context),
            state: sea_orm::Set(state_val),
            description: sea_orm::Set(description),
            target_url: sea_orm::Set(target_url),
            creator_id: sea_orm::Set(user_id),
            created_at: sea_orm::Set(chrono::Utc::now()),
            updated_at: sea_orm::Set(chrono::Utc::now()),
        },
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return crate::error::AppError::internal(e.to_string()).into_response(),
    };

    (
        StatusCode::OK,
        Json(ExternalCiResponse {
            id: status.id,
            status: status.state,
        }),
    )
        .into_response()
}
