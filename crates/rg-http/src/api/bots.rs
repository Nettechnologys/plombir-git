//! Bot accounts and their tokens — `/users/bots/...` (card_60a80311d512).
//!
//! A person creates a bot for an agent, grants it repository access the usual
//! way (as a collaborator, by name), and mints its tokens here. The bot cannot
//! do any of this for itself: its tokens carry the `repo` scope only, and every
//! route below is a `user`-scope route.
//!
//! The narrowing a token can carry — repositories, MCP tools, protected
//! branches — is shared with the person's own tokens (`POST /users/tokens`),
//! so it is defined here once: [`TokenNarrowing`] in, [`TokenNarrowingResponse`]
//! out.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::auth::AuthUser;
use crate::error::AppError;
use crate::AppState;

/// Most repositories one token may be confined to — a confinement, not a
/// second collaborator list.
const MAX_TOKEN_REPOSITORIES: usize = 100;

/// What narrows a token beyond its scopes. Every field is optional; a token
/// with none of them is an ordinary token.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct TokenNarrowing {
    /// Confine the token to these repositories, as `owner/name`. Omitted: every
    /// repository the account can reach.
    #[serde(default)]
    pub repositories: Option<Vec<String>>,
    /// Confine the token to these MCP tools. The token then works only through
    /// `POST /api/v1/mcp`. Omitted: an ordinary token.
    #[serde(default)]
    pub mcp_tools: Option<Vec<String>>,
    /// Refuse every write to a protected branch: merge, push, server-side
    /// commit. Defaults to `true` for a bot's token and `false` otherwise.
    #[serde(default)]
    pub deny_protected_merge: Option<bool>,
}

/// A [`TokenNarrowing`] checked against the instance: names resolved to ids,
/// tools checked against the tools this server implements.
pub(crate) struct ResolvedNarrowing {
    pub(crate) repository_ids: Option<Vec<i64>>,
    pub(crate) mcp_tools: Option<String>,
    pub(crate) deny_protected_merge: bool,
}

/// The narrowing as a token listing shows it.
#[derive(Debug, Serialize, ToSchema)]
pub struct TokenNarrowingResponse {
    /// `owner/name` of each repository the token is confined to, as the
    /// repositories are called now; `null` when it is not confined.
    pub repositories: Option<Vec<String>>,
    /// The MCP tools the token is confined to; `null` when it is not.
    pub mcp_tools: Option<Vec<String>>,
    pub deny_protected_merge: bool,
}

/// Check a requested narrowing.
///
/// A repository is named by the person minting the token, and has to be one
/// *they* can read: an unknown repository and an invisible one get the same
/// answer, so minting a token is not a way to probe for private names.
pub(crate) async fn resolve_narrowing(
    state: &AppState,
    caller_id: i64,
    narrowing: &TokenNarrowing,
    deny_protected_merge_default: bool,
) -> Result<ResolvedNarrowing, AppError> {
    let repository_ids = match &narrowing.repositories {
        None => None,
        Some(names) if names.is_empty() => {
            return Err(AppError::bad_request(
                "repositories must name at least one repository; omit it for an unconfined token",
            ));
        }
        Some(names) if names.len() > MAX_TOKEN_REPOSITORIES => {
            return Err(AppError::bad_request(format!(
                "a token may be confined to at most {MAX_TOKEN_REPOSITORIES} repositories"
            )));
        }
        Some(names) => {
            let mut ids = Vec::with_capacity(names.len());
            for full_name in names {
                let unknown = || {
                    AppError::bad_request(format!(
                        "repository '{full_name}' does not exist or is not visible to you"
                    ))
                };
                let Some((owner, name)) = full_name.trim().split_once('/') else {
                    return Err(AppError::bad_request(format!(
                        "repository '{full_name}' must be written as owner/name"
                    )));
                };
                let Some(repository) =
                    rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name)
                        .await
                        .map_err(AppError::from)?
                else {
                    return Err(unknown());
                };
                if !super::repo_access::may_read(state, &repository, Some(caller_id)).await? {
                    return Err(unknown());
                }
                ids.push(repository.id);
            }
            ids.sort_unstable();
            ids.dedup();
            Some(ids)
        }
    };

    let mcp_tools = match &narrowing.mcp_tools {
        None => None,
        Some(tools) if tools.is_empty() => {
            return Err(AppError::bad_request(
                "mcp_tools must name at least one tool; omit it for a token that is not \
                 confined to MCP",
            ));
        }
        Some(tools) => {
            let known: Vec<&str> = rg_mcp::tools::tool_names().collect();
            if let Some(unknown) = tools.iter().find(|tool| !known.contains(&tool.trim())) {
                return Err(AppError::bad_request(format!(
                    "unknown MCP tool '{unknown}'; this server implements: {}",
                    known.join(", ")
                )));
            }
            // Canonical order and no repeats, so two equal requests store
            // equal values.
            Some(
                known
                    .into_iter()
                    .filter(|name| tools.iter().any(|tool| tool.trim() == *name))
                    .collect::<Vec<_>>()
                    .join(","),
            )
        }
    };

    Ok(ResolvedNarrowing {
        repository_ids,
        mcp_tools,
        deny_protected_merge: narrowing
            .deny_protected_merge
            .unwrap_or(deny_protected_merge_default),
    })
}

/// `owner/name` of a repository as it is called now.
async fn repository_full_name(
    state: &AppState,
    repository: &rg_db::entities::repository::Model,
) -> anyhow::Result<Option<String>> {
    let owner = match repository.org_id {
        Some(org_id) => rg_db::ops::org_ops::get_org(&state.db, org_id)
            .await?
            .map(|org| org.name),
        None => rg_db::ops::user_ops::find_by_id(&state.db, repository.owner_id)
            .await?
            .map(|user| user.username),
    };
    Ok(owner.map(|owner| format!("{owner}/{}", repository.name)))
}

/// The narrowing of each of `tokens`, keyed by token id, for a listing.
pub(crate) async fn narrowing_responses(
    state: &AppState,
    tokens: &[rg_db::entities::access_token::Model],
) -> Result<std::collections::HashMap<i64, TokenNarrowingResponse>, AppError> {
    let restricted: Vec<i64> = tokens
        .iter()
        .filter(|token| token.repo_restricted)
        .map(|token| token.id)
        .collect();
    let by_token = rg_db::ops::token_ops::repository_ids_by_token(&state.db, &restricted)
        .await
        .map_err(AppError::from)?;
    let mut responses = std::collections::HashMap::new();
    for token in tokens {
        let repositories = if token.repo_restricted {
            let mut names = Vec::new();
            for id in by_token.get(&token.id).into_iter().flatten() {
                let Some(repository) = rg_db::ops::repo_ops::find_by_id(&state.db, *id)
                    .await
                    .map_err(AppError::from)?
                else {
                    continue;
                };
                if let Some(name) = repository_full_name(state, &repository)
                    .await
                    .map_err(AppError::from)?
                {
                    names.push(name);
                }
            }
            names.sort();
            Some(names)
        } else {
            None
        };
        responses.insert(
            token.id,
            TokenNarrowingResponse {
                repositories,
                mcp_tools: token
                    .mcp_tool_list()
                    .map(|tools| tools.into_iter().map(str::to_string).collect()),
                deny_protected_merge: token.deny_protected_merge,
            },
        );
    }
    Ok(responses)
}

// ── Bots ────────────────────────────────────────────────────────────────

/// A bot account as its owner sees it.
#[derive(Debug, Serialize, ToSchema)]
pub struct BotResponse {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<rg_db::entities::user::Model> for BotResponse {
    fn from(bot: rg_db::entities::user::Model) -> Self {
        Self {
            id: bot.id,
            username: bot.username,
            display_name: bot.display_name,
            is_active: bot.is_active,
            created_at: bot.created_at,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateBotRequest {
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// The bot `{bot}` when the caller owns it; anybody else's bot, or no bot at
/// all, is the same `404`.
async fn owned_bot(
    state: &AppState,
    owner_id: i64,
    bot: &str,
) -> Result<rg_db::entities::user::Model, AppError> {
    rg_core::user::bots::find_owned_bot(&state.db, owner_id, bot)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("bot not found"))
}

/// GET /api/v1/users/bots
#[utoipa::path(
    get,
    path = "/users/bots",
    tag = "Users",
    responses(
        (status = 200, description = "The caller's bots, oldest first", body = Vec<BotResponse>),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_bots(State(state): State<AppState>, AuthUser(user_id): AuthUser) -> Response {
    match rg_core::user::bots::list_bots(&state.db, user_id).await {
        Ok(bots) => (
            StatusCode::OK,
            Json(bots.into_iter().map(BotResponse::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// POST /api/v1/users/bots
#[utoipa::path(
    post,
    path = "/users/bots",
    tag = "Users",
    request_body = CreateBotRequest,
    responses(
        (status = 201, description = "Created", body = BotResponse),
        (status = 400, description = "The name is not a valid account name", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "A bot or a disabled account cannot create bots", body = serde_json::Value),
        (status = 409, description = "The name is taken", body = serde_json::Value),
    ),
)]
pub async fn create_bot(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Json(body): Json<CreateBotRequest>,
) -> Response {
    let actor = match super::access_audit::grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let bot = match rg_core::user::bots::create_bot(
        &state.db,
        user_id,
        &body.username,
        body.display_name.as_deref(),
    )
    .await
    {
        Ok(bot) => bot,
        Err(error) => return AppError::from(error).into_response(),
    };
    rg_core::audit::record(
        &state.db,
        &actor,
        "user.create_bot",
        Some("user"),
        Some(bot.id),
        Some(&bot.username),
        Some(&headers),
        None,
    )
    .await;
    (StatusCode::CREATED, Json(BotResponse::from(bot))).into_response()
}

/// DELETE /api/v1/users/bots/{bot}
///
/// Retires the bot like any account: its repositories' storage first, then the
/// row — and with it every token it holds.
#[utoipa::path(
    delete,
    path = "/users/bots/{bot}",
    tag = "Users",
    params(("bot" = String, Path, description = "The bot's username")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such bot of the caller's", body = serde_json::Value),
        (status = 409, description = "The bot still owns an organization", body = serde_json::Value),
    ),
)]
pub async fn delete_bot(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Path(bot): Path<String>,
) -> Response {
    let actor = match super::access_audit::grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let bot = match owned_bot(&state, user_id, &bot).await {
        Ok(bot) => bot,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = rg_core::user::service::delete_user(
        &state.db,
        &state.repo_root,
        state.blob_storage.as_ref(),
        &state.oci_storage,
        bot.id,
    )
    .await
    {
        return AppError::from(error).into_response();
    }
    rg_core::audit::record(
        &state.db,
        &actor,
        "user.delete_bot",
        Some("user"),
        Some(bot.id),
        Some(&bot.username),
        Some(&headers),
        None,
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateBotTokenRequest {
    pub name: String,
    /// RFC 3339 expiry; omitted: the token does not expire.
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(flatten)]
    pub narrowing: TokenNarrowing,
}

/// A token of a bot, as its owner sees it.
#[derive(Debug, Serialize, ToSchema)]
pub struct BotTokenResponse {
    pub id: i64,
    pub name: String,
    pub scopes: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(flatten)]
    pub narrowing: TokenNarrowingResponse,
}

/// GET /api/v1/users/bots/{bot}/tokens
#[utoipa::path(
    get,
    path = "/users/bots/{bot}/tokens",
    tag = "Users",
    params(("bot" = String, Path, description = "The bot's username")),
    responses(
        (status = 200, description = "The bot's tokens", body = Vec<BotTokenResponse>),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such bot of the caller's", body = serde_json::Value),
    ),
)]
pub async fn list_bot_tokens(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(bot): Path<String>,
) -> Response {
    let bot = match owned_bot(&state, user_id, &bot).await {
        Ok(bot) => bot,
        Err(error) => return error.into_response(),
    };
    let tokens = match rg_db::ops::token_ops::list_by_user(&state.db, bot.id).await {
        Ok(tokens) => tokens,
        Err(error) => return AppError::from(error).into_response(),
    };
    let mut narrowings = match narrowing_responses(&state, &tokens).await {
        Ok(narrowings) => narrowings,
        Err(error) => return error.into_response(),
    };
    let tokens: Vec<BotTokenResponse> = tokens
        .into_iter()
        .filter_map(|token| {
            let narrowing = narrowings.remove(&token.id)?;
            Some(BotTokenResponse {
                id: token.id,
                name: token.name,
                scopes: token.scopes,
                expires_at: token.expires_at,
                last_used_at: token.last_used_at,
                created_at: token.created_at,
                narrowing,
            })
        })
        .collect();
    (StatusCode::OK, Json(tokens)).into_response()
}

/// POST /api/v1/users/bots/{bot}/tokens
///
/// The raw token is in the response once and nowhere else. A bot's token
/// always carries the `repo` scope and nothing more, and is kept off protected
/// branches unless `deny_protected_merge: false` says otherwise.
#[utoipa::path(
    post,
    path = "/users/bots/{bot}/tokens",
    tag = "Users",
    params(("bot" = String, Path, description = "The bot's username")),
    request_body = CreateBotTokenRequest,
    responses(
        (status = 201, description = "Created; `token` is shown only here", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such bot of the caller's", body = serde_json::Value),
    ),
)]
pub async fn create_bot_token(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Path(bot): Path<String>,
    Json(body): Json<CreateBotTokenRequest>,
) -> Response {
    let audit_actor = match super::access_audit::grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let bot = match owned_bot(&state, user_id, &bot).await {
        Ok(bot) => bot,
        Err(error) => return error.into_response(),
    };
    if body.name.trim().is_empty() {
        return AppError::bad_request("token name cannot be empty").into_response();
    }
    let expires_at = match super::users::parse_token_expiration(body.expires_at.as_deref()) {
        Ok(expires_at) => expires_at,
        Err(error) => return error.into_response(),
    };
    let narrowing = match resolve_narrowing(&state, user_id, &body.narrowing, true).await {
        Ok(narrowing) => narrowing,
        Err(error) => return error.into_response(),
    };

    // Generated only after every field is known to be valid: a rejected
    // request never mints even a transient raw token.
    let raw_token = super::users::generate_token();
    let model = rg_db::entities::access_token::ActiveModel {
        id: sea_orm::NotSet,
        user_id: sea_orm::Set(bot.id),
        name: sea_orm::Set(body.name),
        token_hash: sea_orm::Set(super::users::hash_token(&raw_token)),
        scopes: sea_orm::Set(BOT_TOKEN_SCOPES.to_string()),
        expires_at: sea_orm::Set(expires_at),
        last_used_at: sea_orm::Set(None),
        created_at: sea_orm::Set(chrono::Utc::now()),
        repo_restricted: sea_orm::Set(narrowing.repository_ids.is_some()),
        mcp_tools: sea_orm::Set(narrowing.mcp_tools),
        deny_protected_merge: sea_orm::Set(narrowing.deny_protected_merge),
    };
    let token = match rg_db::ops::token_ops::create(
        &state.db,
        model,
        narrowing.repository_ids.as_deref().unwrap_or_default(),
    )
    .await
    {
        Ok(token) => token,
        Err(error) => return AppError::from(error).into_response(),
    };
    // The token's name, scope and narrowing; never the token or its hash.
    // Journalled under the bot — whose credential it is — by its owner.
    rg_core::audit::record(
        &state.db,
        &audit_actor,
        "user.create_bot_token",
        Some("user"),
        Some(bot.id),
        Some(&bot.username),
        Some(&headers),
        Some(serde_json::json!({
            "token_id": token.id,
            "token_name": token.name,
            "scopes": token.scopes,
            "expires_at": token.expires_at,
            "repositories": body.narrowing.repositories,
            "mcp_tools": token.mcp_tools,
            "deny_protected_merge": token.deny_protected_merge,
        })),
    )
    .await;
    let mut narrowings = match narrowing_responses(&state, std::slice::from_ref(&token)).await {
        Ok(narrowings) => narrowings,
        Err(error) => return error.into_response(),
    };
    let narrowing = narrowings.remove(&token.id);
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": token.id,
            "name": token.name,
            "token": raw_token,
            "scopes": token.scopes,
            "expires_at": token.expires_at,
            "created_at": token.created_at,
            "repositories": narrowing.as_ref().and_then(|n| n.repositories.clone()),
            "mcp_tools": narrowing.as_ref().and_then(|n| n.mcp_tools.clone()),
            "deny_protected_merge": token.deny_protected_merge,
        })),
    )
        .into_response()
}

/// The only scope a bot's token carries: the repository API, which is where
/// the bot's work is. Account management — tokens, keys, bots — is its
/// owner's, and stays out of its reach.
pub(crate) const BOT_TOKEN_SCOPES: &str = "repo";

/// DELETE /api/v1/users/bots/{bot}/tokens/{id}
#[utoipa::path(
    delete,
    path = "/users/bots/{bot}/tokens/{id}",
    tag = "Users",
    params(
        ("bot" = String, Path, description = "The bot's username"),
        ("id" = i64, Path, description = "The token's id"),
    ),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such bot or token of the caller's", body = serde_json::Value),
    ),
)]
pub async fn delete_bot_token(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Path((bot, id)): Path<(String, i64)>,
) -> Response {
    let audit_actor = match super::access_audit::grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let bot = match owned_bot(&state, user_id, &bot).await {
        Ok(bot) => bot,
        Err(error) => return error.into_response(),
    };
    let token = match rg_db::ops::token_ops::find_by_id(&state.db, id).await {
        Ok(Some(token)) if token.user_id == bot.id => token,
        Ok(_) => return AppError::not_found("token not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_db::ops::token_ops::delete_by_id(&state.db, token.id).await {
        Ok(true) => {}
        Ok(false) => return AppError::not_found("token not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    }
    rg_core::audit::record(
        &state.db,
        &audit_actor,
        "user.delete_bot_token",
        Some("user"),
        Some(bot.id),
        Some(&bot.username),
        Some(&headers),
        Some(serde_json::json!({
            "token_id": token.id,
            "token_name": token.name,
        })),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}
