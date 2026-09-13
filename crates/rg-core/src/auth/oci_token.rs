//! OCI Distribution Spec — Bearer Token authentication.
//!
//! Implements [OCI Distribution Spec v1.0 — Token Authentication]
//! (<https://docs.docker.com/registry/spec/auth/token/>).
//!
//! ## Token Format
//!
//! ForgeKeep issues **signed JWTs** (HS256) with the following claims:
//!
//! ```json
//! {
//!   "iss": "forgekeep",
//!   "sub": "<username>",
//!   "aud": "forgekeep-registry",
//!   "exp": 1700000000,
//!   "iat": 1699999900,
//!   "scope": "repository:owner/repo:pull,push"
//! }
//! ```
//!
//! ## Supported Scopes
//!
//! | Scope String | Meaning |
//! |-------------|---------|
//! | `registry:catalog:*` | Access to catalog listing |
//! | `repository:<owner>/<repo>:pull` | Read access to repo |
//! | `repository:<owner>/<repo>:push` | Write access to repo |
//!
//! ## Token Endpoint
//!
//! `GET /v2/auth/token?service=...&scope=...`
//!
//! Returns `{ "token": "<jwt>" }`.

use anyhow::Context;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

/// OCI Bearer token JWT claims.
#[derive(Debug, Serialize, Deserialize)]
pub struct OciTokenClaims {
    /// Issuer — always `"forgekeep"`.
    pub iss: String,

    /// Subject — username (or `anonymous`).
    pub sub: String,

    /// Audience — must match `service` query parameter (`"forgekeep-registry"`).
    pub aud: String,

    /// Expiration (UNIX seconds).
    pub exp: usize,

    /// Issued-at (UNIX seconds).
    pub iat: usize,

    /// Optional not-before (UNIX seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nbf: Option<usize>,

    /// Optional JWT ID (for revocation if needed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,

    /// Space-separated scope strings.
    /// Example: `"repository:alice/myapp:pull,push registry:catalog:*"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Parse a scope string into structured permissions.
///
/// Scope format: `<type>:<name>:<actions>`
/// - `repository:owner/repo:pull,push`
/// - `registry:catalog:*`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedScope {
    pub scope_type: String,       // "repository" | "registry"
    pub name: String,             // "owner/repo" | "catalog"
    pub actions: HashSet<String>, // {"pull", "push"} | {"*"}
}

impl ParsedScope {
    /// Parse a single scope string.
    /// Returns `None` if the format is invalid.
    pub fn parse(scope_str: &str) -> Option<Self> {
        let parts: Vec<&str> = scope_str.splitn(3, ':').collect();
        if parts.len() != 3 {
            return None;
        }
        let actions: HashSet<String> = parts[2]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if actions.is_empty() {
            return None;
        }
        Some(Self {
            scope_type: parts[0].to_string(),
            name: parts[1].to_string(),
            actions,
        })
    }

    /// Check if this scope grants a specific action.
    pub fn has_action(&self, action: &str) -> bool {
        self.actions.contains("*") || self.actions.contains(action)
    }

    /// Check if this is a repository scope for a specific repo.
    pub fn matches_repo(&self, owner: &str, repo: &str) -> bool {
        self.scope_type == "repository" && self.name == format!("{}/{}", owner, repo)
    }
}

/// Generate an OCI Bearer token (JWT HS256).
///
/// - `username`: The authenticated username (or `"anonymous"` for public access).
/// - `scope`: Space-separated scope strings (e.g., `"repository:alice/hello:pull,push"`).
/// - `secret`: The JWT signing secret (same as `jwt_secret`).
/// - `ttl_secs`: Token time-to-live in seconds (recommended: 300 = 5 min).
///
/// Returns the serialized JWT string.
pub fn generate_oci_token(
    username: &str,
    scope: &str,
    secret: &str,
    ttl_secs: u64,
) -> anyhow::Result<String> {
    generate_oci_token_at(username, scope, secret, ttl_secs, SystemTime::now())
}

/// [`generate_oci_token`] with the clock passed in.
///
/// Split out for the same reason [`crate::auth::totp::verify_code_step`] hands
/// its own `now` to a helper: the interesting branch here is a host whose clock
/// reads before the Unix epoch, and no test can produce that by setting the
/// machine's time. Private, because the only caller that mints tokens is
/// `generate_oci_token` — the clock is not a knob the registry offers.
fn generate_oci_token_at(
    username: &str,
    scope: &str,
    secret: &str,
    ttl_secs: u64,
    clock: SystemTime,
) -> anyhow::Result<String> {
    // Not `unwrap`: the clock is operator/runtime input, not an invariant this
    // process has proved. A host with an unset RTC or a skewed namespace clock
    // must get a diagnosable 500 from the token endpoint, not a dead process —
    // and the message has to name the remedy, because the failure is on the
    // machine and not in the request.
    let now = clock
        .duration_since(UNIX_EPOCH)
        .context(
            "the system clock reads a time before the Unix epoch, so an OCI token cannot be \
             stamped; correct the host clock (NTP, or the container's RTC) and retry",
        )?
        .as_secs() as usize;

    let claims = OciTokenClaims {
        iss: "forgekeep".to_string(),
        sub: username.to_string(),
        aud: "forgekeep-registry".to_string(),
        iat: now,
        exp: now + ttl_secs as usize,
        nbf: None,
        jti: None,
        scope: if scope.is_empty() {
            None
        } else {
            Some(scope.to_string())
        },
    };

    let key = EncodingKey::from_secret(secret.as_bytes());
    let token = encode(&Header::default(), &claims, &key)
        .map_err(|e| anyhow::anyhow!("OCI token generation failed: {}", e))?;
    Ok(token)
}

/// Validate an OCI Bearer token (JWT HS256).
///
/// Returns the claims if valid, `None` otherwise.
pub fn validate_oci_token(token: &str, secret: &str) -> Option<OciTokenClaims> {
    let key = DecodingKey::from_secret(secret.as_bytes());
    let mut validation = Validation::default();
    validation.iss = Some(std::collections::HashSet::from(["forgekeep".to_string()]));
    validation.aud = Some(std::collections::HashSet::from([
        "forgekeep-registry".to_string()
    ]));

    match decode::<OciTokenClaims>(token, &key, &validation) {
        Ok(data) => Some(data.claims),
        Err(e) => {
            tracing::debug!("OCI token validation failed: {}", e);
            None
        }
    }
}

/// Build the `WWW-Authenticate` header value for OCI Distribution auth.
///
/// Example:
/// ```text
/// Bearer realm="https://registry.example.com/v2/auth/token",service="registry",scope="repository:alice/hello:pull,push"
/// ```
pub fn build_www_authenticate(realm: &str, service: &str, scope: &str) -> String {
    // The scope goes in verbatim. It used to be percent-encoded, which turns
    // `repository:alice/hello:pull` into `repository%3Aalice%2Fhello%3Apull` —
    // and the client does not decode it. It copies the string out of the
    // challenge and asks the token endpoint for *that*, where `ParsedScope`
    // finds no `:` separators, grants nothing, and hands back a token that is
    // refused by the very request that produced the challenge.
    //
    // These are quoted `auth-param` values (RFC 7235 §2.1), so the delimiters
    // that would need escaping are the quote and the backslash — neither of
    // which appears in a scope — not `:` and `/`. Every registry in the wild
    // emits the unencoded form; the spec's own example is
    // `scope="repository:samalba/my-app:pull,push"`.
    format!(
        r#"Bearer realm="{}",service="{}",scope="{}""#,
        realm, service, scope
    )
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const TEST_SECRET: &str = "test-oci-secret-key-1234567890";

    #[test]
    fn test_generate_and_validate_token() {
        let token = generate_oci_token(
            "alice",
            "repository:alice/hello:pull,push",
            TEST_SECRET,
            300,
        )
        .unwrap();
        assert!(!token.is_empty());

        let claims = validate_oci_token(&token, TEST_SECRET).unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.aud, "forgekeep-registry");
        assert!(claims.scope.as_ref().unwrap().contains("pull,push"));
    }

    #[test]
    fn a_clock_before_the_unix_epoch_is_reported_not_panicked_on() {
        let before_epoch = SystemTime::UNIX_EPOCH - Duration::from_secs(1);

        let err = generate_oci_token_at(
            "alice",
            "repository:alice/hello:pull",
            TEST_SECRET,
            300,
            before_epoch,
        )
        .expect_err("a clock before the epoch cannot stamp a token");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("before the Unix epoch"),
            "the operator has to be told the clock is the problem: {rendered}"
        );
        assert!(
            rendered.contains("correct the host clock"),
            "the error has to name the remedy, the failure is on the machine: {rendered}"
        );
    }

    #[test]
    fn the_injected_clock_is_the_one_that_stamps_iat_and_exp() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        let token =
            generate_oci_token_at("alice", "repository:alice/hello:pull", TEST_SECRET, 300, at)
                .expect("a representable clock stamps a token");

        let claims = decode::<OciTokenClaims>(
            &token,
            &DecodingKey::from_secret(TEST_SECRET.as_bytes()),
            &{
                // The token is stamped in 2023, so the default validation would
                // reject it as expired — this test is about the claims, not the
                // window.
                let mut v = Validation::default();
                v.validate_exp = false;
                v.validate_aud = false;
                v
            },
        )
        .expect("decodable token")
        .claims;

        assert_eq!(claims.iat, 1_700_000_000);
        assert_eq!(claims.exp, 1_700_000_300);
    }

    #[test]
    fn test_validate_invalid_token() {
        let result = validate_oci_token("invalid.jwt.token", TEST_SECRET);
        assert!(result.is_none());
    }

    #[test]
    fn test_validate_wrong_secret() {
        let token =
            generate_oci_token("alice", "repository:alice/hello:pull", TEST_SECRET, 300).unwrap();
        let wrong_secret = "wrong-secret";
        let result = validate_oci_token(&token, wrong_secret);
        assert!(result.is_none());
    }

    #[test]
    fn test_parsed_scope() {
        let scope = ParsedScope::parse("repository:alice/hello:pull,push").unwrap();
        assert_eq!(scope.scope_type, "repository");
        assert_eq!(scope.name, "alice/hello");
        assert!(scope.has_action("pull"));
        assert!(scope.has_action("push"));
        assert!(!scope.has_action("delete"));
        assert!(scope.matches_repo("alice", "hello"));
        assert!(!scope.matches_repo("alice", "world"));
    }

    #[test]
    fn test_parsed_scope_wildcard() {
        let scope = ParsedScope::parse("repository:alice/hello:*").unwrap();
        assert!(scope.has_action("pull"));
        assert!(scope.has_action("push"));
        assert!(scope.has_action("delete"));
    }

    #[test]
    fn test_parsed_scope_invalid() {
        assert!(ParsedScope::parse("invalid-scope").is_none());
        assert!(ParsedScope::parse("type:name").is_none()); // missing actions
    }

    #[test]
    fn test_build_www_authenticate() {
        let www = build_www_authenticate(
            "https://example.com/v2/auth/token",
            "registry",
            "repository:alice/hello:pull,push",
        );
        assert!(www.contains(r#"realm="https://example.com/v2/auth/token""#));
        assert!(www.contains(r#"service="registry""#));
        // Verbatim, not percent-encoded: the client copies this string into its
        // token request, so `%3A` here becomes a scope the token endpoint
        // cannot parse and therefore cannot grant.
        assert!(
            www.contains(r#"scope="repository:alice/hello:pull,push""#),
            "the challenge must carry the scope a client can ask for: {www}"
        );
    }
}
