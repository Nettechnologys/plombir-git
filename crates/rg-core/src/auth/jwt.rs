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
    /// Issued-at (Unix timestamp seconds).
    pub iat: i64,
    /// Expiry (Unix timestamp seconds).
    pub exp: i64,
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
    format!("ironforge:mfa-challenge:{secret}")
}

/// Generate a signed JWT for a user.
pub fn generate_token(user_id: i64, username: &str, secret: &str, ttl_days: i64) -> Result<String> {
    let now = Utc::now();
    let exp = now + Duration::days(ttl_days);
    let claims = Claims {
        sub: user_id.to_string(),
        username: username.to_string(),
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
        let token = generate_token(42, "alice", secret, 1).unwrap();
        let claims = validate_token(&token, secret).unwrap();
        assert_eq!(claims.sub, "42");
        assert_eq!(claims.username, "alice");
    }

    #[test]
    fn test_invalid_token() {
        assert!(validate_token("not.a.token", "secret").is_none());
    }

    #[test]
    fn test_wrong_secret_fails() {
        let token = generate_token(1, "bob", "secret_a", 7).unwrap();
        assert!(validate_token(&token, "secret_b").is_none());
    }

    #[test]
    fn test_expired_token_fails() {
        let token = generate_token(1, "charlie", "secret", -1).unwrap(); // already expired
        assert!(validate_token(&token, "secret").is_none());
    }

    #[test]
    fn test_token_claims_fields() {
        let token = generate_token(99, "testuser", "mykey", 30).unwrap();
        let claims = validate_token(&token, "mykey").unwrap();
        assert_eq!(claims.sub, "99");
        assert_eq!(claims.username, "testuser");
        assert!(claims.iat > 0);
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn test_different_user_ids() {
        let secret = "key";
        let t1 = generate_token(0, "user0", secret, 7).unwrap();
        let t2 = generate_token(i64::MAX, "usermax", secret, 7).unwrap();

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

    #[test]
    fn mfa_challenge_is_short_lived_and_cannot_be_used_as_a_session() {
        let challenge = generate_mfa_challenge(42, "alice", "ldap", "secret").unwrap();
        assert!(validate_token(&challenge, "secret").is_none());
        let claims = validate_mfa_challenge(&challenge, "secret").unwrap();
        assert_eq!(claims.sub, "42");
        assert_eq!(claims.username, "alice");
        assert_eq!(claims.auth_provider, "ldap");
        assert!(claims.exp - claims.iat <= 300);

        let session = generate_token(42, "alice", "secret", 7).unwrap();
        assert!(validate_mfa_challenge(&session, "secret").is_none());
    }
}
