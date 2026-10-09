//! JWT token generation and validation.
//!
//! Tokens are HS256-signed JWTs with a configurable expiry (default 7 days).

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// JWT claims payload.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    /// Subject — user id as string.
    pub sub: String,
    /// Username (convenience field, not authoritative).
    pub username: String,
    /// Monotonic user-side generation. A mismatch revokes this bearer session.
    ///
    /// Old tokens decode as generation zero during a rolling upgrade; they are
    /// then invalidated by the next password reset or logout.
    #[serde(default)]
    pub session_version: i64,
    /// `access_tokens.id` when this is the synthetic token minted for a
    /// presented personal access token, `None` for a real session.
    ///
    /// A PAT reaches the API by being translated into one of these
    /// (`rg_http::pat_auth::pat_to_bearer_jwt`), which makes every handler
    /// downstream unable to tell the two apart. That is what the translation is
    /// for — but a handler minting a capability that *outlives the request*
    /// needs the difference, because the two credentials are revoked by
    /// different acts: a session by its generation moving on, a PAT by its row
    /// going away. Without this claim a presigned LFS URL obtained with a PAT
    /// was bound to a session generation the token has nothing to do with
    /// (card_e4e177acd095).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pat_id: Option<i64>,
    /// Until when (Unix timestamp seconds) this session counts as *recently
    /// re-authenticated* — "sudo mode". `None` for a session that has only
    /// ever logged in.
    ///
    /// A seven-day bearer session is the right length for reading and writing
    /// code and the wrong length for minting a credential that outlives it: a
    /// stolen session that can add an SSH key or a personal access token is
    /// permanent access, not seven days of it. The routes that mint such
    /// credentials therefore demand a session whose holder has re-proved the
    /// password (and the second factor) within [`SUDO_TTL`], and that proof is
    /// carried here rather than in a second cookie so the one session gate
    /// (`session_standing_middleware`, the revocation check) keeps seeing one
    /// token. Set only by [`reissue_with_sudo`]; a token minted before this
    /// claim existed decodes as `None` and simply has to step up once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sudo_exp: Option<i64>,
    /// Issued-at (Unix timestamp seconds).
    pub iat: i64,
    /// Expiry (Unix timestamp seconds).
    pub exp: i64,
}

/// How long a successful `POST /users/me/sudo` keeps a session in sudo mode.
///
/// Ten minutes is long enough to add a key, mint a token and register a
/// passkey in one sitting, and short enough that a session stolen *after* the
/// step-up is back to an ordinary session before an attacker is likely to
/// notice it. The window is counted from the step-up, not extended by use.
pub const SUDO_TTL: Duration = Duration::minutes(10);

impl Claims {
    /// Whether this session is in sudo mode at `now` (Unix seconds).
    pub fn sudo_active_at(&self, now: i64) -> bool {
        self.sudo_exp.is_some_and(|until| until > now)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MfaChallengeClaims {
    pub sub: String,
    pub username: String,
    pub auth_provider: String,
    pub iat: i64,
    pub exp: i64,
}

fn mfa_challenge_key(secret: &str) -> String {
    format!("plombir-git:mfa-challenge:{secret}")
}

/// Generate a signed JWT for a user and its current session generation.
pub fn generate_token(
    user_id: i64,
    username: &str,
    session_version: i64,
    secret: &str,
    ttl_days: i64,
) -> Result<String> {
    encode_claims(user_id, username, session_version, None, secret, ttl_days)
}

/// Generate the synthetic session a presented personal access token is
/// translated into, tagged with the token it came from.
///
/// Separate from [`generate_token`] so the tag cannot be forgotten at the one
/// call site that must set it, and so no real login can accidentally set it.
pub fn generate_token_for_pat(
    user_id: i64,
    username: &str,
    session_version: i64,
    pat_id: i64,
    secret: &str,
    ttl_days: i64,
) -> Result<String> {
    encode_claims(
        user_id,
        username,
        session_version,
        Some(pat_id),
        secret,
        ttl_days,
    )
}

fn encode_claims(
    user_id: i64,
    username: &str,
    session_version: i64,
    pat_id: Option<i64>,
    secret: &str,
    ttl_days: i64,
) -> Result<String> {
    let now = Utc::now();
    let exp = now + Duration::days(ttl_days);
    let claims = Claims {
        sub: user_id.to_string(),
        username: username.to_string(),
        session_version,
        pat_id,
        sudo_exp: None,
        iat: now.timestamp(),
        exp: exp.timestamp(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .context("jwt encode failed")
}

/// Re-issue a session token in sudo mode: the same subject, generation and
/// expiry, with `sudo_exp` set to now plus [`SUDO_TTL`].
///
/// The expiry is kept rather than renewed on purpose — proving the password
/// again is not a new login, and a session that was going to end tomorrow still
/// ends tomorrow. `sudo_exp` is clamped to `exp` so the claim never promises
/// more than the token can deliver. The synthetic session a PAT is translated
/// into is refused: a PAT holder cannot step up, because the whole point of the
/// step is to tell a session from a delegated token.
pub fn reissue_with_sudo(claims: &Claims, secret: &str) -> Result<String> {
    if claims.pat_id.is_some() {
        anyhow::bail!("a personal access token cannot enter sudo mode");
    }
    let now = Utc::now();
    let sudo_exp = (now + SUDO_TTL).timestamp().min(claims.exp);
    let reissued = Claims {
        sub: claims.sub.clone(),
        username: claims.username.clone(),
        session_version: claims.session_version,
        pat_id: None,
        sudo_exp: Some(sudo_exp),
        iat: now.timestamp(),
        exp: claims.exp,
    };
    encode(
        &Header::default(),
        &reissued,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .context("sudo jwt encode failed")
}

/// Validate and decode a JWT. Returns `None` if invalid/expired.
pub fn validate_token(token: &str, secret: &str) -> Option<Claims> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .ok()
    .map(|d| d.claims)
}

/// Generate a five-minute token proving that the primary login factor passed.
/// A domain-separated signing key prevents this token from being accepted as a
/// normal user session JWT.
pub fn generate_mfa_challenge(
    user_id: i64,
    username: &str,
    auth_provider: &str,
    secret: &str,
) -> Result<String> {
    let now = Utc::now();
    let claims = MfaChallengeClaims {
        sub: user_id.to_string(),
        username: username.to_string(),
        auth_provider: auth_provider.to_string(),
        iat: now.timestamp(),
        exp: (now + Duration::minutes(5)).timestamp(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(mfa_challenge_key(secret).as_bytes()),
    )
    .context("MFA challenge encode failed")
}

pub fn validate_mfa_challenge(token: &str, secret: &str) -> Option<MfaChallengeClaims> {
    decode::<MfaChallengeClaims>(
        token,
        &DecodingKey::from_secret(mfa_challenge_key(secret).as_bytes()),
        &Validation::default(),
    )
    .ok()
    .map(|decoded| decoded.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_validate() {
        let secret = "test_secret_key";
        let token = generate_token(42, "alice", 3, secret, 1).unwrap();
        let claims = validate_token(&token, secret).unwrap();
        assert_eq!(claims.sub, "42");
        assert_eq!(claims.username, "alice");
        assert_eq!(claims.session_version, 3);
    }

    #[test]
    fn test_invalid_token() {
        assert!(validate_token("not.a.token", "secret").is_none());
    }

    #[test]
    fn test_wrong_secret_fails() {
        let token = generate_token(1, "bob", 0, "secret_a", 7).unwrap();
        assert!(validate_token(&token, "secret_b").is_none());
    }

    #[test]
    fn test_expired_token_fails() {
        let token = generate_token(1, "charlie", 0, "secret", -1).unwrap(); // already expired
        assert!(validate_token(&token, "secret").is_none());
    }

    #[test]
    fn test_token_claims_fields() {
        let token = generate_token(99, "testuser", 7, "mykey", 30).unwrap();
        let claims = validate_token(&token, "mykey").unwrap();
        assert_eq!(claims.sub, "99");
        assert_eq!(claims.username, "testuser");
        assert!(claims.iat > 0);
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn test_different_user_ids() {
        let secret = "key";
        let t1 = generate_token(0, "user0", 0, secret, 7).unwrap();
        let t2 = generate_token(i64::MAX, "usermax", 0, secret, 7).unwrap();

        let c1 = validate_token(&t1, secret).unwrap();
        assert_eq!(c1.sub, "0");

        let c2 = validate_token(&t2, secret).unwrap();
        assert_eq!(c2.sub, i64::MAX.to_string());
    }

    #[test]
    fn test_empty_token_fails() {
        assert!(validate_token("", "secret").is_none());
    }

    #[test]
    fn test_malformed_token_fails() {
        assert!(validate_token("aaa.bbb", "secret").is_none());
        assert!(validate_token("aaa.bbb.ccc.ddd", "secret").is_none());
    }

    /// A login session carries no sudo claim, the re-issued one carries a
    /// bounded one, and nothing else about the session moves.
    #[test]
    fn reissue_with_sudo_keeps_the_session_and_adds_a_bounded_window() {
        let secret = "sudo-secret";
        let token = generate_token(42, "alice", 3, secret, 7).unwrap();
        let claims = validate_token(&token, secret).unwrap();
        assert_eq!(claims.sudo_exp, None);
        assert!(!claims.sudo_active_at(Utc::now().timestamp()));
        assert!(
            !token.contains("sudo_exp"),
            "an absent claim is not serialized"
        );

        let elevated = reissue_with_sudo(&claims, secret).unwrap();
        let sudo = validate_token(&elevated, secret).unwrap();
        assert_eq!(sudo.sub, "42");
        assert_eq!(sudo.username, "alice");
        assert_eq!(sudo.session_version, 3);
        assert_eq!(sudo.pat_id, None);
        assert_eq!(sudo.exp, claims.exp, "stepping up is not a new login");
        let now = Utc::now().timestamp();
        let until = sudo.sudo_exp.expect("the re-issued token is in sudo mode");
        assert!(sudo.sudo_active_at(now));
        assert!(until > now && until <= now + SUDO_TTL.num_seconds());
        assert!(!sudo.sudo_active_at(until), "the window is half-open");
    }

    /// The window never outlives the session it is attached to.
    #[test]
    fn sudo_window_is_clamped_to_the_session_expiry() {
        let secret = "sudo-secret";
        let claims = Claims {
            sub: "1".into(),
            username: "short".into(),
            session_version: 0,
            pat_id: None,
            sudo_exp: None,
            iat: Utc::now().timestamp(),
            exp: Utc::now().timestamp() + 60,
        };
        let elevated = reissue_with_sudo(&claims, secret).unwrap();
        let sudo = validate_token(&elevated, secret).unwrap();
        assert_eq!(sudo.sudo_exp, Some(claims.exp));
    }

    /// A PAT's synthetic session has no password to re-prove.
    #[test]
    fn a_pat_session_cannot_be_reissued_in_sudo_mode() {
        let secret = "sudo-secret";
        let token = generate_token_for_pat(7, "bot", 0, 99, secret, 1).unwrap();
        let claims = validate_token(&token, secret).unwrap();
        assert!(reissue_with_sudo(&claims, secret).is_err());
    }

    /// Tokens minted before the claim existed keep validating, as `None`.
    #[test]
    fn a_token_without_the_sudo_claim_still_validates() {
        #[derive(Serialize)]
        struct Legacy<'a> {
            sub: &'a str,
            username: &'a str,
            session_version: i64,
            iat: i64,
            exp: i64,
        }
        let now = Utc::now().timestamp();
        let legacy = encode(
            &Header::default(),
            &Legacy {
                sub: "5",
                username: "old",
                session_version: 2,
                iat: now,
                exp: now + 3600,
            },
            &EncodingKey::from_secret(b"legacy"),
        )
        .unwrap();
        let claims = validate_token(&legacy, "legacy").expect("a pre-sudo token still decodes");
        assert_eq!(claims.sudo_exp, None);
        assert!(!claims.sudo_active_at(now));

        // And a token whose window has passed is an ordinary session again.
        let expired = Claims {
            sudo_exp: Some(now - 1),
            ..claims
        };
        let token = encode(
            &Header::default(),
            &expired,
            &EncodingKey::from_secret(b"legacy"),
        )
        .unwrap();
        let decoded = validate_token(&token, "legacy").unwrap();
        assert_eq!(decoded.sudo_exp, Some(now - 1));
        assert!(!decoded.sudo_active_at(now));
    }

    #[test]
    fn mfa_challenge_is_short_lived_and_cannot_be_used_as_a_session() {
        let challenge = generate_mfa_challenge(42, "alice", "ldap", "secret").unwrap();
        assert!(validate_token(&challenge, "secret").is_none());
        let claims = validate_mfa_challenge(&challenge, "secret").unwrap();
        assert_eq!(claims.sub, "42");
        assert_eq!(claims.username, "alice");
        assert_eq!(claims.auth_provider, "ldap");
        assert!(claims.exp - claims.iat <= 300);

        let session = generate_token(42, "alice", 0, "secret", 7).unwrap();
        assert!(validate_mfa_challenge(&session, "secret").is_none());
    }
}
