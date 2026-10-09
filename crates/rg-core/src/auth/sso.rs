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
//! run these endpoints through [`crate::net::ssrf_safe_outbound_client`]'s
//! connector-level private-IP rejection — doing so would break that supported
//! deployment. Transport confidentiality is independent: HTTPS is required
//! unless the instance operator names an exact HTTP origin in
//! [`OidcTransportPolicy`].

use std::collections::HashSet;
use std::sync::Arc;

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
    pub transport_policy: OidcTransportPolicy,
}

/// Exact plaintext HTTP origins on which an operator explicitly permits OIDC
/// discovery and credential-bearing token/userinfo traffic.
///
/// This is intentionally one policy for all three endpoints but an exact
/// origin set: allowing the discovery server does not let its document grant a
/// neighbouring authority permission to receive the client secret or Bearer
/// token.
#[derive(Clone, Debug, Default)]
pub struct OidcTransportPolicy(Arc<HashSet<crate::net::HttpOrigin>>);

impl OidcTransportPolicy {
    /// Parse `[auth].allow_insecure_oidc_origins` as exact HTTP origins.
    pub fn parse(values: &[String]) -> Result<Self> {
        let mut origins = HashSet::with_capacity(values.len());
        for value in values {
            let origin =
                crate::net::HttpOrigin::from_config(value, "plaintext OIDC opt-in origin")?;
            if origin.scheme != "http" {
                anyhow::bail!(
                    "plaintext OIDC opt-in origin '{}' must use http",
                    value.trim()
                );
            }
            origins.insert(origin);
        }
        Ok(Self(Arc::new(origins)))
    }

    /// Whether at least one plaintext OIDC origin was explicitly named.
    pub fn allows_insecure_http(&self) -> bool {
        !self.0.is_empty()
    }

    /// Require HTTPS unless the exact HTTP origin has an operator exception.
    pub fn require_confidential_endpoint(&self, raw: &str, endpoint: &str) -> Result<()> {
        let url = reqwest::Url::parse(raw)
            .with_context(|| format!("invalid OIDC {endpoint} endpoint '{raw}'"))?;
        match url.scheme() {
            "https" => Ok(()),
            "http"
                if crate::net::HttpOrigin::from_url(&url)
                    .is_some_and(|origin| self.0.contains(&origin)) =>
            {
                Ok(())
            }
            "http" => anyhow::bail!(
                "OIDC {endpoint} endpoint uses plaintext HTTP; use https:// or add its exact \
                 origin to `[auth].allow_insecure_oidc_origins` as an instance-operator exception"
            ),
            scheme => anyhow::bail!("OIDC {endpoint} endpoint must use https, not '{scheme}'"),
        }
    }
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

/// Why a provider's answer cannot be used to identify a Plombir Git account.
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
                "no Plombir Git username could be derived from this SSO profile"
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

// ── Talking to a host we do not own ──────────────────────────────

/// Tag a failed call to an identity provider as *its* failure.
///
/// Every request in this module goes to a host this instance does not own, and
/// none of them can be fixed by the person signing in: a DNS failure, a
/// connect timeout, a provider `500` and a truncated body are all "we could not
/// ask", not "your request is wrong". Flattened into a bare `anyhow::Error`
/// they were indistinguishable from a genuine client fault, and the SSO
/// callback answered `400` to all of them.
///
/// The marker is layered as *context* over the transport error, so the operator
/// log still gets the underlying reqwest cause under `{:#}` — see
/// [`crate::error::UpstreamUnavailable`].
trait ProviderCall<T> {
    fn provider_call(self, what: &str) -> Result<T>;
}

impl<T> ProviderCall<T> for std::result::Result<T, reqwest::Error> {
    fn provider_call(self, what: &str) -> Result<T> {
        self.map_err(|error| {
            anyhow::Error::new(error).context(crate::error::UpstreamUnavailable::new(what))
        })
    }
}

/// Classify an error status from a provider's **token** endpoint.
///
/// The one place where a provider's refusal really can be the caller's doing:
/// an authorization code that has expired or was already redeemed comes back
/// as a `4xx`, and starting the login again is exactly the right remedy — so
/// that stays a `400`. A `429`, a `5xx`, no answer at all or an unparseable
/// body is the provider failing, and telling the client to fix its request
/// there is both wrong and un-retryable.
fn token_endpoint_error(what: &str, error: reqwest::Error) -> anyhow::Error {
    match error.status() {
        Some(status)
            if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS =>
        {
            anyhow::Error::new(error).context(crate::error::InvalidRequest::new(
                "the identity provider rejected the authorization grant; start the sign-in again",
            ))
        }
        _ => anyhow::Error::new(error).context(crate::error::UpstreamUnavailable::new(what)),
    }
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
        .provider_call("failed to exchange OAuth2 code")?;

    let raw: RawTokenResponse = resp
        .error_for_status()
        .map_err(|error| token_endpoint_error("OAuth2 token endpoint returned an error", error))?
        .json()
        .await
        .provider_call("failed to parse the token response")?;

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
    fetch_user_info_with_builtin_urls(config, access_token, &BUILTIN_PROFILE_URLS).await
}

struct BuiltinProfileUrls<'a> {
    github_user: &'a str,
    github_emails: &'a str,
    gitlab_user: &'a str,
    google_user: &'a str,
}

const BUILTIN_PROFILE_URLS: BuiltinProfileUrls<'static> = BuiltinProfileUrls {
    github_user: "https://api.github.com/user",
    github_emails: "https://api.github.com/user/emails",
    gitlab_user: "https://gitlab.com/api/v4/user",
    google_user: "https://openidconnect.googleapis.com/v1/userinfo",
};

async fn fetch_user_info_with_builtin_urls(
    config: &SsoProviderConfig,
    access_token: &str,
    builtins: &BuiltinProfileUrls<'_>,
) -> Result<SsoIdentity> {
    let info = if config.provider_type == "oidc" {
        // The issuer and userinfo endpoint come from discovery, regardless of
        // the local slug. A built-in slug must not send this issuer's token to
        // the public OAuth2 provider with the same name.
        fetch_oidc_userinfo(config, access_token).await
    } else {
        match config.slug.as_str() {
            "github" => {
                fetch_github_user(access_token, builtins.github_user, builtins.github_emails).await
            }
            "gitlab" => fetch_gitlab_user(access_token, builtins.gitlab_user).await,
            "google" => fetch_google_user(access_token, builtins.google_user).await,
            _ => Err(anyhow::anyhow!("unsupported SSO provider: {}", config.slug)),
        }
    }?;

    Ok(info.into_identity()?)
}

// ── GitHub user info ─────────────────────────────────────────────

async fn fetch_github_user(
    access_token: &str,
    user_endpoint: &str,
    emails_endpoint: &str,
) -> Result<SsoUserInfo> {
    let client = crate::net::outbound_client();

    let user_resp = client
        .get(user_endpoint)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "PlombirGit/0.1")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .provider_call("GitHub user API request failed")?;

    // Without this, a refusal is parsed as a profile: GitHub answers `401` with
    // a JSON body, `user["id"]` is simply absent in it, and the login used to
    // fail as "the provider returned no account id for this login" — pointing
    // the person at a scope when the token was the problem.
    let user: serde_json::Value = user_resp
        .error_for_status()
        .provider_call("GitHub user API returned an error")?
        .json()
        .await
        .provider_call("failed to parse the GitHub user response")?;

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
    let (email, email_verified) =
        match fetch_github_email(client, access_token, emails_endpoint).await? {
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
    endpoint: &str,
) -> Result<Option<String>> {
    let resp = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "PlombirGit/0.1")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .provider_call("GitHub user emails API request failed")?;

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
        .provider_call("GitHub user emails API returned an error")?
        .json()
        .await
        .provider_call("failed to parse the GitHub user emails response")?;

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

async fn fetch_gitlab_user(access_token: &str, endpoint: &str) -> Result<SsoUserInfo> {
    let client = crate::net::outbound_client();
    let resp = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .provider_call("GitLab user API request failed")?;

    // Same reason as the GitHub profile fetch: an error body parses as a
    // profile with every field missing, and the identity gate then reports the
    // provider's refusal as a provider that named nobody.
    let user: serde_json::Value = resp
        .error_for_status()
        .provider_call("GitLab user API returned an error")?
        .json()
        .await
        .provider_call("failed to parse the GitLab user response")?;

    Ok(SsoUserInfo {
        provider_user_id: user["id"]
            .as_i64()
            .map(|id| id.to_string())
            .unwrap_or_default(),
        provider_username: user["username"].as_str().unwrap_or("").to_string(),
        email: user["email"].as_str().unwrap_or("").to_string(),
        email_verified: gitlab_email_verified(&user),
        display_name: user["name"].as_str().map(str::to_string),
        avatar_url: user["avatar_url"].as_str().map(str::to_string),
    })
}

/// GitLab's word on the primary address of `/api/v4/user`.
///
/// There is no `email_verified` there. What GitLab does keep is `confirmed_at`:
/// the account's address was confirmed, and a changed primary waits in
/// `unconfirmed_email` until it is confirmed too, so `email` is the confirmed
/// one whenever `confirmed_at` is set. Its absence is not a denial — older
/// servers and restricted tokens omit the field — so it stays `None`, which an
/// allowlist reads as "not vouched for" (card_7099e8a305bc).
fn gitlab_email_verified(user: &serde_json::Value) -> Option<bool> {
    user["confirmed_at"]
        .as_str()
        .filter(|confirmed_at| !confirmed_at.trim().is_empty())
        .map(|_| true)
}

// ── Google OIDC user info ────────────────────────────────────────

async fn fetch_google_user(access_token: &str, endpoint: &str) -> Result<SsoUserInfo> {
    let client = crate::net::outbound_client();
    let resp = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .provider_call("Google userinfo API request failed")?;

    let user: serde_json::Value = resp
        .error_for_status()
        .provider_call("Google userinfo API returned an error")?
        .json()
        .await
        .provider_call("failed to parse the Google userinfo response")?;

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
        .provider_call("OIDC userinfo request failed")?
        .error_for_status()
        .provider_call("OIDC userinfo endpoint returned an error")?
        .json::<serde_json::Value>()
        .await
        .provider_call("failed to parse the OIDC userinfo response")?;

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

/// The suffix OpenID Connect Discovery (§4) appends to an issuer URL.
const OIDC_DISCOVERY_SUFFIX: &str = "/.well-known/openid-configuration";

#[derive(Debug, Deserialize)]
struct OidcEndpoints {
    /// OpenID Connect Discovery 1.0 §4.3: the issuer URL the document was
    /// fetched from. Checked by [`Self::require_matching_issuer`].
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
}

impl OidcEndpoints {
    fn require_confidential_transport(&self, policy: &OidcTransportPolicy) -> Result<()> {
        // The authorization endpoint starts the login and carries the state
        // and PKCE challenge; the token and userinfo endpoints carry the
        // client secret and Bearer token. All three are decided by the same
        // document, so all three answer to the same transport rule.
        policy.require_confidential_endpoint(&self.authorization_endpoint, "authorization")?;
        policy.require_confidential_endpoint(&self.token_endpoint, "token")?;
        policy.require_confidential_endpoint(&self.userinfo_endpoint, "userinfo")
    }

    /// Reject a discovery document whose `issuer` is not the URL it was served
    /// from.
    ///
    /// OpenID Connect Discovery 1.0 §4.3: the `issuer` value "MUST be
    /// identical to the Issuer URL that was directly used to retrieve the
    /// configuration information". Without the check, any document reachable
    /// at the discovery URL — including one a DNS-rebind or a fetch of the
    /// wrong tenant put there — can present endpoints of its choosing while
    /// the operator believes the configured issuer answered.
    fn require_matching_issuer(&self, discovery_url: &str) -> Result<()> {
        let expected = discovery_url
            .strip_suffix(OIDC_DISCOVERY_SUFFIX)
            .with_context(|| {
                format!(
                    "OIDC discovery URL '{discovery_url}' does not end in \
                     '{OIDC_DISCOVERY_SUFFIX}', so its issuer cannot be verified"
                )
            })?;
        // Issuers differ on a trailing slash across implementations; §4.3
        // errata and the certification suite treat `https://idp` and
        // `https://idp/` as the same issuer.
        if self.issuer.trim_end_matches('/') != expected.trim_end_matches('/') {
            anyhow::bail!(
                "OIDC discovery document claims issuer '{}', which is not the discovery URL \
                 issuer '{expected}'; refusing to use its endpoints",
                self.issuer
            );
        }
        Ok(())
    }
}

async fn resolve_oidc_endpoints(config: &SsoProviderConfig) -> Result<OidcEndpoints> {
    if let Some(discovery_url) = config
        .discovery_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    {
        config
            .transport_policy
            .require_confidential_endpoint(discovery_url, "discovery")?;
        let endpoints = crate::net::outbound_client()
            .get(discovery_url)
            .send()
            .await
            .provider_call("OIDC discovery request failed")?
            .error_for_status()
            .provider_call("OIDC discovery endpoint returned an error")?
            .json::<OidcEndpoints>()
            .await
            .provider_call("failed to parse the OIDC discovery document")?;
        endpoints.require_matching_issuer(discovery_url)?;
        endpoints.require_confidential_transport(&config.transport_policy)?;
        return Ok(endpoints);
    }

    if config.slug == "google" {
        let endpoints = OidcEndpoints {
            issuer: "https://accounts.google.com".to_string(),
            authorization_endpoint: default_oidc_auth_url("google").unwrap_or_default(),
            token_endpoint: default_oidc_token_url("google").unwrap_or_default(),
            userinfo_endpoint: "https://openidconnect.googleapis.com/v1/userinfo".to_string(),
        };
        endpoints.require_confidential_transport(&config.transport_policy)?;
        return Ok(endpoints);
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
        builtin_oauth2_auth_url(&self.slug)
    }

    fn default_oauth2_token_url(&self) -> Option<String> {
        builtin_oauth2_token_url(&self.slug)
    }
}

fn builtin_oauth2_auth_url(slug: &str) -> Option<String> {
    match slug {
        "github" => Some("https://github.com/login/oauth/authorize".into()),
        "gitlab" => Some("https://gitlab.com/oauth/authorize".into()),
        _ => None,
    }
}

fn builtin_oauth2_token_url(slug: &str) -> Option<String> {
    match slug {
        "github" => Some("https://github.com/login/oauth/access_token".into()),
        "gitlab" => Some("https://gitlab.com/oauth/token".into()),
        _ => None,
    }
}

/// Can a login through this provider build an authorization request at all?
///
/// The admin API asks this before storing an **enabled** OAuth2/OIDC provider,
/// so a configuration that can never reach an IdP is a `400` while the operator
/// is still looking at the form — instead of a login that dies inside
/// [`oauth2_authorize_url`] weeks later and reads as the IdP's fault.
///
/// It lives here, next to the endpoint tables it consults, rather than as a
/// second slug list in the HTTP layer: an answer given far from the table it
/// describes is an answer that drifts away from what the login path does.
pub fn has_resolvable_endpoints(
    provider_type: &str,
    slug: &str,
    discovery_url: Option<&str>,
) -> bool {
    match provider_type {
        // Mirrors `resolve_oidc_endpoints`: the discovery document first, and
        // the built-in table only for the slugs that have one.
        "oidc" => {
            discovery_url.is_some_and(|url| !url.trim().is_empty())
                || default_oidc_auth_url(slug).is_some()
        }
        // Plain OAuth2 has no discovery step — `oauth2_authorize_url` takes the
        // endpoint from the built-in table or gives up.
        _ => builtin_oauth2_auth_url(slug).is_some(),
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
    use super::{
        fetch_user_info_with_builtin_urls, oidc_email_verified, BuiltinProfileUrls, OidcEndpoints,
        OidcTransportPolicy, SsoIdentityDefect, SsoProviderConfig, SsoUserInfo,
    };

    fn discovery_document(issuer: &str, authorization_endpoint: &str) -> OidcEndpoints {
        OidcEndpoints {
            issuer: issuer.to_string(),
            authorization_endpoint: authorization_endpoint.to_string(),
            token_endpoint: "https://idp.example/token".to_string(),
            userinfo_endpoint: "https://idp.example/userinfo".to_string(),
        }
    }

    /// Discovery §4.3: a document may only speak for the issuer it was fetched
    /// from, however valid its endpoint URLs look on their own.
    #[test]
    fn discovery_document_issuer_must_be_the_discovery_url_issuer() {
        const DISCOVERY: &str = "https://idp.example/.well-known/openid-configuration";
        let matching = discovery_document("https://idp.example", "https://idp.example/authorize");
        assert!(matching.require_matching_issuer(DISCOVERY).is_ok());
        // A trailing slash is how some implementations spell the same issuer.
        let slashed = discovery_document("https://idp.example/", "https://idp.example/authorize");
        assert!(slashed.require_matching_issuer(DISCOVERY).is_ok());

        let foreign = discovery_document("https://evil.example", "https://evil.example/authorize");
        let error = foreign
            .require_matching_issuer(DISCOVERY)
            .expect_err("a document for another issuer must be refused")
            .to_string();
        assert!(
            error.contains("evil.example"),
            "error names the claim: {error}"
        );

        // Without the well-known suffix there is no issuer to compare against,
        // so the document is refused rather than trusted.
        assert!(matching
            .require_matching_issuer("https://idp.example/tenant")
            .is_err());
    }

    /// A document that omits `issuer` cannot be attributed to anyone and must
    /// not deserialize into something usable.
    #[test]
    fn discovery_document_without_an_issuer_is_refused() {
        let error = serde_json::from_value::<OidcEndpoints>(serde_json::json!({
            "authorization_endpoint": "https://idp.example/authorize",
            "token_endpoint": "https://idp.example/token",
            "userinfo_endpoint": "https://idp.example/userinfo",
        }))
        .expect_err("a document without an issuer must not parse");
        assert!(error.to_string().contains("issuer"), "{error}");
    }

    #[test]
    fn authorization_endpoint_answers_to_the_transport_policy() {
        let insecure = discovery_document("https://idp.example", "http://idp.example/authorize");
        let secure_defaults = OidcTransportPolicy::parse(&[]).expect("empty policy");
        assert!(
            insecure
                .require_confidential_transport(&secure_defaults)
                .is_err(),
            "plaintext authorization endpoint must be refused by default"
        );

        let exception =
            OidcTransportPolicy::parse(&["http://idp.example".to_string()]).expect("exact origin");
        assert!(
            insecure.require_confidential_transport(&exception).is_ok(),
            "the operator's exact-origin exception also covers authorization"
        );
    }

    #[tokio::test]
    async fn oidc_builtin_slugs_fetch_profile_from_the_discovered_userinfo_origin() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        for slug in ["gitlab", "github", "google"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local IdP");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let discovery = format!("{origin}/.well-known/openid-configuration");
            let endpoint = format!("{origin}/userinfo");
            let allowed_origin = origin.clone();
            let issuer = origin.clone();
            let token = format!("private-{slug}-token");
            let expected_token = token.clone();
            let server = tokio::spawn(async move {
                for (path, body, bearer) in [
                    (
                        "/.well-known/openid-configuration",
                        serde_json::json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{origin}/authorize"),
                            "token_endpoint": format!("{origin}/token"),
                            "userinfo_endpoint": endpoint,
                        })
                        .to_string(),
                        false,
                    ),
                    (
                        "/userinfo",
                        serde_json::json!({
                            "sub": format!("subject-{slug}"),
                            "preferred_username": slug,
                            "email": format!("{slug}@example.test"),
                            "email_verified": true,
                        })
                        .to_string(),
                        true,
                    ),
                ] {
                    let (mut socket, _) = listener.accept().await.expect("IdP request");
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let n = socket.read(&mut chunk).await.expect("read IdP request");
                        assert!(n > 0, "request ended before headers");
                        request.extend_from_slice(&chunk[..n]);
                    }
                    let request = String::from_utf8(request).expect("ASCII HTTP request");
                    assert!(request.starts_with(&format!("GET {path} HTTP/1.1\r\n")));
                    let header = format!("authorization: Bearer {expected_token}\r\n");
                    assert_eq!(
                        request
                            .to_ascii_lowercase()
                            .contains(&header.to_ascii_lowercase()),
                        bearer
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("IdP reply");
                }
            });

            // Stand in for every built-in public profile URL. The tested
            // dispatcher receives these URLs, while OIDC must ignore them.
            let public_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind public-provider sink");
            let public_url = format!("http://{}/user", public_listener.local_addr().unwrap());
            let (stop_public, stopped_public) = tokio::sync::oneshot::channel();
            let public_sink = tokio::spawn(async move {
                tokio::select! {
                    request = public_listener.accept() => {
                        request.expect("public-provider request");
                        1
                    }
                    _ = stopped_public => 0,
                }
            });

            let config = SsoProviderConfig {
                slug: slug.to_string(),
                provider_type: "oidc".to_string(),
                client_id: "test-client".to_string(),
                client_secret: "test-secret".to_string(),
                redirect_url: "http://localhost/callback".to_string(),
                scopes: vec!["openid".to_string()],
                discovery_url: Some(discovery),
                transport_policy: OidcTransportPolicy::parse(&[allowed_origin])
                    .expect("allow the local IdP origin"),
            };
            let identity = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                fetch_user_info_with_builtin_urls(
                    &config,
                    &token,
                    &BuiltinProfileUrls {
                        github_user: &public_url,
                        github_emails: &public_url,
                        gitlab_user: &public_url,
                        google_user: &public_url,
                    },
                ),
            )
            .await
            .expect("profile must come from the local IdP")
            .expect("valid discovered profile");
            assert_eq!(identity.provider_user_id, format!("subject-{slug}"));
            server.await.expect("both IdP requests completed");
            stop_public.send(()).expect("public sink still listening");
            assert_eq!(public_sink.await.expect("public sink completed"), 0);
        }
    }

    #[test]
    fn oidc_plaintext_exceptions_are_exact_http_origins() {
        let policy = OidcTransportPolicy::parse(&["http://idp.internal:8080".to_string()])
            .expect("valid exact OIDC origin");

        policy
            .require_confidential_endpoint("http://idp.internal:8080/token", "token")
            .expect("paths on the named origin are allowed");
        policy
            .require_confidential_endpoint("https://other.example/token", "token")
            .expect("HTTPS never needs an exception");
        for neighbor in [
            "http://idp.internal:8081/token",
            "http://other.internal:8080/token",
            "http://idp.internal/token",
        ] {
            assert!(
                policy
                    .require_confidential_endpoint(neighbor, "token")
                    .is_err(),
                "the exception widened to {neighbor}"
            );
        }
    }

    #[test]
    fn oidc_plaintext_config_accepts_origins_not_urls_or_wildcards() {
        for invalid in [
            "https://idp.internal",
            "http://*.internal",
            "http://idp.internal/path",
            "http://user@idp.internal",
        ] {
            assert!(
                OidcTransportPolicy::parse(&[invalid.to_string()]).is_err(),
                "invalid plaintext OIDC exception was accepted: {invalid}"
            );
        }
    }

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

            assert_eq!(
                identity.provider_username, expected,
                "from `{provider_name}`"
            );
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

        assert_eq!(
            identity.provider_username.len(),
            super::SSO_USERNAME_BASE_MAX
        );
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

    /// GitLab has no `email_verified`; a confirmed account is its confirmation.
    #[test]
    fn gitlab_vouches_for_the_primary_only_once_the_account_is_confirmed() {
        use super::gitlab_email_verified;
        assert_eq!(
            gitlab_email_verified(&serde_json::json!({"confirmed_at": "2026-01-02T03:04:05Z"})),
            Some(true)
        );
        assert_eq!(
            gitlab_email_verified(&serde_json::json!({"confirmed_at": null})),
            None
        );
        assert_eq!(
            gitlab_email_verified(&serde_json::json!({"confirmed_at": ""})),
            None
        );
        assert_eq!(gitlab_email_verified(&serde_json::json!({})), None);
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
