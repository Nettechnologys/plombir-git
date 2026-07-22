use super::repo_access::require_admin;
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutSecretRequest {
    pub value: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SecretResponse {
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn response(secret: rg_db::entities::ci_secret::Model) -> SecretResponse {
    SecretResponse {
        name: secret.name,
        created_at: secret.created_at,
        updated_at: secret.updated_at,
    }
}

pub(crate) fn valid_secret_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z'))
        && chars.all(|ch| matches!(ch, '_' | 'A'..='Z' | '0'..='9'))
        && name.len() <= 100
        && !matches!(
            name,
            "CI" | "IRONFORGE"
                | "CI_PIPELINE_ID"
                | "CI_COMMIT_SHA"
                | "CI_SHA"
                | "CI_REF"
                | "CI_EVENT"
                | "CI_JOB_TOKEN"
                | "CI_OIDC_TOKEN_URL"
                | "HOME"
                | "PATH"
        )
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/actions/secrets", tag = "CI/CD", params(("owner" = String, Path), ("name" = String, Path)), responses((status = 200, body = [SecretResponse]), (status = 403, body = serde_json::Value)))]
pub async fn list(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    match rg_db::ops::ci_secret_ops::list_by_repo(&state.db, repo.id).await {
        Ok(items) => (
            StatusCode::OK,
            Json(items.into_iter().map(response).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

#[utoipa::path(put, path = "/repos/{owner}/{name}/actions/secrets/{secret_name}", tag = "CI/CD", request_body = PutSecretRequest, params(("owner" = String, Path), ("name" = String, Path), ("secret_name" = String, Path)), responses((status = 201, body = SecretResponse), (status = 400, body = serde_json::Value), (status = 403, body = serde_json::Value)))]
pub async fn put(
    State(state): State<AppState>,
    Path((owner, name, secret_name)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(body): Json<PutSecretRequest>,
) -> impl IntoResponse {
    let (repo, actor_id) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    if !valid_secret_name(&secret_name) {
        return AppError::bad_request("secret names must match [A-Z_][A-Z0-9_]*, be at most 100 characters, and not use reserved CI names").into_response();
    }
    if body.value.len() < 4 || body.value.len() > 65_536 {
        return AppError::bad_request("secret value must contain 4-65536 bytes").into_response();
    }
    let key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let encrypted = match rg_core::auth::encryption::encrypt(&body.value, &key) {
        Ok(v) => v,
        Err(e) => return AppError::internal(e).into_response(),
    };
    match rg_db::ops::ci_secret_ops::upsert(&state.db, repo.id, &secret_name, &encrypted, actor_id)
        .await
    {
        Ok(item) => (StatusCode::CREATED, Json(response(item))).into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/actions/secrets/{secret_name}", tag = "CI/CD", params(("owner" = String, Path), ("name" = String, Path), ("secret_name" = String, Path)), responses((status = 204), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn delete(
    State(state): State<AppState>,
    Path((owner, name, secret_name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    match rg_db::ops::ci_secret_ops::delete_by_repo_and_name(&state.db, repo.id, &secret_name).await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => AppError::not_found("CI secret not found").into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_secret_names() {
        assert!(valid_secret_name("DEPLOY_TOKEN_2"));
        assert!(!valid_secret_name("deploy_token"));
        assert!(!valid_secret_name("CI_JOB_TOKEN"));
    }
}
