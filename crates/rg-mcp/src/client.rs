//! Thin async client for the ForgeKeep REST API.
//!
//! Adds to whichever [`crate::Backend`] the state carries:
//! - the `/api/v1/` prefix
//! - Bearer token injection (HTTP backend; the in-process one authenticates
//!   itself)
//! - Status-code → `crate::Error` conversion

use super::{AppState, Backend};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub struct ApiClient {
    backend: Backend,
}

impl ApiClient {
    pub fn new(state: &AppState) -> Self {
        Self {
            backend: state.backend.clone(),
        }
    }

    fn api_path(path: &str) -> String {
        format!("/api/v1/{}", path.trim_start_matches('/'))
    }

    /// One exchange: the status and the whole body, successful or not.
    async fn exchange(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> crate::Result<(u16, Vec<u8>)> {
        match &self.backend {
            Backend::Http { api_base, client } => {
                let url = format!("{}{}", api_base.trim_end_matches('/'), Self::api_path(path));
                let mut builder = client.request(method, url);
                if let Some(json) = body {
                    builder = builder.json(json);
                }
                let resp = builder.send().await?;
                let status = resp.status().as_u16();
                Ok((status, resp.bytes().await?.to_vec()))
            }
            Backend::InProcess(transport) => {
                let response = transport
                    .exchange(method, Self::api_path(path), body.cloned())
                    .await?;
                Ok((response.status, response.body))
            }
        }
    }

    /// [`exchange`](Self::exchange), with a non-2xx answer turned into
    /// `Error::Api` so tool handlers can surface backend errors.
    async fn success(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> crate::Result<Vec<u8>> {
        let (status, bytes) = self.exchange(method, path, body).await?;
        if !(200..300).contains(&status) {
            return Err(super::Error::Api {
                status,
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        Ok(bytes)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> crate::Result<T> {
        let bytes = self.success(reqwest::Method::GET, path, None).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub async fn get_raw(&self, path: &str) -> crate::Result<String> {
        let bytes = self.success(reqwest::Method::GET, path, None).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn get_bytes(&self, path: &str) -> crate::Result<Vec<u8>> {
        self.success(reqwest::Method::GET, path, None).await
    }

    /// Send a write request (POST/PATCH/PUT/DELETE) with an optional JSON body
    /// and return the response as text.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> crate::Result<String> {
        let bytes = self.success(method, path, body).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
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
