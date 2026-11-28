//! Issuing personal access tokens and bot tokens.
//!
//! Both kinds are one row in `access_tokens` and one raw credential handed to
//! the caller once. The HTTP handlers used to build that row each on their
//! own — the raw token, its digest, the narrowing columns — so a column added
//! to one path could be forgotten on the other. They now decide who may ask
//! for what and answer; the credential itself is minted here.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use sea_orm::{ActiveValue::Set, DatabaseConnection, NotSet};

use rg_db::entities::access_token;

/// Prefix of every access token, so the type stays recognisable (GitHub
/// `ghp_` style) and secret scanners can find a leaked one. Kept from the
/// project's former name: tokens already issued carry it.
pub const ACCESS_TOKEN_PREFIX: &str = "ifp_";

/// What an access token is issued for. Scopes and narrowing have already been
/// validated by the caller; nothing here second-guesses them.
#[derive(Debug, Clone)]
pub struct NewAccessToken {
    /// The account the token acts as — the person, or the bot.
    pub user_id: i64,
    pub name: String,
    /// Normalised scope string (`pat_scope::normalize_scopes`).
    pub scopes: String,
    pub expires_at: Option<DateTime<Utc>>,
    /// `Some` confines the token to these repositories, `None` leaves it the
    /// account's whole reach.
    pub repository_ids: Option<Vec<i64>>,
    /// Comma-separated MCP tools the token is confined to, if any.
    pub mcp_tools: Option<String>,
    pub deny_protected_merge: bool,
    /// The refs the token may write, when it is confined to some: a value
    /// ending in `/` admits the refs under it, any other one exactly that ref
    /// (ADR-0001 D3). Only a token issued for an agent run carries one.
    pub write_ref_prefix: Option<String>,
    /// Whether a token confined to MCP tools may also use the git transport.
    pub allow_git: bool,
    /// The agent run attempt the token is issued for; deleting the run
    /// deletes the token.
    pub agent_run_attempt_id: Option<i64>,
}

/// A freshly issued token: the raw credential, shown to its owner once and
/// never stored, and the row that authenticates it.
pub struct IssuedAccessToken {
    pub raw: String,
    pub token: access_token::Model,
}

/// A new raw access token: 32 bytes (256 bits) from the OS CSPRNG,
/// hex-encoded after [`ACCESS_TOKEN_PREFIX`].
///
/// Every byte of entropy comes from the CSPRNG — there is deliberately no
/// timestamp- or PID-derived fallback that could make a token predictable. If
/// the OS entropy source is unavailable `fill_bytes` panics rather than
/// emitting a guessable token.
pub fn generate_access_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{ACCESS_TOKEN_PREFIX}{}", hex::encode(bytes))
}

/// Mint a token and store the row that authenticates it, with its repository
/// confinement, in one transaction.
///
/// The row keeps only `rg_db::token_digest(raw)` — the same digest every
/// verifier looks a presented token up by.
pub async fn issue(db: &DatabaseConnection, new: NewAccessToken) -> Result<IssuedAccessToken> {
    let raw = generate_access_token();
    let model = access_token::ActiveModel {
        id: NotSet,
        user_id: Set(new.user_id),
        name: Set(new.name),
        token_hash: Set(rg_db::token_digest(&raw)),
        scopes: Set(new.scopes),
        expires_at: Set(new.expires_at),
        last_used_at: Set(None),
        created_at: Set(Utc::now()),
        repo_restricted: Set(new.repository_ids.is_some()),
        mcp_tools: Set(new.mcp_tools),
        deny_protected_merge: Set(new.deny_protected_merge),
        write_ref_prefix: Set(new.write_ref_prefix),
        allow_git: Set(new.allow_git),
        agent_run_attempt_id: Set(new.agent_run_attempt_id),
    };
    let token =
        rg_db::ops::token_ops::create(db, model, new.repository_ids.as_deref().unwrap_or_default())
            .await?;
    Ok(IssuedAccessToken { raw, token })
}

/// How long a run token outlives the run's own timeout.
///
/// The token is deleted when the attempt ends ([`revoke_for_agent_run_attempt`]);
/// `expires_at` is the backstop for an end that never got that far — a server
/// restarted mid-run, a runner that vanished. The margin covers the clone, the
/// image pull and the final push after the agent's last turn.
pub const AGENT_RUN_TOKEN_GRACE_SECS: i64 = 10 * 60;

/// What a token for one attempt of an agent run is issued for.
#[derive(Debug, Clone, Copy)]
pub struct AgentRunGrant<'a> {
    /// The bot the run works as.
    pub bot: &'a rg_db::entities::user::Model,
    /// The one repository the run works in.
    pub repository_id: i64,
    /// The attempt the token belongs to; deleting it deletes the token.
    pub attempt_id: i64,
    /// What the token may write: `refs/heads/agent/<bot>/` for a first run, the
    /// pull request's branch for an iteration ([`crate::agent::run::branch_prefix`]).
    pub write_ref_prefix: &'a str,
    /// The run's timeout, from the bot's profile.
    pub timeout_secs: i64,
}

/// Mint the token one attempt of an agent run works with (ADR-0001 D3).
///
/// Nobody asks for it over HTTP: there is no `SudoUser` here because the only
/// caller is the core that starts an attempt. What it can do is fixed here and
/// nowhere else:
///
/// - one repository, `repo` scope, kept off protected branches;
/// - only the MCP tools of [`crate::agent::run::RUN_MCP_TOOLS`], plus the git
///   transport (`allow_git`) — no other REST route;
/// - writes only under `write_ref_prefix`, which must be the bot's own
///   `refs/heads/agent/<bot>/` or a branch under it, so no caller can hand a
///   run `main`;
/// - tied to the attempt, so the CI gate knows its writes for an agent's
///   (`push_hooks::PushOrigin`), and gone with it;
/// - expiring [`AGENT_RUN_TOKEN_GRACE_SECS`] after the run's timeout.
///
/// The attempt must be one of a run of this bot in this repository: a token
/// tied to somebody else's run would have its writes counted as that run's.
pub async fn issue_for_agent_run(
    db: &DatabaseConnection,
    grant: AgentRunGrant<'_>,
) -> Result<IssuedAccessToken> {
    if !grant.bot.is_bot() {
        return Err(crate::error::forbidden(
            "an agent run token is issued only to a bot account",
        ));
    }
    if !crate::agent::run::prefix_is_the_bots(&grant.bot.username, grant.write_ref_prefix) {
        return Err(crate::error::invalid_request(format!(
            "an agent run of '{}' may write only under '{}'",
            grant.bot.username,
            crate::agent::run::branch_prefix(&grant.bot.username)
        )));
    }
    if grant.timeout_secs <= 0 {
        return Err(crate::error::invalid_request(
            "an agent run token needs a positive run timeout",
        ));
    }
    let run = rg_db::ops::agent_run_ops::run_of_attempt(db, grant.attempt_id)
        .await?
        .ok_or_else(|| crate::error::not_found("agent run attempt"))?;
    if run.bot_id != grant.bot.id || run.repository_id != grant.repository_id {
        return Err(crate::error::forbidden(
            "the attempt belongs to another bot's run or another repository",
        ));
    }
    issue(
        db,
        NewAccessToken {
            user_id: grant.bot.id,
            name: format!("agent run {} attempt {}", run.id, grant.attempt_id),
            scopes: "repo".to_string(),
            expires_at: Some(
                Utc::now()
                    + chrono::Duration::seconds(grant.timeout_secs + AGENT_RUN_TOKEN_GRACE_SECS),
            ),
            repository_ids: Some(vec![grant.repository_id]),
            mcp_tools: Some(crate::agent::run::RUN_MCP_TOOLS.join(",")),
            deny_protected_merge: true,
            write_ref_prefix: Some(grant.write_ref_prefix.to_string()),
            allow_git: true,
            agent_run_attempt_id: Some(grant.attempt_id),
        },
    )
    .await
}

/// Delete the token of agent run attempt `attempt_id`; `false` when it had
/// none left.
///
/// Called when the attempt ends, however it ends. Deleting the attempt (or its
/// run) takes the token too — the foreign key cascades — so this is for the
/// attempt that ends and stays on record.
pub async fn revoke_for_agent_run_attempt(
    db: &DatabaseConnection,
    attempt_id: i64,
) -> Result<bool> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let deleted = access_token::Entity::delete_many()
        .filter(access_token::Column::AgentRunAttemptId.eq(attempt_id))
        .exec(db)
        .await
        .map_err(anyhow::Error::from)
        .map_err(|error| error.context("db: delete the token of an agent run attempt"))?;
    Ok(deleted.rows_affected > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn token_has_expected_prefix_and_length() {
        let token = generate_access_token();
        // `ifp_` (4) + 32 bytes as hex (64) = 68 chars.
        assert!(
            token.starts_with(ACCESS_TOKEN_PREFIX),
            "an access token must carry the {ACCESS_TOKEN_PREFIX} prefix: {token}"
        );
        assert_eq!(token.len(), 68, "unexpected token length: {token}");
    }

    #[test]
    fn token_body_is_256_bits_of_hex_entropy() {
        let token = generate_access_token();
        let body = token.strip_prefix(ACCESS_TOKEN_PREFIX).expect("prefix");
        let bytes = hex::decode(body).expect("the token body must be valid hex");
        assert_eq!(
            bytes.len(),
            32,
            "a token must carry 32 bytes (256 bits) of entropy"
        );
    }

    #[test]
    fn token_is_unique_across_many_calls() {
        // A CSPRNG token — not a timestamp/PID — must never collide, even when
        // minted in a tight loop within the same process/instant.
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            assert!(
                seen.insert(generate_access_token()),
                "token collision — entropy too weak"
            );
        }
    }

    #[test]
    fn distinct_tokens_hash_distinctly() {
        assert_ne!(
            rg_db::token_digest(&generate_access_token()),
            rg_db::token_digest(&generate_access_token())
        );
    }
}
