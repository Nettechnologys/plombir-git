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

/// Request timeout for MCP → ForgeKeep API calls (whole request, incl. body),
/// so a slow/hanging server can't pin a tool call forever.
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Connect timeout (TCP + TLS handshake only).
const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Build the `reqwest::Client` used for every ForgeKeep API call: the static
/// `Bearer` header plus the outbound request + connect timeouts.
///
/// Built once and cached on [`AppState`] — cloning a `reqwest::Client` is a
/// cheap `Arc` bump that shares one connection pool + TLS config, so tool calls
/// reuse keep-alive connections instead of standing up a fresh pool each time.
fn build_http_client(pat: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    if !pat.is_empty() {
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {pat}"))
            .unwrap_or(reqwest::header::HeaderValue::from_static(""));
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    // unwrap is acceptable here — build() only fails if native TLS is
    // entirely unavailable, which means the system is fundamentally broken
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .default_headers(headers)
        .build()
        .expect("reqwest::Client::build() failed: no native TLS backend available")
}

/// ForgeKeep API base URL + PAT cache.
///
/// Constructed once at startup from environment variables.
#[derive(Clone)]
pub struct AppState {
    pub api_base: String,
    pub pat: String,
    /// Pre-built, reusable API client (Bearer header + timeouts baked in).
    http_client: reqwest::Client,
}

impl AppState {
    pub fn from_env() -> Result<Self> {
        let api_base =
            std::env::var("FORGEKEEP_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let pat = std::env::var("FORGEKEEP_PAT").unwrap_or_default();

        if pat.is_empty() {
            tracing::warn!("FORGEKEEP_PAT not set – API calls may fail");
        }

        Ok(Self::new(api_base, pat))
    }

    /// Construct from an explicit base URL + PAT, building the cached HTTP
    /// client once. The `Bearer` header depends only on `pat`, which is fixed
    /// for the lifetime of an `AppState`, so the client never needs rebuilding.
    pub fn new(api_base: String, pat: String) -> Self {
        let http_client = build_http_client(&pat);
        Self {
            api_base,
            pat,
            http_client,
        }
    }

    /// Cheap clone of the shared `reqwest::Client` (Bearer header + timeouts).
    pub fn http_client(&self) -> reqwest::Client {
        self.http_client.clone()
    }
}
