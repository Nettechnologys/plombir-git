//! A bot's agent profile — `/users/bots/{bot}/agent` (ADR-0001,
//! card_d4d689acfa70).
//!
//! The bot's owner says here what the bot runs when a person assigns it an
//! issue, on which runner labels, with which model key, in which repositories
//! and under which limits. Only the owner reaches these routes: anybody else's
//! bot is the same `404` as no bot, exactly as on the bot's token routes.
//!
//! The model key goes in and never comes out. A write takes it, seals it with
//! the instance's at-rest key and stores the ciphertext; every answer says
//! only `has_model_key`. Setting one is a credential change, so `PUT` needs a
//! recent login session (`SudoUser`) like minting a token does, and the audit
//! entry records that the key changed, never the key.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::auth::{AuthUser, SudoUser};
use super::bots::{owned_bot, repository_full_names, resolve_repository_names};
use crate::error::AppError;
use crate::AppState;

use rg_core::agent::profile::{self, ModelKey, ProfileUpdate, SavedProfile};

/// A bot's agent profile as its owner submits it.
///
/// Every field but the key is the new value — an omitted limit is the default,
/// not "unchanged" — so what the form showed is what is stored.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentProfileRequest {
    /// Whether assigning the bot starts a run. A preset needs a model key
    /// before it can be enabled.
    #[serde(default)]
    pub enabled: bool,
    /// `claude_code`, `codex` or `custom`.
    pub preset: String,
    /// The image the run starts from. Required for `custom`; for a preset it
    /// replaces the preset's image.
    #[serde(default)]
    pub image: Option<String>,
    /// The command a `custom` profile runs. A preset runs its own.
    #[serde(default)]
    pub command: Option<String>,
    /// Runner labels the run needs; omitted or empty for the preset's.
    #[serde(default)]
    pub runner_tags: Option<Vec<String>>,
    /// A new model API key. Omitted or `null` keeps the stored key.
    #[serde(default)]
    pub model_key: Option<String>,
    /// `true` removes the stored key. Cannot be sent with `model_key`.
    #[serde(default)]
    pub clear_model_key: bool,
    /// Longest run, in seconds: 60 to 14 400, default 1 800.
    #[serde(default)]
    pub timeout_secs: Option<i64>,
    /// Runs the bot may start in a day: 1 to 500, default 20.
    #[serde(default)]
    pub max_runs_per_day: Option<i64>,
    /// Review iterations on one pull request: 1 to 50, default 5.
    #[serde(default)]
    pub max_iterations: Option<i64>,
    /// The repositories the bot may work in, as `owner/name`, each one you can
    /// read. Being a collaborator is not enough on its own: a run starts only
    /// in a repository listed here.
    #[serde(default)]
    pub repositories: Vec<String>,
}

/// A bot's agent profile as its owner sees it. The model key is never in it.
#[derive(Debug, Serialize, ToSchema)]
pub struct AgentProfileResponse {
    pub enabled: bool,
    pub preset: String,
    pub image: Option<String>,
    pub command: Option<String>,
    pub runner_tags: Vec<String>,
    /// Whether a model key is stored. The key itself is never returned.
    pub has_model_key: bool,
    pub timeout_secs: i64,
    pub max_runs_per_day: i64,
    pub max_iterations: i64,
    /// `owner/name` of each repository the bot may work in, as it is called
    /// now.
    pub repositories: Vec<String>,
    /// Whether this instance starts agent runs at all (`[agents].enabled`).
    pub agents_enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

async fn response(state: &AppState, saved: SavedProfile) -> Result<AgentProfileResponse, AppError> {
    let full_names = repository_full_names(state, &saved.repository_ids)
        .await
        .map_err(AppError::from)?;
    let mut repositories: Vec<String> = saved
        .repository_ids
        .iter()
        .filter_map(|id| full_names.get(id).cloned())
        .collect();
    repositories.sort();
    let has_model_key = saved.has_model_key();
    let runner_tags = saved.runner_tags();
    let row = saved.profile;
    Ok(AgentProfileResponse {
        enabled: row.enabled,
        preset: row.preset,
        image: row.image,
        command: row.command,
        runner_tags,
        has_model_key,
        timeout_secs: row.timeout_secs,
        max_runs_per_day: row.max_runs_per_day,
        max_iterations: row.max_iterations,
        repositories,
        agents_enabled: state.agents_enabled,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// GET /api/v1/users/bots/{bot}/agent
#[utoipa::path(
    get,
    path = "/users/bots/{bot}/agent",
    tag = "Users",
    params(("bot" = String, Path, description = "The bot's username")),
    responses(
        (status = 200, description = "The bot's agent profile", body = AgentProfileResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such bot of the caller's, or it has no agent profile", body = serde_json::Value),
    ),
)]
pub async fn get_agent_profile(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(bot): Path<String>,
) -> Response {
    let bot = match owned_bot(&state, user_id, &bot).await {
        Ok(bot) => bot,
        Err(error) => return error.into_response(),
    };
    let saved = match profile::get(&state.db, &bot).await {
        Ok(Some(saved)) => saved,
        Ok(None) => return AppError::not_found("this bot has no agent profile").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    match response(&state, saved).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// PUT /api/v1/users/bots/{bot}/agent
///
/// Creates the profile or replaces it. The model key is kept unless
/// `model_key` or `clear_model_key` says otherwise.
#[utoipa::path(
    put,
    path = "/users/bots/{bot}/agent",
    tag = "Users",
    params(("bot" = String, Path, description = "The bot's username")),
    request_body = AgentProfileRequest,
    responses(
        (status = 200, description = "Saved", body = AgentProfileResponse),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "A login session is required to change a model key", body = serde_json::Value),
        (status = 404, description = "No such bot of the caller's", body = serde_json::Value),
    ),
)]
pub async fn put_agent_profile(
    State(state): State<AppState>,
    SudoUser(user_id): SudoUser,
    headers: HeaderMap,
    Path(bot): Path<String>,
    Json(body): Json<AgentProfileRequest>,
) -> Response {
    let audit_actor = match super::access_audit::grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let bot = match owned_bot(&state, user_id, &bot).await {
        Ok(bot) => bot,
        Err(error) => return error.into_response(),
    };
    let model_key = match (body.model_key, body.clear_model_key) {
        (Some(_), true) => {
            return AppError::bad_request("send either model_key or clear_model_key, not both")
                .into_response();
        }
        (Some(key), false) => ModelKey::Set(key),
        (None, true) => ModelKey::Clear,
        (None, false) => ModelKey::Keep,
    };
    if body.repositories.len() > profile::MAX_REPOSITORIES {
        return AppError::bad_request(format!(
            "an agent profile may list at most {} repositories",
            profile::MAX_REPOSITORIES
        ))
        .into_response();
    }
    let repository_ids = match resolve_repository_names(&state, user_id, &body.repositories).await {
        Ok(ids) => ids,
        Err(error) => return error.into_response(),
    };
    let key_change = match &model_key {
        ModelKey::Keep => "kept",
        ModelKey::Set(_) => "set",
        ModelKey::Clear => "cleared",
    };
    let encryption_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let saved = match profile::save(
        &state.db,
        &encryption_key,
        &bot,
        ProfileUpdate {
            enabled: body.enabled,
            preset: body.preset,
            image: body.image,
            command: body.command,
            runner_tags: body.runner_tags,
            model_key,
            timeout_secs: body.timeout_secs,
            max_runs_per_day: body.max_runs_per_day,
            max_iterations: body.max_iterations,
            repository_ids,
        },
    )
    .await
    {
        Ok(saved) => saved,
        Err(error) => return AppError::from(error).into_response(),
    };
    // What the profile now does and that the key changed — never the key.
    rg_core::audit::record(
        &state.db,
        &audit_actor,
        "user.update_bot_agent_profile",
        Some("user"),
        Some(bot.id),
        Some(&bot.username),
        Some(&headers),
        Some(serde_json::json!({
            "enabled": saved.profile.enabled,
            "preset": saved.profile.preset,
            "image": saved.profile.image,
            "runner_tags": saved.runner_tags(),
            "model_key": key_change,
            "has_model_key": saved.has_model_key(),
            "repository_ids": saved.repository_ids,
            "timeout_secs": saved.profile.timeout_secs,
            "max_runs_per_day": saved.profile.max_runs_per_day,
            "max_iterations": saved.profile.max_iterations,
        })),
    )
    .await;
    match response(&state, saved).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// DELETE /api/v1/users/bots/{bot}/agent
///
/// Deletes the profile, its repository list and its model key. The bot stays.
#[utoipa::path(
    delete,
    path = "/users/bots/{bot}/agent",
    tag = "Users",
    params(("bot" = String, Path, description = "The bot's username")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such bot of the caller's, or it has no agent profile", body = serde_json::Value),
    ),
)]
pub async fn delete_agent_profile(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Path(bot): Path<String>,
) -> Response {
    let audit_actor = match super::access_audit::grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let bot = match owned_bot(&state, user_id, &bot).await {
        Ok(bot) => bot,
        Err(error) => return error.into_response(),
    };
    match profile::delete(&state.db, &bot).await {
        Ok(true) => {}
        Ok(false) => {
            return AppError::not_found("this bot has no agent profile").into_response();
        }
        Err(error) => return AppError::from(error).into_response(),
    }
    rg_core::audit::record(
        &state.db,
        &audit_actor,
        "user.delete_bot_agent_profile",
        Some("user"),
        Some(bot.id),
        Some(&bot.username),
        Some(&headers),
        None,
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}
