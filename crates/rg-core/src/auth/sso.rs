//! OAuth2 / OIDC SSO service. Supports GitHub, GitLab, Google, and custom providers.
//!
//! # Security features
//! - PKCE (S256) for all OAuth2 providers (RFC 7636)
//! - CSRF state tied to a signed cookie
//! - Token refresh support
//! - OIDC Discovery for Google and generic OIDC providers
//!
//! # Outbound HTTP hardening
//! Every call to a provider endpoint (token / userinfo / discovery) goes
//! through the shared [`crate::net::outbound_client`], which bounds every
//! request with a request + connect timeout and follows **no** redirects — so a
//! slow/hanging IdP can't pin a login task forever, and a `3xx` from a token or
//! userinfo endpoint can't bounce the bearer token to an internal host.
//!
//! Unlike webhook delivery, SSO endpoints are **admin-configured** (or come from
//! the provider's own OIDC discovery document), not arbitrary user input, and a
//! self-hosted forge legitimately points SSO at an *internal* IdP (self-hosted
//! Keycloak / GitLab on a private address). We therefore deliberately do **not**
//! run these endpoints through [`crate::net::guard_outbound_url`]'s private-IP
//! rejection — doing so would break that supported deployment. The timeout +
//! redirect ban are the parts of the hardening that apply regardless of trust
//! boundary.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};

// ── Public types ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SsoProviderConfig {
    pub slug: String,
    pub provider_type: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub scopes: Vec<String>,
    pub discovery_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SsoUserInfo {
    pub provider_user_id: String,
    pub provider_username: String,
    pub email: String,
    /// What the provider said about the address in `email`: `Some(true)` when
    /// it vouched for it, `Some(false)` when it explicitly said it is
    /// unconfirmed, `None` when it offered no signal either way.
    pub email_verified: Option<bool>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Why a provider's answer cannot be used to identify a ForgeKeep account.
///
/// Each variant is the provider's answer being refused, not a server fault:
/// they reach the person signing in as a `400` with the reason, because the fix
/// (grant the `user:email` scope, confirm the address) is on that side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsoIdentityDefect {
    /// The provider named no stable id for this account.
    MissingProviderUserId,
    /// The provider returned no email address at all.
    MissingEmail,
    /// The provider returned something that is not an address.
    MalformedEmail,
    /// The provider returned an address and said it is unconfirmed.
    UnverifiedEmail,
    /// No username could be derived for the local account.
    UnusableUsername,
}

impl SsoIdentityDefect {
    /// The reason as the client sees it. Deliberately actionable: the common
    /// cause of the first two is an OAuth app without the `user:email` scope.
    pub const fn message(self) -> &'static str {
        match self {
            Self::MissingProviderUserId => "the SSO provider returned no account id for this login",
            Self::MissingEmail => {
                "the SSO provider returned no email address for this login; \
                 grant the provider access to your email (GitHub: the `user:email` scope)"
            }
            Self::MalformedEmail => "the SSO provider returned a malformed email address",
            Self::UnverifiedEmail => {
                "the SSO provider reports this email address as unverified; \
                 confirm it with the provider and sign in again"
            }
            Self::UnusableUsername => {
                "no ForgeKeep username could be derived from this SSO profile"
            }
        }
    }
}

impl std::fmt::Display for SsoIdentityDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for SsoIdentityDefect {}

/// A provider profile whose identity keys have been proven usable.
///
/// The account lookup behind an SSO callback runs on two values that come
/// straight out of somebody else's JSON — `provider_user_id` and `email`. An
/// absent field collected through `unwrap_or_default()` becomes `""`, and `""`
/// is a perfectly good key: the second person whose provider withheld the same
/// field matches the first one's row and signs into their account.
///
/// So the check is a type rather than a convention. [`SsoUserInfo::into_identity`]
/// is the only way to build an `SsoIdentity`, and every function that turns
/// these values into a database lookup takes one — a call site cannot forget to
/// ask, and a future one cannot be added that skips the question.
#[derive(Debug, Clone)]
pub struct SsoIdentity(SsoUserInfo);

impl SsoIdentity {
    /// The profile behind the proof. Read-only: mutating it would invalidate
    /// exactly the guarantee this type carries.
    pub fn profile(&self) -> &SsoUserInfo {
        &self.0
    }
}

impl std::ops::Deref for SsoIdentity {
    type Target = SsoUserInfo;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl SsoUserInfo {
    /// Check the two values the account lookup keys on, without consuming the
    /// profile. Whitespace counts as absent — `" "` and `""` are the same
    /// missing field to every consumer downstream.
    pub fn check_identity_keys(&self) -> Result<(), SsoIdentityDefect> {
        if self.provider_user_id.trim().is_empty() {
            return Err(SsoIdentityDefect::MissingProviderUserId);
        }

        let email = self.email.trim();
        if email.is_empty() {
            return Err(SsoIdentityDefect::MissingEmail);
        }
        if !crate::user::service::valid_email(email) {
            return Err(SsoIdentityDefect::MalformedEmail);
        }
        // Only an explicit denial refuses the login. A provider that says
        // nothing about the address (GitHub's profile fallback, GitLab) leaves
        // `None` here, and inventing a verdict for it would either lock out
        // working deployments or claim a guarantee nobody gave.
        if self.email_verified == Some(false) {
            return Err(SsoIdentityDefect::UnverifiedEmail);
        }

        Ok(())
    }

    /// Normalise the provider's strings and prove the identity keys.
    ///
    /// The username is the one field allowed to be missing *and* the one field
    /// allowed to be wrong: it names the local account rather than finding an
    /// existing one, so a value the local rule refuses is repaired rather than
    /// turned into a failed login. What it may not be is *unchecked* — it lands
    /// in `users.username`, which addresses a URL path and shares one namespace
    /// with organisation names, so the provider's spelling passes through
    /// [`sanitize_username`] before it can become that key.
    pub fn into_identity(mut self) -> Result<SsoIdentity, SsoIdentityDefect> {
        self.provider_user_id = self.provider_user_id.trim().to_string();
        self.provider_username = self.provider_username.trim().to_string();
        self.email = self.email.trim().to_string();

        self.check_identity_keys()?;

        // The address is the fallback for both "no username at all" and "a
        // username with nothing usable in it" — by this point it is proven, so
        // it is the better of the two sources either way.
        self.provider_username = sanitize_username(&self.provider_username)
            .or_else(|| username_from_email(&self.email))
            .ok_or(SsoIdentityDefect::UnusableUsername)?;

        Ok(SsoIdentity(self))
    }
}

/// Longest base username [`sanitize_username`] may return.
///
/// `provision_sso_user` appends a uniqueness suffix to this base — `_1` … `_99`
/// first, then `_` plus six random letters — and the result still has to fit
/// the 30-character limit [`crate::user::service::validate_username`] enforces.
/// 23 + 7 is that limit exactly.
const SSO_USERNAME_BASE_MAX: usize = 23;

/// Shortest username the local rule accepts.
const SSO_USERNAME_MIN: usize = 3;

/// Reduce a name the *provider* chose to one the local rule accepts.
///
/// `users.username` is a local key: it addresses `/{username}/{repo}`, it is
/// what `find_by_username` matches, and it shares a namespace with organisation
/// names. Self-registration has had to satisfy
/// [`crate::user::service::validate_username`] for that reason all along, while
/// SSO provisioning wrote whatever the provider's JSON carried — an OIDC
/// `preferred_username` of `"John Doe"` or `"a/../b"` became exactly that
/// `users.username`.
///
/// So the provider's spelling is repaired instead of trusted:
///
/// * everything outside `[A-Za-z0-9_-]` is dropped (spaces, dots, `/`, and with
///   them any `..`),
/// * leading characters are dropped until the first alphanumeric one, because
///   the rule requires the name to start with one,
/// * the result is cut to [`SSO_USERNAME_BASE_MAX`],
/// * and a 1–2 character remainder is padded rather than refused — the local
///   minimum exists to keep names readable, not to lock out a person whose
///   provider name is short.
///
/// `None` means nothing usable survived, which is the caller's cue to try the
/// address instead.
fn sanitize_username(candidate: &str) -> Option<String> {
    let filtered: String = candidate
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();

    // ASCII-only by construction above, so byte indexing is safe here.
    let first_alphanumeric = filtered.find(|c: char| c.is_ascii_alphanumeric())?;
    let mut name: String = filtered[first_alphanumeric..]
        .chars()
        .take(SSO_USERNAME_BASE_MAX)
        .collect();
    while name.len() < SSO_USERNAME_MIN {
        name.push('0');
    }

    Some(name)
}

/// Derive a local username from an address when the provider withheld its own.
///
/// The local part goes through the same repair as a provider-supplied name —
/// this value ends up in `users.username` too, so a local part carrying `/` or
/// `..` must not survive into it.
fn username_from_email(email: &str) -> Option<String> {
    sanitize_username(email.split('@').next().unwrap_or_default())
}

/// Read the OIDC `email_verified` claim, tolerating the IdPs that send it as a
/// string. `None` means the provider said nothing — see
/// [`SsoUserInfo::check_identity_keys`] for why that is not the same as `false`.
fn oidc_email_verified(user: &serde_json::Value) -> Option<bool> {
    let claim = &user["email_verified"];
    claim
        .as_bool()
        .or_else(|| claim.as_str().and_then(|value| value.parse::<bool>().ok()))
}

/// Result of an OAuth2 authorization code exchange.
#[derive(Debug, Clone)]
pub struct OAuth2TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

// ── Authorization URL generation ─────────────────────────────────

/// Generate OAuth2 / OIDC authorization URL with PKCE S256.
/// Returns (auth_url, csrf_state, code_verifier).
pub async fn oauth2_authorize_url(config: &SsoProviderConfig) -> Result<(String, String, String)> {
    // PKCE: generate code_verifier (43-128 URL-safe chars per RFC 7636)
    let code_verifier: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    let code_challenge = pkce_s256_challenge(&code_verifier);

    // CSRF state: 32-char random
    let csrf_state: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let scopes = config.scopes.join(" ");

    // Build auth URL — use discovery for OIDC, default endpoints for OAuth2
    let base_url = match config.provider_type.as_str() {
        "oidc" => resolve_oidc_endpoints(config).await?.authorization_endpoint,
        _ => config
            .default_oauth2_auth_url()
            .ok_or_else(|| anyhow::anyhow!("no auth URL for provider: {}", config.slug))?,
    };

    let url = format!(
        "{}?client_id={}&redirect_uri={}&scope={}&state={}&response_type=code&code_challenge={}&code_challenge_method=S256",
        base_url,
        url_encode(&config.client_id),
        url_encode(&config.redirect_url),
        url_encode(&scopes),
        url_encode(&csrf_state),
        url_encode(&code_challenge),
    );

    Ok((url, csrf_state, code_verifier))
}

// ── Token exchange with PKCE ─────────────────────────────────────

/// Exchange authorization code for tokens. Supports PKCE code_verifier.
/// Returns access_token + optional refresh_token.
pub async fn oauth2_exchange_code(
    config: &SsoProviderConfig,
    code: &str,
    code_verifier: &str,
) -> Result<OAuth2TokenResponse> {
    let token_url = if config.provider_type == "oidc" {
        resolve_oidc_endpoints(config).await?.token_endpoint
    } else {
        config
            .default_oauth2_token_url()
            .ok_or_else(|| anyhow::anyhow!("no token URL for provider: {}", config.slug))?
    };

    let client = crate::net::outbound_client();

    #[derive(Deserialize)]
    struct RawTokenResponse {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: Option<u64>,
    }

    let resp = client
        .post(&token_url)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", config.redirect_url.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", code_verifier),
        ])
        .header("Accept", "application/json")
        .send()
        .await
        .context("failed to exchange OAuth2 code")?;

    let raw: RawTokenResponse = resp
        .error_for_status()
        .context("OAuth2 token endpoint returned an error")?
        .json()
        .await
        .context("failed to parse token response")?;

    Ok(OAuth2TokenResponse {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token,
        expires_in: raw.expires_in,
    })
}

// ── Token refresh ────────────────────────────────────────────────

/// Refresh an access token using a refresh_token.
pub async fn oauth2_refresh_token(
    config: &SsoProviderConfig,
    refresh_token: &str,
) -> Result<OAuth2TokenResponse> {
    let token_url = if config.provider_type == "oidc" {
        resolve_oidc_endpoints(config).await?.token_endpoint
    } else {
        config
            .default_oauth2_token_url()
            .ok_or_else(|| anyhow::anyhow!("no token URL for provider: {}", config.slug))?
    };

    let client = crate::net::outbound_client();

    #[derive(Deserialize)]
    struct RawTokenResponse {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: Option<u64>,
    }

    let resp = client
        .post(&token_url)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .header("Accept", "application/json")
        .send()
        .await
        .context("failed to refresh OAuth2 token")?;

    let raw: RawTokenResponse = resp
        .error_for_status()
        .context("OAuth2 token endpoint returned an error")?
        .json()
        .await
        .context("failed to parse token refresh response")?;

    Ok(OAuth2TokenResponse {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token,
        expires_in: raw.expires_in,
    })
}

// ── User info fetching ───────────────────────────────────────────

/// Fetch user info from an OAuth2/OIDC provider.
///
/// The identity gate runs here rather than at the call site: what leaves this
/// function is an [`SsoIdentity`], so a profile that cannot identify a person
/// never reaches an account lookup — see the type's documentation for what that
/// buys. The refusal is an [`SsoIdentityDefect`] inside the returned error, and
/// the HTTP layer downcasts to it to answer with the actual reason instead of
/// the generic "could not reach the provider".
pub async fn oauth2_fetch_user_info(
    config: &SsoProviderConfig,
    access_token: &str,
) -> Result<SsoIdentity> {
    let info = match config.slug.as_str() {
        "github" => fetch_github_user(access_token).await,
        "gitlab" => fetch_gitlab_user(access_token).await,
        "google" => fetch_google_user(access_token).await,
        _ => {
            // For unknown OIDC providers, try the standard userinfo endpoint
            if config.provider_type == "oidc" {
                fetch_oidc_userinfo(config, access_token).await
            } else {
                Err(anyhow::anyhow!("unsupported SSO provider: {}", config.slug))
            }
        }
    }?;

    Ok(info.into_identity()?)
}

// ── GitHub user info ─────────────────────────────────────────────

async fn fetch_github_user(access_token: &str) -> Result<SsoUserInfo> {
    let client = crate::net::outbound_client();

    let user_resp = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "ForgeKeep/0.1")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("GitHub user API request failed")?;

    // Without this, a refusal is parsed as a profile: GitHub answers `401` with
    // a JSON body, `user["id"]` is simply absent in it, and the login used to
    // fail as "the provider returned no account id for this login" — pointing
    // the person at a scope when the token was the problem.
    let user: serde_json::Value = user_resp
        .error_for_status()
        .context("GitHub user API returned an error")?
        .json()
        .await
        .context("failed to parse GitHub user response")?;

    let provider_user_id = user["id"]
        .as_i64()
        .map(|id| id.to_string())
        .or_else(|| user["node_id"].as_str().map(|s| s.to_string()))
        .unwrap_or_default();

    let provider_username = user["login"].as_str().unwrap_or("").to_string();
    let display_name = user["name"].as_str().map(str::to_string);
    let avatar_url = user["avatar_url"].as_str().map(str::to_string);
    // `/user/emails` only ever yields an address GitHub marked verified, so
    // that branch carries the provider's word for it. The profile fallback is a
    // different payload with no `verified` flag on it — claiming one would be
    // inventing a guarantee, so it stays `None`.
    let (email, email_verified) = match fetch_github_email(client, access_token).await? {
        Some(email) => (email, Some(true)),
        None => (user["email"].as_str().unwrap_or("").to_string(), None),
    };

    Ok(SsoUserInfo {
        provider_user_id,
        provider_username,
        email,
        email_verified,
        display_name,
        avatar_url,
    })
}

/// Ask GitHub for the addresses it has confirmed for this account.
///
/// Three-valued on purpose:
///
/// * `Ok(Some(address))` — GitHub vouched for that address.
/// * `Ok(None)` — the question was answered and there is no confirmed address
///   to hand out, *or* this OAuth app was never granted the `user:email` scope.
///   Either way the caller's profile fallback is the intended next step.
/// * `Err` — the question could not be asked at all.
///
/// It used to be `Option<String>` built from two `.ok()?`, so a timeout, a DNS
/// failure, a GitHub `500` and a truncated body were all indistinguishable from
/// "this account has no verified address". The caller then fell back to the
/// profile field — most often `""` — and an empty email is precisely the key
/// that used to find a *different* person's account
/// (see [`SsoIdentity`]). Refusing the login on "could not ask" is the honest
/// answer, and it reaches the operator as a logged reason rather than as a
/// silently weaker identity.
async fn fetch_github_email(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<Option<String>> {
    let resp = client
        .get("https://api.github.com/user/emails")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "ForgeKeep/0.1")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("GitHub user emails API request failed")?;

    // An OAuth app without the `user:email` scope is refused here rather than
    // told the list is empty. That is an *answer* — the deployment never asked
    // for the scope — so it stays a fallback to the profile address instead of
    // failing a sign-in that has worked all along. It is still worth a line:
    // silence is how this ends up looking like an account with no email.
    let status = resp.status();
    if matches!(status.as_u16(), 401 | 403 | 404) {
        tracing::warn!(
            status = status.as_u16(),
            "GitHub refused the verified-email list; falling back to the profile address \
             (grant the OAuth app the `user:email` scope to avoid this)"
        );
        return Ok(None);
    }

    let emails: Vec<serde_json::Value> = resp
        .error_for_status()
        .context("GitHub user emails API returned an error")?
        .json()
        .await
        .context("failed to parse the GitHub user emails response")?;

    for email in &emails {
        let primary = email["primary"].as_bool().unwrap_or(false);
        let verified = email["verified"].as_bool().unwrap_or(false);
        if primary && verified {
            if let Some(e) = email["email"].as_str() {
                return Ok(Some(e.to_string()));
            }
        }
    }

    Ok(emails
        .iter()
        .find(|e| e["verified"].as_bool().unwrap_or(false))
        .and_then(|e| e["email"].as_str())
        .map(str::to_string))
}

// ── GitLab user info ─────────────────────────────────────────────

async fn fetch_gitlab_user(access_token: &str) -> Result<SsoUserInfo> {
    let client = crate::net::outbound_client();
    let resp = client
        .get("https://gitlab.com/api/v4/user")
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .context("GitLab user API request failed")?;

    // Same reason as the GitHub profile fetch: an error body parses as a
    // profile with every field missing, and the identity gate then reports the
    // provider's refusal as a provider that named nobody.
    let user: serde_json::Value = resp
        .error_for_status()
        .context("GitLab user API returned an error")?
        .json()
        .await
        .context("failed to parse GitLab user response")?;

    Ok(SsoUserInfo {
        provider_user_id: user["id"]
            .as_i64()
            .map(|id| id.to_string())
            .unwrap_or_default(),
        provider_username: user["username"].as_str().unwrap_or("").to_string(),
        email: user["email"].as_str().unwrap_or("").to_string(),
        // `/api/v4/user` carries no verification flag for the primary address.
        email_verified: None,
        display_name: user["name"].as_str().map(str::to_string),
        avatar_url: user["avatar_url"].as_str().map(str::to_string),
    })
}

// ── Google OIDC user info ────────────────────────────────────────

async fn fetch_google_user(access_token: &str) -> Result<SsoUserInfo> {
    let client = crate::net::outbound_client();
    let resp = client
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .context("Google userinfo API request failed")?;

    let user: serde_json::Value = resp
        .error_for_status()
        .context("Google userinfo API returned an error")?
        .json()
        .await
        .context("failed to parse Google userinfo response")?;

    Ok(SsoUserInfo {
        provider_user_id: user["sub"].as_str().unwrap_or_default().to_string(),
        provider_username: user["email"]
            .as_str()
            .unwrap_or("")
            .split('@')
            .next()
            .unwrap_or("")
            .to_string(),
        email: user["email"].as_str().unwrap_or("").to_string(),
        email_verified: oidc_email_verified(&user),
        display_name: user["name"].as_str().map(str::to_string),
        avatar_url: user["picture"].as_str().map(str::to_string),
    })
}

// ── Generic OIDC userinfo ────────────────────────────────────────

async fn fetch_oidc_userinfo(
    config: &SsoProviderConfig,
    access_token: &str,
) -> Result<SsoUserInfo> {
    let client = crate::net::outbound_client();
    let endpoint = resolve_oidc_endpoints(config).await?.userinfo_endpoint;
    let user = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .context("OIDC userinfo request failed")?
        .error_for_status()
        .context("OIDC userinfo endpoint returned an error")?
        .json::<serde_json::Value>()
        .await
        .context("failed to parse OIDC userinfo response")?;

    Ok(SsoUserInfo {
        provider_user_id: user["sub"].as_str().unwrap_or_default().to_string(),
        provider_username: user["preferred_username"]
            .as_str()
            .or(user["email"].as_str().and_then(|e| e.split('@').next()))
            .unwrap_or("")
            .to_string(),
        email: user["email"].as_str().unwrap_or("").to_string(),
        email_verified: oidc_email_verified(&user),
        display_name: user["name"].as_str().map(str::to_string),
        avatar_url: user["picture"].as_str().map(str::to_string),
    })
}

#[derive(Debug, Deserialize)]
struct OidcEndpoints {
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
}

async fn resolve_oidc_endpoints(config: &SsoProviderConfig) -> Result<OidcEndpoints> {
    if let Some(discovery_url) = config
        .discovery_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    {
        return crate::net::outbound_client()
            .get(discovery_url)
            .send()
            .await
            .context("OIDC discovery request failed")?
            .error_for_status()
            .context("OIDC discovery endpoint returned an error")?
            .json::<OidcEndpoints>()
            .await
            .context("failed to parse OIDC discovery document");
    }

    if config.slug == "google" {
        return Ok(OidcEndpoints {
            authorization_endpoint: default_oidc_auth_url("google").unwrap_or_default(),
            token_endpoint: default_oidc_token_url("google").unwrap_or_default(),
            userinfo_endpoint: "https://openidconnect.googleapis.com/v1/userinfo".to_string(),
        });
    }

    anyhow::bail!("OIDC provider '{}' requires a discovery URL", config.slug)
}

// ── PKCE helpers ─────────────────────────────────────────────────

/// Generate PKCE S256 code challenge from code_verifier.
fn pkce_s256_challenge(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest.as_slice())
}

// ── URL helpers ──────────────────────────────────────────────────

/// Minimal percent-encoding for OAuth2 query parameters.
fn url_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => c.to_string(),
            ' ' => "%20".to_string(),
            other => {
                let bytes = other.to_string().into_bytes();
                bytes
                    .iter()
                    .map(|b| format!("%{:02X}", b))
                    .collect::<Vec<_>>()
                    .join("")
            }
        })
        .collect()
}

// ── Default endpoints ────────────────────────────────────────────

impl SsoProviderConfig {
    fn default_oauth2_auth_url(&self) -> Option<String> {
        match self.slug.as_str() {
            "github" => Some("https://github.com/login/oauth/authorize".into()),
            "gitlab" => Some("https://gitlab.com/oauth/authorize".into()),
            _ => None,
        }
    }

    fn default_oauth2_token_url(&self) -> Option<String> {
        match self.slug.as_str() {
            "github" => Some("https://github.com/login/oauth/access_token".into()),
            "gitlab" => Some("https://gitlab.com/oauth/token".into()),
            _ => None,
        }
    }
}

fn default_oidc_auth_url(slug: &str) -> Option<String> {
    match slug {
        "google" => Some("https://accounts.google.com/o/oauth2/v2/auth".into()),
        _ => None,
    }
}

fn default_oidc_token_url(slug: &str) -> Option<String> {
    match slug {
        "google" => Some("https://oauth2.googleapis.com/token".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{oidc_email_verified, SsoIdentityDefect, SsoUserInfo};

    fn profile(uid: &str, username: &str, email: &str) -> SsoUserInfo {
        SsoUserInfo {
            provider_user_id: uid.to_string(),
            provider_username: username.to_string(),
            email: email.to_string(),
            email_verified: None,
            display_name: None,
            avatar_url: None,
        }
    }

    /// The bug this gate exists for: two people whose provider withheld the
    /// email both used to arrive with `email = ""`, and `""` is a working key
    /// for `find_by_email` — the second one signed into the first one's
    /// account. Neither may become an identity now, so no lookup runs at all.
    #[test]
    fn two_profiles_without_an_email_can_never_meet_in_one_account() {
        let first = profile("provider-uid-1", "alice", "").into_identity();
        let second = profile("provider-uid-2", "bob", "").into_identity();

        assert_eq!(first.unwrap_err(), SsoIdentityDefect::MissingEmail);
        assert_eq!(second.unwrap_err(), SsoIdentityDefect::MissingEmail);
    }

    /// Same shape on the other key: an absent `sub` / `id` used to reach
    /// `find_by_provider_and_uid` as `""` and match whichever OAuth link was
    /// written with it first.
    #[test]
    fn a_profile_without_a_provider_id_never_reaches_the_oauth_link_lookup() {
        assert_eq!(
            profile("", "alice", "alice@example.com")
                .into_identity()
                .unwrap_err(),
            SsoIdentityDefect::MissingProviderUserId,
        );
    }

    /// A field of blanks is a field the provider did not send; `" "` must not
    /// buy a login that `""` is refused.
    #[test]
    fn whitespace_is_the_same_answer_as_nothing() {
        assert_eq!(
            profile("  ", "alice", "alice@example.com")
                .into_identity()
                .unwrap_err(),
            SsoIdentityDefect::MissingProviderUserId,
        );
        assert_eq!(
            profile("provider-uid-1", "alice", "\t ")
                .into_identity()
                .unwrap_err(),
            SsoIdentityDefect::MissingEmail,
        );
    }

    #[test]
    fn an_address_that_is_not_an_address_is_refused() {
        assert_eq!(
            profile("provider-uid-1", "alice", "alice-at-example.com")
                .into_identity()
                .unwrap_err(),
            SsoIdentityDefect::MalformedEmail,
        );
        assert_eq!(
            profile("provider-uid-1", "alice", "@example.com")
                .into_identity()
                .unwrap_err(),
            SsoIdentityDefect::MalformedEmail,
        );
    }

    /// The address is the key an existing local account is *merged* on, so a
    /// provider that says out loud it has not confirmed it does not get to
    /// nominate whose account this is.
    #[test]
    fn an_address_the_provider_calls_unverified_is_refused() {
        let mut unverified = profile("provider-uid-1", "alice", "alice@example.com");
        unverified.email_verified = Some(false);

        assert_eq!(
            unverified.into_identity().unwrap_err(),
            SsoIdentityDefect::UnverifiedEmail,
        );
    }

    /// Silence is not a denial: most providers send no verification signal at
    /// all, and refusing them would lock out working deployments.
    #[test]
    fn a_provider_that_says_nothing_about_verification_still_signs_in() {
        let identity = profile("provider-uid-1", "alice", "alice@example.com")
            .into_identity()
            .expect("no signal is not a negative signal");

        assert_eq!(identity.email, "alice@example.com");
    }

    /// The username names the new local account instead of finding an existing
    /// one, so a provider that withheld it costs a derivation, not a refusal.
    #[test]
    fn a_missing_username_is_derived_from_the_address() {
        let identity = profile("provider-uid-1", "", "alice.smith@example.com")
            .into_identity()
            .expect("the address carries enough to name the account");

        assert_eq!(identity.provider_username, "alicesmith");
    }

    /// That derived value lands in `users.username`, which also addresses a URL
    /// path — so the local part is filtered, not copied. The trailing `0` is
    /// the local minimum length being met, not part of the address.
    #[test]
    fn a_derived_username_carries_no_path_characters() {
        let identity = profile("provider-uid-1", "", "a/../b@example.com")
            .into_identity()
            .expect("something usable survives the filter");

        assert_eq!(identity.provider_username, "ab0");
        assert!(crate::user::service::validate_username(&identity.provider_username).is_ok());
    }

    #[test]
    fn a_local_part_with_nothing_usable_in_it_is_refused() {
        assert_eq!(
            profile("provider-uid-1", "", "...@example.com")
                .into_identity()
                .unwrap_err(),
            SsoIdentityDefect::UnusableUsername,
        );
    }

    /// The bug this repair exists for: an OIDC `preferred_username` is a
    /// display string on the provider's side and a *key* on ours. Copying it
    /// put a space — and a URL path separator — into `users.username`.
    #[test]
    fn a_provider_username_the_local_rule_refuses_is_repaired_not_copied() {
        for (provider_name, expected) in [
            ("John Doe", "JohnDoe"),
            ("a.b/c", "abc"),
            ("a/../b", "ab0"),
            ("_leading", "leading"),
            ("ünïcode", "ncode"),
            ("x", "x00"),
        ] {
            let identity = profile("provider-uid-1", provider_name, "alice@example.com")
                .into_identity()
                .unwrap_or_else(|error| {
                    panic!("`{provider_name}` should name an account, got {error:?}")
                });

            assert_eq!(identity.provider_username, expected, "from `{provider_name}`");
            assert!(
                crate::user::service::validate_username(&identity.provider_username).is_ok(),
                "`{}` must satisfy the same rule self-registration does",
                identity.provider_username
            );
        }
    }

    /// A provider name with nothing usable in it falls back to the address
    /// rather than failing the login — the address is already proven by now.
    #[test]
    fn an_unusable_provider_username_falls_back_to_the_address() {
        let identity = profile("provider-uid-1", "...", "alice.smith@example.com")
            .into_identity()
            .expect("the address still names the account");

        assert_eq!(identity.provider_username, "alicesmith");
    }

    /// The base has to leave room for the uniqueness suffix
    /// `provision_sso_user` appends, or the *generated* name breaks the rule
    /// the base was trimmed to satisfy.
    #[test]
    fn a_long_provider_username_leaves_room_for_the_uniqueness_suffix() {
        let identity = profile("provider-uid-1", &"a".repeat(64), "alice@example.com")
            .into_identity()
            .expect("a long name is cut, not refused");

        assert_eq!(identity.provider_username.len(), super::SSO_USERNAME_BASE_MAX);
        for suffix in ["_99", "_abcdef"] {
            let generated = format!("{}{suffix}", identity.provider_username);
            assert!(
                crate::user::service::validate_username(&generated).is_ok(),
                "`{generated}` must still satisfy the local rule"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_never_reaches_the_stored_account() {
        let identity = profile(" provider-uid-1 ", " alice ", " alice@example.com ")
            .into_identity()
            .expect("trimmed values are usable");

        assert_eq!(identity.provider_user_id, "provider-uid-1");
        assert_eq!(identity.provider_username, "alice");
        assert_eq!(identity.email, "alice@example.com");
    }

    /// Some IdPs send the OIDC claim as a JSON string. Reading `"false"` as
    /// "no answer" would silently drop the one signal that refuses a merge.
    #[test]
    fn the_email_verified_claim_is_read_as_bool_or_string() {
        assert_eq!(
            oidc_email_verified(&serde_json::json!({"email_verified": true})),
            Some(true)
        );
        assert_eq!(
            oidc_email_verified(&serde_json::json!({"email_verified": false})),
            Some(false)
        );
        assert_eq!(
            oidc_email_verified(&serde_json::json!({"email_verified": "false"})),
            Some(false)
        );
        assert_eq!(
            oidc_email_verified(&serde_json::json!({"email_verified": "true"})),
            Some(true)
        );
        assert_eq!(oidc_email_verified(&serde_json::json!({})), None);
        assert_eq!(
            oidc_email_verified(&serde_json::json!({"email_verified": "yes"})),
            None,
            "an unparseable claim is no answer, not a negative one"
        );
    }
}
