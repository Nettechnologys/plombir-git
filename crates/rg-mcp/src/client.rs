//! Thin async client for the ForgeKeep REST API.
//!
//! Wraps `reqwest::Client` and adds:
//! - `/api/v1/` prefix
//! - Bearer token injection
//! - Status-code → `crate::Error` conversion

use super::AppState;
use serde::de::DeserializeOwned;
use serde_json::Value;

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

    /// Send a write request (POST/PATCH/PUT/DELETE) with an optional JSON body
    /// and return the response as text. Non-2xx responses become `Error::Api`,
    /// mirroring the read helpers so tool handlers can surface backend errors.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> crate::Result<String> {
        let mut builder = self.inner.request(method, self.url(path));
        if let Some(json) = body {
            builder = builder.json(json);
        }
        let resp = builder.send().await?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(super::Error::Api { status, body });
        }
        Ok(resp.text().await?)
    }

    /// POST with a JSON body.
    pub async fn post_raw(&self, path: &str, body: &Value) -> crate::Result<String> {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    /// POST with no body (action endpoints like pipeline retry/cancel).
    pub async fn post_empty(&self, path: &str) -> crate::Result<String> {
        self.send(reqwest::Method::POST, path, None).await
    }

    /// PATCH with a JSON body.
    pub async fn patch_raw(&self, path: &str, body: &Value) -> crate::Result<String> {
        self.send(reqwest::Method::PATCH, path, Some(body)).await
    }
}
