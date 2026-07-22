//! Thin async client for the IronForge REST API.
//!
//! Wraps `reqwest::Client` and adds:
//! - `/api/v1/` prefix
//! - Bearer token injection
//! - Status-code → `crate::Error` conversion

use super::AppState;
use serde::de::DeserializeOwned;

pub struct ApiClient {
    inner: reqwest::Client,
    base: String,
}

impl ApiClient {
    pub fn new(state: &AppState) -> Self {
        Self {
            inner: state.http_client(),
            base: state.api_base.trim_end_matches('/').to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/{}", self.base, path.trim_start_matches('/'))
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> crate::Result<T> {
        let resp = self.inner.get(self.url(path)).send().await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(super::Error::Api { status, body });
        }
        Ok(resp.json().await?)
    }

    pub async fn get_raw(&self, path: &str) -> crate::Result<String> {
        let resp = self.inner.get(self.url(path)).send().await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(super::Error::Api { status, body });
        }
        Ok(resp.text().await?)
    }

    pub async fn get_bytes(&self, path: &str) -> crate::Result<Vec<u8>> {
        let resp = self.inner.get(self.url(path)).send().await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(super::Error::Api { status, body });
        }
        Ok(resp.bytes().await?.to_vec())
    }
}
