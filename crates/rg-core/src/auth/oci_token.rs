//! OCI Distribution Spec — Bearer Token authentication.
//!
//! Implements [OCI Distribution Spec v1.0 — Token Authentication]
//! (https://docs.docker.com/registry/spec/auth/token/).
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

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
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
    // URL-encode the parameters
    let scope_encoded = urlencoding::encode(scope);
    format!(
        r#"Bearer realm="{}",service="{}",scope="{}""#,
        realm, service, scope_encoded
    )
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(www.contains("scope="));
    }
}
