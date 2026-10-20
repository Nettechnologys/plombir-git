//! OIDC discovery, JWKS, and audience-bound token exchange for CI jobs.

use crate::{error::AppError, AppState};
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct TokenQuery {
    audience: String,
}

fn issuer(state: &AppState, headers: &HeaderMap) -> Result<String, AppError> {
    let base = crate::public_url::require_public_base_url(state, headers)?;
    Ok(format!("{base}/api/v1/ci/oidc"))
}
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}
fn valid_audience(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_whitespace)
}

#[derive(Serialize)]
pub struct DiscoveryResponse {
    issuer: String,
    jwks_uri: String,
    token_endpoint: String,
    response_types_supported: [&'static str; 1],
    subject_types_supported: [&'static str; 1],
    id_token_signing_alg_values_supported: [&'static str; 1],
    scopes_supported: [&'static str; 1],
}

#[utoipa::path(get, path = "/ci/oidc/.well-known/openid-configuration", tag = "CI/CD", responses((status = 200, description = "OIDC discovery document")))]
pub async fn discovery(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let issuer = match issuer(&state, &headers) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    Json(DiscoveryResponse {
        jwks_uri: format!("{issuer}/jwks"),
        token_endpoint: format!("{issuer}/token"),
        issuer,
        response_types_supported: ["id_token"],
        subject_types_supported: ["public"],
        id_token_signing_alg_values_supported: ["EdDSA"],
        scopes_supported: ["openid"],
    })
    .into_response()
}

#[utoipa::path(get, path = "/ci/oidc/jwks", tag = "CI/CD", responses((status = 200, description = "Public Ed25519 signing keys")))]
pub async fn jwks(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({ "keys": [rg_core::auth::ci_oidc::jwk(&state.instance_key)] }))
}

#[derive(Serialize)]
pub struct TokenResponse {
    value: String,
    expires_at: i64,
}

#[utoipa::path(get, path = "/ci/oidc/token", tag = "CI/CD", params(("audience" = String, Query, description = "Intended relying party")), responses((status = 200, description = "Short-lived workload identity token"), (status = 401, description = "Invalid CI job token"), (status = 403, description = "Job is not running, or its token lacks id-token:write")))]
pub async fn token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> impl IntoResponse {
    if !valid_audience(&query.audience) {
        return AppError::bad_request("audience must contain 1-255 non-whitespace characters")
            .into_response();
    }
    let Some(job_token) = bearer(&headers) else {
        return AppError::unauthorized("CI_JOB_TOKEN bearer token required").into_response();
    };
    let Some(claims) =
        rg_core::auth::ci_token::validate_ci_token_signature(job_token, &state.jwt_secret)
    else {
        return AppError::unauthorized("invalid or expired CI job token").into_response();
    };
    // A workload identity is what a cloud role trusts this repository by, so a
    // token gets one only when its job asked for it — `id-token: write` in an
    // Actions workflow, every native job — and not merely for running: every
    // step of the job holds the job token, a downloaded action's included.
    if !claims.has_scope(rg_core::ci::ID_TOKEN_SCOPE) {
        return AppError::forbidden(
            "this job's token may not be exchanged for a workload identity; declare \
             `permissions: id-token: write` on the job",
        )
        .into_response();
    }
    // The binding lives in `api::auth` because the repository read gate needs
    // exactly the same question answered — a signature that is still in date
    // says nothing about whether the job it names is still running.
    let (job, pipeline) = match super::auth::ci_job_binding(&state, &claims).await {
        Ok(pair) => pair,
        Err(error) => return error.into_response(),
    };
    let issuer = match issuer(&state, &headers) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let (owner, repository) =
        match rg_core::repo::service::repository_identity(&state.db, pipeline.repo_id).await {
            Ok(identity) => identity,
            Err(error) => return AppError::from(error).into_response(),
        };
    // A lookup that failed is a failed request, not an absent actor: a token
    // must not state less than it should because the database hiccuped.
    let actor = match pipeline.triggered_by {
        Some(user_id) => match rg_db::ops::user_ops::find_by_id(&state.db, user_id).await {
            Ok(user) => user.map(|user| user.username),
            Err(error) => return AppError::from(error).into_response(),
        },
        None => None,
    };
    // The name the job was attached under, whose gate released it — not the
    // environment row's current name, which a later rename could point at a
    // stronger environment. Only a job that really has an environment row
    // carries one.
    let environment = job
        .environment_id
        .and(job.environment_name.as_deref())
        .filter(|name| !name.is_empty());
    let (value, expires_at) = match rg_core::auth::ci_oidc::issue(
        &state.instance_key,
        &issuer,
        &query.audience,
        &rg_core::auth::ci_oidc::JobIdentity {
            owner: &owner,
            repository: &repository,
            repository_id: pipeline.repo_id,
            pipeline_id: pipeline.id,
            job_id: job.id,
            ref_name: &pipeline.ref_name,
            sha: &pipeline.commit_sha,
            environment,
            actor: actor.as_deref(),
        },
    ) {
        Ok(value) => value,
        Err(error) => return AppError::from(error).into_response(),
    };
    let mut response = Json(TokenResponse { value, expires_at }).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audience_validation_is_strict() {
        assert!(valid_audience("sts.amazonaws.com"));
        assert!(!valid_audience(""));
        assert!(!valid_audience("two words"));
    }
}
