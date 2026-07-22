//!
//! `rg-mcp` - ForgeKeep MCP Server
//!
//! MCP (Model Context Protocol) server that exposes ForgeKeep
//! repository data as Tools and Resources to AI agents.
//!
//! Supported transport:
//! - **stdio**: run as a subprocess of an MCP-capable agent.
//!
//! HTTP SSE transport is intentionally not advertised until implemented.

pub mod client;
pub mod error;
pub mod protocol;
pub mod resources;
pub mod tools;

// Re-export for convenience
pub use error::{Error, Result};

/// Read `new` from the environment, falling back to the deprecated `old` name
/// (IronForge → ForgeKeep rebrand) with a one-time deprecation warning.
fn env_var_compat(new: &str, old: &str) -> Option<String> {
    if let Ok(value) = std::env::var(new) {
        return Some(value);
    }
    match std::env::var(old) {
        Ok(value) => {
            tracing::warn!(
                "environment variable `{old}` is deprecated and will be removed in a future \
                 release; use `{new}` instead"
            );
            Some(value)
        }
        Err(_) => None,
    }
}

/// ForgeKeep API base URL + PAT cache.
///
/// Constructed once at startup from environment variables.
#[derive(Clone)]
pub struct AppState {
    pub api_base: String,
    pub pat: String,
}

impl AppState {
    pub fn from_env() -> Result<Self> {
        let api_base = env_var_compat("FORGEKEEP_URL", "IRONFORGE_URL")
            .unwrap_or_else(|| "http://localhost:8080".to_string());
        let pat = env_var_compat("FORGEKEEP_PAT", "IRONFORGE_PAT").unwrap_or_default();

        if pat.is_empty() {
            tracing::warn!("FORGEKEEP_PAT not set – API calls may fail");
        }

        Ok(Self { api_base, pat })
    }

    /// Build a `reqwest::Client` with Bearer token header.
    pub fn http_client(&self) -> reqwest::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        if !self.pat.is_empty() {
            let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.pat))
                .unwrap_or(reqwest::header::HeaderValue::from_static(""));
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
        // unwrap is acceptable here — build() only fails if native TLS is
        // entirely unavailable, which means the system is fundamentally broken
        reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("reqwest::Client::build() failed: no native TLS backend available")
    }
}
