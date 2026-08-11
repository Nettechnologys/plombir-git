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

/// Keep the PAT on the exact origin of each initiating request. Reqwest's
/// default sensitive-header stripping compares host and port but not scheme,
/// so it is not sufficient for an HTTPS-to-HTTP redirect on the same socket.
fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    let default = reqwest::redirect::Policy::default();
    reqwest::redirect::Policy::custom(move |attempt| {
        let stays_on_origin = attempt.previous().first().is_some_and(|initial| {
            initial.scheme() == attempt.url().scheme()
                && initial.host_str() == attempt.url().host_str()
                && initial.port_or_known_default() == attempt.url().port_or_known_default()
        });
        if stays_on_origin {
            default.redirect(attempt)
        } else {
            attempt.stop()
        }
    })
}

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
        .redirect(same_origin_redirect_policy())
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

#[cfg(test)]
mod redirect_tests {
    use super::build_http_client;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_headers(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn write_response(stream: &mut TcpStream, status: &str, headers: &str) {
        let response =
            format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    enum OriginChange {
        Scheme,
        Host,
        Port,
    }

    async fn assert_origin_change_is_stopped(
        client: &reqwest::Client,
        expected_authorization: &str,
        change: OriginChange,
    ) {
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_address = source.local_addr().unwrap();
        let sink = if matches!(change, OriginChange::Port) {
            Some(TcpListener::bind("127.0.0.1:0").await.unwrap())
        } else {
            None
        };
        let sink_address = sink.as_ref().map(|listener| listener.local_addr().unwrap());
        let location = match change {
            OriginChange::Scheme => {
                format!("https://127.0.0.1:{}/changed-scheme", source_address.port())
            }
            OriginChange::Host => {
                format!("http://127.0.0.1:{}/changed-host", source_address.port())
            }
            OriginChange::Port => format!("http://{}/changed-port", sink_address.unwrap()),
        };
        let initial_host = if matches!(change, OriginChange::Host) {
            "localhost"
        } else {
            "127.0.0.1"
        };
        let initial_url = format!("http://{initial_host}:{}/start", source_address.port());

        let sink_task = sink.map(|sink| {
            tokio::spawn(async move {
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), sink.accept()).await,
                    Ok(Ok(_))
                )
            })
        });
        let source_task = tokio::spawn(async move {
            let (mut first, _) = source.accept().await.unwrap();
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: {location}\r\n"),
            )
            .await;
            let same_listener_followed = if matches!(change, OriginChange::Port) {
                false
            } else {
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), source.accept()).await,
                    Ok(Ok(_))
                )
            };
            (first_request, same_listener_followed)
        });

        let response = client
            .get(initial_url)
            .send()
            .await
            .expect("the cross-origin redirect must be returned, not followed");
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);

        let (first_request, same_listener_followed) = source_task.await.unwrap();
        assert!(
            first_request
                .to_ascii_lowercase()
                .contains(expected_authorization),
            "baseline request did not carry its credential: {first_request}"
        );
        let separate_sink_followed = match sink_task {
            Some(task) => task.await.unwrap(),
            None => false,
        };
        assert!(
            !same_listener_followed && !separate_sink_followed,
            "{change:?}-changing destination was contacted"
        );
    }

    #[tokio::test]
    async fn mcp_client_stops_every_origin_change_before_sending_the_pat() {
        let client = build_http_client("mcp-pat");
        for change in [OriginChange::Scheme, OriginChange::Host, OriginChange::Port] {
            assert_origin_change_is_stopped(&client, "authorization: bearer mcp-pat", change).await;
        }
    }

    #[tokio::test]
    async fn mcp_client_keeps_same_origin_redirects_and_the_pat() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: http://{address}/renamed\r\n"),
            )
            .await;
            let (mut second, _) = listener.accept().await.unwrap();
            let second_request = read_headers(&mut second).await;
            write_response(&mut second, "204 No Content", "").await;
            (first_request, second_request)
        });

        let response = build_http_client("mcp-pat")
            .get(format!("http://{address}/start"))
            .send()
            .await
            .expect("same-origin redirect");
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

        let (first, second) = server.await.unwrap();
        for request in [first, second] {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer mcp-pat"),
                "same-origin request lost the MCP PAT: {request}"
            );
        }
    }
}
