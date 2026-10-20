//! `vars.*`: the configuration variables an Actions workflow reads as
//! `${{ vars.NAME }}`, set on a repository or on an organization.
//!
//! A repository's variable overrides its organization's of the same name, as
//! on GitHub. A value is configuration, not a credential: it is shown back
//! here and a job log prints it — what must not be printed is a secret.

use crate::api::access_audit::{grant_actor, record_grant};
use crate::api::orgs::OrgAdmin;
use crate::api::repo_access::RepoAdmin;
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use rg_db::ops::ci_variable_ops::{self, Owner, Variable, Written};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The longest value a variable holds, in bytes — GitHub's own ceiling.
pub const MAX_VARIABLE_VALUE_BYTES: usize = 48 * 1024;

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutVariableRequest {
    pub value: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VariableResponse {
    pub name: String,
    pub value: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<Variable> for VariableResponse {
    fn from(variable: Variable) -> Self {
        Self {
            name: variable.name,
            value: variable.value,
            created_at: variable.created_at,
            updated_at: variable.updated_at,
        }
    }
}

/// `[A-Z_][A-Z0-9_]*`, at most 100 characters, and not `GITHUB_*` — GitHub's
/// rule, so a variable that works there works here. Unlike a secret's name it
/// never becomes an environment variable, so a runner name is not reserved.
pub(crate) fn valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z'))
        && chars.all(|ch| matches!(ch, '_' | 'A'..='Z' | '0'..='9'))
        && name.len() <= 100
        && !name.starts_with("GITHUB_")
}

fn invalid_request(name: &str, value: Option<&str>) -> Option<AppError> {
    if !valid_variable_name(name) {
        return Some(AppError::bad_request(
            "variable names must match [A-Z_][A-Z0-9_]*, be at most 100 characters, and not \
             start with GITHUB_",
        ));
    }
    if value.is_some_and(|value| value.is_empty() || value.len() > MAX_VARIABLE_VALUE_BYTES) {
        return Some(AppError::bad_request(format!(
            "variable value must contain 1-{MAX_VARIABLE_VALUE_BYTES} bytes"
        )));
    }
    None
}

async fn list_of(state: &AppState, owner: Owner) -> axum::response::Response {
    match ci_variable_ops::list(&state.db, owner).await {
        Ok(items) => (
            StatusCode::OK,
            Json(
                items
                    .into_iter()
                    .map(VariableResponse::from)
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

async fn one_of(state: &AppState, owner: Owner, name: &str) -> axum::response::Response {
    match ci_variable_ops::list(&state.db, owner).await {
        Ok(items) => match items.into_iter().find(|item| item.name == name) {
            Some(item) => (StatusCode::OK, Json(VariableResponse::from(item))).into_response(),
            None => AppError::not_found("CI variable not found").into_response(),
        },
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/actions/variables", tag = "CI/CD", params(("owner" = String, Path), ("name" = String, Path)), responses((status = 200, body = [VariableResponse]), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn list(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    list_of(&state, Owner::Repository(repo.id)).await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/actions/variables/{variable_name}", tag = "CI/CD", params(("owner" = String, Path), ("name" = String, Path), ("variable_name" = String, Path)), responses((status = 200, body = VariableResponse), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn get(
    State(state): State<AppState>,
    Path((_, _, variable_name)): Path<(String, String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    one_of(&state, Owner::Repository(repo.id), &variable_name).await
}

#[utoipa::path(put, path = "/repos/{owner}/{name}/actions/variables/{variable_name}", tag = "CI/CD", request_body = PutVariableRequest, params(("owner" = String, Path), ("name" = String, Path), ("variable_name" = String, Path)), responses((status = 201, description = "Created"), (status = 204, description = "Updated"), (status = 400, body = serde_json::Value), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn put(
    State(state): State<AppState>,
    Path((owner, _, variable_name)): Path<(String, String, String)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<PutVariableRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Some(error) = invalid_request(&variable_name, Some(&body.value)) {
        return error.into_response();
    }
    match ci_variable_ops::upsert(
        &state.db,
        Owner::Repository(repo.id),
        &variable_name,
        &body.value,
        actor_id,
    )
    .await
    {
        Ok(written) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.set_ci_variable",
                &owner,
                &repo,
                &headers,
                serde_json::json!({
                    "variable": variable_name,
                    "updated": written == Written::Updated,
                }),
            )
            .await;
            written_status(written).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/actions/variables/{variable_name}", tag = "CI/CD", params(("owner" = String, Path), ("name" = String, Path), ("variable_name" = String, Path)), responses((status = 204), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn delete(
    State(state): State<AppState>,
    Path((owner, _, variable_name)): Path<(String, String, String)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    match ci_variable_ops::delete(&state.db, Owner::Repository(repo.id), &variable_name).await {
        Ok(true) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.remove_ci_variable",
                &owner,
                &repo,
                &headers,
                serde_json::json!({ "variable": variable_name }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => AppError::not_found("CI variable not found").into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// `201` for a variable this call created, `204` for one whose value it
/// replaced — Gitea's split between its create and update calls, in one verb.
fn written_status(written: Written) -> StatusCode {
    match written {
        Written::Created => StatusCode::CREATED,
        Written::Updated => StatusCode::NO_CONTENT,
    }
}

async fn record_org_change(
    state: &AppState,
    actor: &rg_core::audit::AuditActor,
    org: &rg_db::entities::organization::Model,
    action: &str,
    headers: &HeaderMap,
    details: serde_json::Value,
) {
    rg_core::audit::record(
        &state.db,
        actor,
        action,
        Some("org"),
        Some(org.id),
        Some(&org.name),
        Some(headers),
        Some(details),
    )
    .await;
}

#[utoipa::path(get, path = "/orgs/{name}/actions/variables", tag = "CI/CD", params(("name" = String, Path)), responses((status = 200, body = [VariableResponse]), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn org_list(
    State(state): State<AppState>,
    OrgAdmin { org, .. }: OrgAdmin,
) -> impl IntoResponse {
    list_of(&state, Owner::Organization(org.id)).await
}

#[utoipa::path(get, path = "/orgs/{name}/actions/variables/{variable_name}", tag = "CI/CD", params(("name" = String, Path), ("variable_name" = String, Path)), responses((status = 200, body = VariableResponse), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn org_get(
    State(state): State<AppState>,
    Path((_, variable_name)): Path<(String, String)>,
    OrgAdmin { org, .. }: OrgAdmin,
) -> impl IntoResponse {
    one_of(&state, Owner::Organization(org.id), &variable_name).await
}

#[utoipa::path(put, path = "/orgs/{name}/actions/variables/{variable_name}", tag = "CI/CD", request_body = PutVariableRequest, params(("name" = String, Path), ("variable_name" = String, Path)), responses((status = 201, description = "Created"), (status = 204, description = "Updated"), (status = 400, body = serde_json::Value), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn org_put(
    State(state): State<AppState>,
    Path((_, variable_name)): Path<(String, String)>,
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
    Json(body): Json<PutVariableRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Some(error) = invalid_request(&variable_name, Some(&body.value)) {
        return error.into_response();
    }
    match ci_variable_ops::upsert(
        &state.db,
        Owner::Organization(org.id),
        &variable_name,
        &body.value,
        actor_id,
    )
    .await
    {
        Ok(written) => {
            record_org_change(
                &state,
                &audit_actor,
                &org,
                "org.set_ci_variable",
                &headers,
                serde_json::json!({
                    "variable": variable_name,
                    "updated": written == Written::Updated,
                }),
            )
            .await;
            written_status(written).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(delete, path = "/orgs/{name}/actions/variables/{variable_name}", tag = "CI/CD", params(("name" = String, Path), ("variable_name" = String, Path)), responses((status = 204), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn org_delete(
    State(state): State<AppState>,
    Path((_, variable_name)): Path<(String, String)>,
    OrgAdmin { org, actor_id }: OrgAdmin,
    headers: HeaderMap,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    match ci_variable_ops::delete(&state.db, Owner::Organization(org.id), &variable_name).await {
        Ok(true) => {
            record_org_change(
                &state,
                &audit_actor,
                &org,
                "org.remove_ci_variable",
                &headers,
                serde_json::json!({ "variable": variable_name }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => AppError::not_found("CI variable not found").into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_names_follow_githubs_rule() {
        assert!(valid_variable_name("DEPLOY_REGION"));
        assert!(valid_variable_name("_X1"));
        assert!(!valid_variable_name("deploy_region"));
        assert!(!valid_variable_name("1X"));
        assert!(!valid_variable_name("GITHUB_TOKEN"));
        assert!(!valid_variable_name(&"X".repeat(101)));
    }
}
