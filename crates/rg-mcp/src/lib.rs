//!
//! `rg-mcp` - IronForge MCP Server
//!
//! MCP (Model Context Protocol) server that exposes IronForge
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

/// IronForge API base URL + PAT cache.
///
/// Constructed once at startup from environment variables.
#[derive(Clone)]
pub struct AppState {
    pub api_base: String,
    pub pat: String,
}

impl AppState {
    pub fn from_env() -> Result<Self> {
        use std::env;

        let api_base =
            env::var("IRONFORGE_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let pat = env::var("IRONFORGE_PAT").unwrap_or_default();

        if pat.is_empty() {
            tracing::warn!("IRONFORGE_PAT not set – API calls may fail");
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
