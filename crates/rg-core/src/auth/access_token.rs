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
    };
    let token =
        rg_db::ops::token_ops::create(db, model, new.repository_ids.as_deref().unwrap_or_default())
            .await?;
    Ok(IssuedAccessToken { raw, token })
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
