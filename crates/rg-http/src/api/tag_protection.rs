use super::repo_access::{RepoAdmin, RepoRead};
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
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

/// Decode the stored allow-list of one tag-protection rule.
///
/// `NULL` is a configured absence — no allow-list — and reads as an empty vec.
/// A present-but-undecodable column must not read as one too: `[]` would show
/// the operator a rule that admits nobody, and a `GET` → edit → `PATCH` round
/// trip would then persist that emptiness over a list that was only unreadable.
fn decode_allowed_user_ids(
    model: &rg_db::entities::protected_tag::Model,
) -> Result<Vec<i64>, AppError> {
    let Some(json) = model.allowed_user_ids.as_deref() else {
        return Ok(Vec::new());
    };
    serde_json::from_str(json).map_err(|error| {
        tracing::error!(
            protected_tag_id = model.id,
            error = %error,
            "stored allowed_user_ids is not a JSON array of user ids"
        );
        AppError::internal("stored tag protection allow-list is unreadable")
    })
}

fn response(
    model: rg_db::entities::protected_tag::Model,
) -> Result<TagProtectionResponse, AppError> {
    Ok(TagProtectionResponse {
        id: model.id,
        allowed_user_ids: decode_allowed_user_ids(&model)?,
        pattern: model.pattern,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}
fn valid_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern.len() <= 255
        && !pattern.starts_with("refs/")
        && !pattern.chars().any(char::is_whitespace)
        && !pattern.contains("..")
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/tags/protection", tag = "Tag Protection", params(("owner" = String, Path), ("name" = String, Path)), responses((status = 200, body = [TagProtectionResponse])))]
pub async fn list(State(state): State<AppState>, RepoRead { repo }: RepoRead) -> impl IntoResponse {
    match rg_db::ops::protected_tag_ops::list_by_repo(&state.db, repo.id).await {
        Ok(items) => match items
            .into_iter()
            .map(response)
            .collect::<Result<Vec<_>, AppError>>()
        {
            Ok(items) => (StatusCode::OK, Json(items)).into_response(),
            Err(e) => e.into_response(),
        },
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/tags/protection", tag = "Tag Protection", request_body = CreateTagProtectionRequest, params(("owner" = String, Path), ("name" = String, Path)), responses((status = 201, body = TagProtectionResponse)))]
pub async fn create(
    State(state): State<AppState>,
    RepoAdmin { repo, .. }: RepoAdmin,
    Json(body): Json<CreateTagProtectionRequest>,
) -> impl IntoResponse {
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
        Ok(v) => match response(v) {
            Ok(body) => (StatusCode::CREATED, Json(body)).into_response(),
            Err(e) => e.into_response(),
        },
        // `(repo_id, pattern)` is unique and nothing pre-checks it, so every
        // repeat of an existing pattern arrives here.
        Err(e) if rg_db::is_unique_violation_anyhow(&e) => {
            AppError::conflict("tag protection pattern already exists").into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(patch, path = "/repos/{owner}/{name}/tags/protection/{id}", tag = "Tag Protection", request_body = UpdateTagProtectionRequest, params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)), responses((status = 200, body = TagProtectionResponse)))]
pub async fn update(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, .. }: RepoAdmin,
    Json(body): Json<UpdateTagProtectionRequest>,
) -> impl IntoResponse {
    let model = match tag_protection_in_repo(&state, repo.id, id).await {
        Ok(model) => model,
        Err(e) => return e.into_response(),
    };
    let mut active: rg_db::entities::protected_tag::ActiveModel = model.into();
    active.allowed_user_ids = Set(Some(
        serde_json::to_string(&body.allowed_user_ids).unwrap_or_default(),
    ));
    active.updated_at = Set(chrono::Utc::now());
    match rg_db::ops::protected_tag_ops::update(&state.db, active).await {
        Ok(v) => match response(v) {
            Ok(body) => (StatusCode::OK, Json(body)).into_response(),
            Err(e) => e.into_response(),
        },
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/tags/protection/{id}", tag = "Tag Protection", params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)), responses((status = 204), (status = 404, description = "No such rule, or it belongs to another repository", body = serde_json::Value)))]
pub async fn delete(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    if let Err(e) = tag_protection_in_repo(&state, repo.id, id).await {
        return e.into_response();
    }
    // That lookup and this `DELETE` are two statements, so a concurrent delete
    // can land in between; the 204 therefore comes from `rows_affected` rather
    // than from the row having existed a moment ago.
    match rg_db::ops::protected_tag_ops::delete_by_id(&state.db, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => AppError::not_found("tag protection rule not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Fetch a tag protection rule and re-anchor it to the repository the caller
/// was authorized for.
///
/// `{id}` is a global `protected_tags` primary key while `RepoAdmin` only ever
/// proves something about `{owner}/{name}`, so administering one repository
/// must not reach another one's rules. A mismatch answers 404 rather than 403:
/// a 403 would still confirm the id exists, which is most of what an id-walking
/// caller wants to learn.
///
/// `update` and `delete` each spelled this comparison inline. A named helper is
/// the form `global_id_anchor_guard` can read — a comparison is not — so a third
/// route that forgets the anchor now fails the build rather than review.
async fn tag_protection_in_repo(
    state: &AppState,
    repo_id: i64,
    protection_id: i64,
) -> Result<rg_db::entities::protected_tag::Model, AppError> {
    match rg_db::ops::protected_tag_ops::find_by_id(&state.db, protection_id).await {
        Ok(Some(v)) if v.repo_id == repo_id => Ok(v),
        Ok(_) => Err(AppError::not_found("tag protection not found")),
        Err(e) => Err(AppError::from(e)),
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
