use super::repo_access::{require_admin, require_read};
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use sea_orm::{NotSet, Set};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTagProtectionRequest {
    pub pattern: String,
    pub allowed_user_ids: Option<Vec<i64>>,
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTagProtectionRequest {
    pub allowed_user_ids: Vec<i64>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct TagProtectionResponse {
    pub id: i64,
    pub pattern: String,
    pub allowed_user_ids: Vec<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn response(model: rg_db::entities::protected_tag::Model) -> TagProtectionResponse {
    TagProtectionResponse {
        id: model.id,
        pattern: model.pattern,
        allowed_user_ids: model
            .allowed_user_ids
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or_default(),
        created_at: model.created_at,
        updated_at: model.updated_at,
    }
}
fn valid_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern.len() <= 255
        && !pattern.starts_with("refs/")
        && !pattern.chars().any(char::is_whitespace)
        && !pattern.contains("..")
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/tags/protection", tag = "Tag Protection", params(("owner" = String, Path), ("name" = String, Path)), responses((status = 200, body = [TagProtectionResponse])))]
pub async fn list(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let repo = match require_read(&state, &headers, &owner, &name).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    match rg_db::ops::protected_tag_ops::list_by_repo(&state.db, repo.id).await {
        Ok(items) => (
            StatusCode::OK,
            Json(items.into_iter().map(response).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/tags/protection", tag = "Tag Protection", request_body = CreateTagProtectionRequest, params(("owner" = String, Path), ("name" = String, Path)), responses((status = 201, body = TagProtectionResponse)))]
pub async fn create(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<CreateTagProtectionRequest>,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let pattern = body.pattern.trim();
    if !valid_pattern(pattern) {
        return AppError::bad_request(
            "tag pattern must be a valid ref-name pattern without the refs/tags/ prefix",
        )
        .into_response();
    }
    let now = chrono::Utc::now();
    let model = rg_db::entities::protected_tag::ActiveModel {
        id: NotSet,
        repo_id: Set(repo.id),
        pattern: Set(pattern.to_owned()),
        allowed_user_ids: Set(body
            .allowed_user_ids
            .map(|v| serde_json::to_string(&v).unwrap_or_default())),
        created_at: Set(now),
        updated_at: Set(now),
    };
    match rg_db::ops::protected_tag_ops::create(&state.db, model).await {
        Ok(v) => (StatusCode::CREATED, Json(response(v))).into_response(),
        Err(e) if e.to_string().to_ascii_lowercase().contains("unique") => {
            AppError::conflict("tag protection pattern already exists").into_response()
        }
        Err(e) => AppError::internal(e).into_response(),
    }
}

#[utoipa::path(patch, path = "/repos/{owner}/{name}/tags/protection/{id}", tag = "Tag Protection", request_body = UpdateTagProtectionRequest, params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)), responses((status = 200, body = TagProtectionResponse)))]
pub async fn update(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
    headers: HeaderMap,
    Json(body): Json<UpdateTagProtectionRequest>,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let model = match rg_db::ops::protected_tag_ops::find_by_id(&state.db, id).await {
        Ok(Some(v)) if v.repo_id == repo.id => v,
        Ok(_) => return AppError::not_found("tag protection not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };
    let mut active: rg_db::entities::protected_tag::ActiveModel = model.into();
    active.allowed_user_ids = Set(Some(
        serde_json::to_string(&body.allowed_user_ids).unwrap_or_default(),
    ));
    active.updated_at = Set(chrono::Utc::now());
    match rg_db::ops::protected_tag_ops::update(&state.db, active).await {
        Ok(v) => (StatusCode::OK, Json(response(v))).into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/tags/protection/{id}", tag = "Tag Protection", params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)), responses((status = 204)))]
pub async fn delete(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    match rg_db::ops::protected_tag_ops::find_by_id(&state.db, id).await {
        Ok(Some(v)) if v.repo_id == repo.id => {}
        Ok(_) => return AppError::not_found("tag protection not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    }
    match rg_db::ops::protected_tag_ops::delete_by_id(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_patterns() {
        assert!(valid_pattern("v*"));
        assert!(valid_pattern("release/**"));
        assert!(!valid_pattern("refs/tags/v*"));
        assert!(!valid_pattern("bad pattern"));
    }
}
