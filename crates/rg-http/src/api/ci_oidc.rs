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
    if let Some(url) = state.external_url.as_deref() {
        return Ok(format!("{}/api/v1/ci/oidc", url.trim_end_matches('/')));
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            AppError::bad_request("Host header is required when external_url is not configured")
        })?;
    Ok(format!("http://{host}/api/v1/ci/oidc"))
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
    Json(serde_json::json!({ "keys": [rg_core::auth::ci_oidc::jwk(&state.jwt_secret)] }))
}

#[derive(Serialize)]
pub struct TokenResponse {
    value: String,
    expires_at: i64,
}

#[utoipa::path(get, path = "/ci/oidc/token", tag = "CI/CD", params(("audience" = String, Query, description = "Intended relying party")), responses((status = 200, description = "Short-lived workload identity token"), (status = 401, description = "Invalid CI job token"), (status = 403, description = "Job is not running")))]
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
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, claims.job_id).await {
        Ok(Some(job)) if matches!(job.status.as_str(), "assigned" | "running") => job,
        Ok(Some(_)) => return AppError::forbidden("CI job is not running").into_response(),
        Ok(None) => return AppError::unauthorized("CI job no longer exists").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) => stage,
        Ok(None) => return AppError::unauthorized("CI stage no longer exists").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id).await
    {
        Ok(Some(pipeline))
            if pipeline.id == claims.pipeline_id && pipeline.repo_id == claims.repo_id =>
        {
            pipeline
        }
        Ok(_) => {
            return AppError::unauthorized("CI token resource binding mismatch").into_response()
        }
        Err(error) => return AppError::internal(error).into_response(),
    };
    let issuer = match issuer(&state, &headers) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let (value, expires_at) = match rg_core::auth::ci_oidc::issue(
        &state.jwt_secret,
        &issuer,
        &query.audience,
        pipeline.repo_id,
        pipeline.id,
        job.id,
        &pipeline.ref_name,
        &pipeline.commit_sha,
    ) {
        Ok(value) => value,
        Err(error) => return AppError::internal(error).into_response(),
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
