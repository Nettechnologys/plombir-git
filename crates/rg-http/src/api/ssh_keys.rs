//! Authenticated user SSH key management.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use sea_orm::Set;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{error::AppError, AppState};

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSshKeyRequest {
    pub title: String,
    #[serde(alias = "key")]
    pub public_key: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SshKeyResponse {
    pub id: i64,
    pub title: String,
    pub public_key: String,
    pub fingerprint: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<rg_db::entities::ssh_key::Model> for SshKeyResponse {
    fn from(key: rg_db::entities::ssh_key::Model) -> Self {
        Self {
            id: key.id,
            title: key.title,
            public_key: key.public_key,
            fingerprint: key.fingerprint,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

fn authenticated_user_id(headers: &HeaderMap, state: &AppState) -> Result<i64, AppError> {
    super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))
}

#[utoipa::path(
    get,
    path = "/users/ssh-keys",
    tag = "Users",
    responses(
        (status = 200, description = "SSH keys for the current user", body = [SshKeyResponse]),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    )
)]
pub async fn list_ssh_keys(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let user_id = match authenticated_user_id(&headers, &state) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };

    match rg_db::ops::ssh_key_ops::list_by_user(&state.db, user_id).await {
        Ok(keys) => (
            StatusCode::OK,
            Json(
                keys.into_iter()
                    .map(SshKeyResponse::from)
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/users/ssh-keys",
    tag = "Users",
    request_body = CreateSshKeyRequest,
    responses(
        (status = 201, description = "SSH key added", body = SshKeyResponse),
        (status = 400, description = "Invalid SSH key", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "SSH key already registered", body = serde_json::Value),
    )
)]
pub async fn create_ssh_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateSshKeyRequest>,
) -> impl IntoResponse {
    let user_id = match authenticated_user_id(&headers, &state) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };

    let title = body.title.trim();
    if title.is_empty() {
        return AppError::bad_request("SSH key title cannot be empty").into_response();
    }
    if title.chars().count() > 100 {
        return AppError::bad_request("SSH key title cannot exceed 100 characters").into_response();
    }

    let public_key = body.public_key.trim();
    if public_key.len() > 16_384 {
        return AppError::bad_request("SSH public key is too large").into_response();
    }
    let fingerprint = match rg_core::auth::ssh_key::fingerprint_from_openssh(public_key) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return AppError::bad_request(error).into_response(),
    };

    match rg_db::ops::ssh_key_ops::find_by_fingerprint(&state.db, &fingerprint).await {
        Ok(Some(_)) => {
            return AppError::conflict("this SSH key is already registered").into_response()
        }
        Ok(None) => {}
        Err(error) => return AppError::internal(error).into_response(),
    }
    match rg_db::ops::deploy_key_ops::find_by_fingerprint(&state.db, &fingerprint).await {
        Ok(Some(_)) => {
            return AppError::conflict("this SSH key is already registered as a deploy key")
                .into_response()
        }
        Ok(None) => {}
        Err(error) => return AppError::internal(error).into_response(),
    }

    let model = rg_db::entities::ssh_key::ActiveModel {
        id: sea_orm::NotSet,
        user_id: Set(user_id),
        title: Set(title.to_string()),
        public_key: Set(public_key.to_string()),
        fingerprint: Set(fingerprint),
        created_at: Set(chrono::Utc::now()),
        last_used_at: Set(None),
    };

    match rg_db::ops::ssh_key_ops::create(&state.db, model).await {
        Ok(key) => (StatusCode::CREATED, Json(SshKeyResponse::from(key))).into_response(),
        // The fingerprint has a unique constraint. Treat a concurrent insert
        // as a conflict without exposing database internals.
        Err(error) if error.to_string().to_ascii_lowercase().contains("unique") => {
            AppError::conflict("this SSH key is already registered").into_response()
        }
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/users/ssh-keys/{id}",
    tag = "Users",
    params(("id" = i64, Path, description = "SSH key id")),
    responses(
        (status = 204, description = "SSH key deleted"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Cannot delete another user's key", body = serde_json::Value),
        (status = 404, description = "SSH key not found", body = serde_json::Value),
    )
)]
pub async fn delete_ssh_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let user_id = match authenticated_user_id(&headers, &state) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };

    let key = match rg_db::ops::ssh_key_ops::find_by_id(&state.db, id).await {
        Ok(Some(key)) => key,
        Ok(None) => return AppError::not_found("SSH key not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    if key.user_id != user_id {
        return AppError::forbidden("you can only delete your own SSH keys").into_response();
    }

    match rg_db::ops::ssh_key_ops::delete_by_id(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}
