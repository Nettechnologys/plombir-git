//! OAuth2 / OIDC SSO service. Supports GitHub, GitLab, Google, and custom providers.
//!
//! # Security features
//! - PKCE (S256) for all OAuth2 providers (RFC 7636)
//! - CSRF state tied to a signed cookie
//! - Token refresh support
//! - OIDC Discovery for Google and generic OIDC providers

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
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
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

    let client = reqwest::Client::new();

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

    let client = reqwest::Client::new();

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
pub async fn oauth2_fetch_user_info(
    config: &SsoProviderConfig,
    access_token: &str,
) -> Result<SsoUserInfo> {
    match config.slug.as_str() {
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
    }
}

// ── GitHub user info ─────────────────────────────────────────────

async fn fetch_github_user(access_token: &str) -> Result<SsoUserInfo> {
    let client = reqwest::Client::new();

    let user_resp = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "IronForge/0.1")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("GitHub user API request failed")?;

    let user: serde_json::Value = user_resp
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
    let email = fetch_github_email(&client, access_token)
        .await
        .unwrap_or_else(|| user["email"].as_str().unwrap_or("").to_string());

    Ok(SsoUserInfo {
        provider_user_id,
        provider_username,
        email,
        display_name,
        avatar_url,
    })
}

async fn fetch_github_email(client: &reqwest::Client, access_token: &str) -> Option<String> {
    let resp = client
        .get("https://api.github.com/user/emails")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "IronForge/0.1")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .ok()?;

    let emails: Vec<serde_json::Value> = resp.json().await.ok()?;

    for email in &emails {
        let primary = email["primary"].as_bool().unwrap_or(false);
        let verified = email["verified"].as_bool().unwrap_or(false);
        if primary && verified {
            if let Some(e) = email["email"].as_str() {
                return Some(e.to_string());
            }
        }
    }

    emails
        .iter()
        .find(|e| e["verified"].as_bool().unwrap_or(false))
        .and_then(|e| e["email"].as_str())
        .map(str::to_string)
}

// ── GitLab user info ─────────────────────────────────────────────

async fn fetch_gitlab_user(access_token: &str) -> Result<SsoUserInfo> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://gitlab.com/api/v4/user")
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .context("GitLab user API request failed")?;

    let user: serde_json::Value = resp
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
        display_name: user["name"].as_str().map(str::to_string),
        avatar_url: user["avatar_url"].as_str().map(str::to_string),
    })
}

// ── Google OIDC user info ────────────────────────────────────────

async fn fetch_google_user(access_token: &str) -> Result<SsoUserInfo> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .context("Google userinfo API request failed")?;

    let user: serde_json::Value = resp
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
        display_name: user["name"].as_str().map(str::to_string),
        avatar_url: user["picture"].as_str().map(str::to_string),
    })
}

// ── Generic OIDC userinfo ────────────────────────────────────────

async fn fetch_oidc_userinfo(
    config: &SsoProviderConfig,
    access_token: &str,
) -> Result<SsoUserInfo> {
    let client = reqwest::Client::new();
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
        return reqwest::Client::new()
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
