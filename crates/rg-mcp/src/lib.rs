//!
//! `rg-mcp` - ForgeKeep MCP Server
//!
//! MCP (Model Context Protocol) server that exposes ForgeKeep
//! repository data as Tools and Resources to AI agents.
//!
//! Supported transports:
//! - **stdio**: run as a subprocess of an MCP-capable agent (`forgekeep-mcp`).
//! - **HTTP**: the ForgeKeep server embeds this crate and answers
//!   `POST /api/v1/mcp` itself, calling its own API in-process through an
//!   [`ApiTransport`] — so an agent needs no local binary, and the server knows
//!   which tool every API call serves.

pub mod client;
pub mod error;
pub mod protocol;
pub mod resources;
pub mod tools;

// Re-export for convenience
pub use error::{Error, Result};

/// The ForgeKeep API this server talks to when `FORGEKEEP_URL` is unset.
///
/// A named constant rather than a literal inside the resolve, because the same
/// address is restated on both pages that describe this binary — the `//!`
/// table of `main.rs` and the README's MCP section — and a value with no name
/// is one no check can bind them to. The tests at the bottom of this file are
/// what hold the three together.
const DEFAULT_API_BASE: &str = "http://localhost:8080";
/// Remote plaintext HTTP is a credential disclosure risk and therefore off
/// unless the operator explicitly accepts it for this one configured server.
const DEFAULT_ALLOW_INSECURE_HTTP: bool = false;

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

// `rg-mcp` deliberately stays independent of the server-side `rg-core` crate,
// so it carries the small DNS-free loopback predicate locally.
fn is_loopback_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    let literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    literal
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => ip.is_loopback(),
            std::net::IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip
                        .to_ipv4_mapped()
                        .is_some_and(|mapped| mapped.is_loopback())
            }
        })
}

fn require_confidential_bearer_server(
    api_base: &str,
    pat: &str,
    allow_insecure_http: bool,
) -> Result<()> {
    let url = reqwest::Url::parse(api_base).map_err(|error| {
        Error::Config(format!(
            "FORGEKEEP_URL must be an absolute http(s) URL: {error}"
        ))
    })?;
    if url.host_str().is_none() {
        return Err(Error::Config(
            "FORGEKEEP_URL must name a server host".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Config(
            "FORGEKEEP_URL must not contain user information".to_string(),
        ));
    }

    match url.scheme() {
        "https" => Ok(()),
        // An unauthenticated MCP can still read public data over HTTP. The
        // confidentiality boundary starts when the cached client carries PAT.
        "http" if pat.is_empty() || is_loopback_host(&url) => Ok(()),
        "http" if allow_insecure_http => {
            tracing::warn!(
                "FORGEKEEP_ALLOW_INSECURE_HTTP=true: the MCP PAT may cross plaintext HTTP"
            );
            Ok(())
        }
        "http" => Err(Error::Config(
            "FORGEKEEP_URL uses plaintext HTTP for a non-loopback server while FORGEKEEP_PAT is set; use HTTPS, keep local development on localhost/loopback, or explicitly set FORGEKEEP_ALLOW_INSECURE_HTTP=true"
                .to_string(),
        )),
        scheme => Err(Error::Config(format!(
            "FORGEKEEP_URL must use http or https, not {scheme}"
        ))),
    }
}

fn parse_allow_insecure_http(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::Config(
            "FORGEKEEP_ALLOW_INSECURE_HTTP must be `true` or `false`".to_string(),
        )),
    }
}

/// The HTTP method an [`ApiTransport`] is asked to send — re-exported so an
/// embedding server implements the trait without depending on `reqwest`.
pub use reqwest::Method;

/// One exchange with the ForgeKeep REST API, before any decoding.
pub struct ApiResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A future an [`ApiTransport`] answers with.
pub type ApiFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<ApiResponse>> + Send + 'a>>;

/// How tool calls reach the ForgeKeep REST API when they do not go over the
/// network — the server hosting this crate's tools behind its own MCP endpoint.
///
/// `path` is the full API path including `/api/v1` and any query string. The
/// implementation owns authentication: the tools never see a credential.
pub trait ApiTransport: Send + Sync {
    fn exchange(
        &self,
        method: reqwest::Method,
        path: String,
        body: Option<serde_json::Value>,
    ) -> ApiFuture<'_>;
}

/// Where tool calls are sent.
#[derive(Clone)]
pub(crate) enum Backend {
    /// A ForgeKeep server over HTTP(S) — the stdio binary's only mode.
    Http {
        api_base: String,
        /// Pre-built, reusable API client (Bearer header + timeouts baked in).
        client: reqwest::Client,
    },
    /// The server this crate is embedded in, without a network hop.
    InProcess(std::sync::Arc<dyn ApiTransport>),
}

/// ForgeKeep API base URL + PAT cache, or an in-process transport.
///
/// The stdio binary constructs it once at startup from environment variables;
/// the server's own MCP endpoint constructs one per request around the
/// caller's credential with [`AppState::in_process`].
#[derive(Clone)]
pub struct AppState {
    pub(crate) backend: Backend,
}

impl AppState {
    pub fn from_env() -> Result<Self> {
        let api_base =
            std::env::var("FORGEKEEP_URL").unwrap_or_else(|_| DEFAULT_API_BASE.to_string());
        let pat = std::env::var("FORGEKEEP_PAT").unwrap_or_default();
        let allow_insecure_http = std::env::var("FORGEKEEP_ALLOW_INSECURE_HTTP")
            .unwrap_or_else(|_| DEFAULT_ALLOW_INSECURE_HTTP.to_string());
        let allow_insecure_http = parse_allow_insecure_http(&allow_insecure_http)?;

        if pat.is_empty() {
            tracing::warn!("FORGEKEEP_PAT not set – API calls may fail");
        }

        Self::try_new_with_transport_policy(api_base, pat, allow_insecure_http)
    }

    /// Construct from an explicit base URL + PAT, building the cached HTTP
    /// client once. The `Bearer` header depends only on `pat`, which is fixed
    /// for the lifetime of an `AppState`, so the client never needs rebuilding.
    pub fn new(api_base: String, pat: String) -> Self {
        Self::try_new(api_base, pat)
            .expect("AppState server URL must satisfy the default transport policy")
    }

    /// Fallible secure-default constructor for embeddings that want to surface
    /// configuration errors instead of panicking.
    pub fn try_new(api_base: String, pat: String) -> Result<Self> {
        Self::try_new_with_transport_policy(api_base, pat, false)
    }

    /// Serve the tools through `transport` instead of over the network.
    pub fn in_process(transport: std::sync::Arc<dyn ApiTransport>) -> Self {
        Self {
            backend: Backend::InProcess(transport),
        }
    }

    fn try_new_with_transport_policy(
        api_base: String,
        pat: String,
        allow_insecure_http: bool,
    ) -> Result<Self> {
        require_confidential_bearer_server(&api_base, &pat, allow_insecure_http)?;
        let client = build_http_client(&pat);
        Ok(Self {
            backend: Backend::Http { api_base, client },
        })
    }

    /// Cheap clone of the shared `reqwest::Client` (Bearer header + timeouts),
    /// when this state talks HTTP.
    #[cfg(test)]
    fn http_client(&self) -> reqwest::Client {
        match &self.backend {
            Backend::Http { client, .. } => client.clone(),
            Backend::InProcess(_) => panic!("an in-process state has no HTTP client"),
        }
    }
}

/// MCP protocol revisions this server speaks, oldest first.
///
/// The server's answer to `initialize` is the client's requested revision when
/// it is one of these, and the newest otherwise — the negotiation the
/// specification describes. Nothing here differs between the revisions for the
/// subset implemented (tools, resources, no sampling or elicitation).
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// Answer one JSON-RPC message, the same way over every transport.
///
/// `None` for a notification: JSON-RPC forbids answering one, and the stdio
/// loop used to reply to `notifications/initialized` with a result carrying a
/// `null` id, which a strict client reads as a protocol violation.
pub fn dispatch(
    state: &AppState,
    req: &protocol::JsonRpcRequest,
) -> Option<protocol::JsonRpcResponse> {
    if req.is_notification() {
        return None;
    }
    Some(match req.method.as_str() {
        "initialize" => handle_initialize(req),
        "ping" => protocol::make_success(req.id.clone(), serde_json::json!({})),
        "tools/list" => tools::list_tools(state, req),
        "tools/call" => tools::call_tool(state, req),
        "resources/list" => resources::list_resources(state, req),
        "resources/read" => resources::read_resource(state, req),
        _ => protocol::make_error(
            req.id.clone(),
            -32601,
            &format!("method not found: {}", req.method),
        ),
    })
}

fn handle_initialize(req: &protocol::JsonRpcRequest) -> protocol::JsonRpcResponse {
    let requested = req
        .params
        .as_ref()
        .and_then(|params| params.get("protocolVersion"))
        .and_then(|version| version.as_str());
    let version = requested
        .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[SUPPORTED_PROTOCOL_VERSIONS.len() - 1]);
    let result = serde_json::json!({
        "protocolVersion": version,
        "serverInfo": {
            "name": "forgekeep-mcp",
            "version": env!("CARGO_PKG_VERSION")
        },
        "capabilities": {
            "tools": { "listChanged": true },
            "resources": { "subscribe": false, "listChanged": true }
        }
    });
    protocol::make_success(req.id.clone(), result)
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use protocol::JsonRpcRequest;

    fn request(raw: serde_json::Value) -> JsonRpcRequest {
        serde_json::from_value(raw).expect("a well-formed JSON-RPC message")
    }

    fn state() -> AppState {
        AppState::new("http://localhost:8080".into(), String::new())
    }

    #[test]
    fn a_notification_is_never_answered() {
        let initialized = request(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
        assert!(initialized.is_notification());
        assert!(dispatch(&state(), &initialized).is_none());
    }

    #[test]
    fn initialize_negotiates_the_protocol_revision() {
        for (asked, answered) in [
            ("2024-11-05", "2024-11-05"),
            ("2025-03-26", "2025-03-26"),
            ("1999-01-01", "2025-06-18"),
        ] {
            let response = dispatch(
                &state(),
                &request(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": { "protocolVersion": asked }
                })),
            )
            .expect("a request is answered");
            assert_eq!(
                response.result.unwrap()["protocolVersion"],
                answered,
                "asked for {asked}"
            );
        }
    }
}

#[cfg(test)]
mod redirect_tests {
    use super::{build_http_client, parse_allow_insecure_http, AppState};
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

    fn local_non_loopback_ipv4() -> std::net::Ipv4Addr {
        let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        probe.connect("192.0.2.1:9").unwrap();
        let std::net::IpAddr::V4(ip) = probe.local_addr().unwrap().ip() else {
            panic!("the test host has no routable IPv4 address");
        };
        assert!(!ip.is_loopback(), "test address must exercise remote HTTP");
        ip
    }

    #[test]
    fn insecure_http_environment_switch_is_strict() {
        assert!(parse_allow_insecure_http("true").unwrap());
        assert!(!parse_allow_insecure_http("FALSE").unwrap());
        assert!(parse_allow_insecure_http("").is_err());
        assert!(parse_allow_insecure_http("yes").is_err());
    }

    #[test]
    fn local_plaintext_and_remote_https_need_no_exception() {
        for base in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "https://forge.example.com",
        ] {
            AppState::try_new(base.to_string(), "mcp-pat".to_string())
                .unwrap_or_else(|error| panic!("{base} should be accepted: {error}"));
        }
    }

    #[tokio::test]
    async fn remote_plaintext_is_refused_before_the_mcp_pat_reaches_a_live_sink() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let address = format!(
            "http://{}:{}",
            local_non_loopback_ipv4(),
            listener.local_addr().unwrap().port()
        );
        let sink = tokio::spawn(async move {
            let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept())
                    .await
            else {
                return None;
            };
            let request = read_headers(&mut stream).await;
            write_response(&mut stream, "204 No Content", "").await;
            Some(request)
        });

        let state = AppState::try_new(address.clone(), "mcp-pat".to_string());
        if let Ok(state) = &state {
            state
                .http_client()
                .get(&address)
                .send()
                .await
                .expect("mutation baseline should reach the live sink");
        }
        let error = match state {
            Err(error) => error,
            Ok(_) => panic!("remote HTTP with a PAT must fail closed"),
        };
        assert!(error.to_string().contains("FORGEKEEP_ALLOW_INSECURE_HTTP"));
        assert!(
            sink.await.unwrap().is_none(),
            "the refused MCP origin received a request"
        );
    }

    #[tokio::test]
    async fn explicit_remote_plaintext_opt_in_allows_only_the_configured_origin() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let address = format!(
            "http://{}:{}",
            local_non_loopback_ipv4(),
            listener.local_addr().unwrap().port()
        );
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_headers(&mut stream).await;
            write_response(&mut stream, "204 No Content", "").await;
            request
        });

        let state =
            AppState::try_new_with_transport_policy(address.clone(), "mcp-pat".to_string(), true)
                .unwrap();
        let response = state.http_client().get(address).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        let request = server.await.unwrap().to_ascii_lowercase();
        assert!(request.contains("authorization: bearer mcp-pat"));
    }
}

// ---------------------------------------------------------------------------
// The environment against the pages that describe it.
//
// `forgekeep-mcp` takes no flags at all — `--help` states nothing, and the
// whole configuration is three environment variables. So the only description of
// them is prose, and it exists twice: the `//!` table of `main.rs`, and the
// README section whoever wires this binary into an agent reads while writing
// the `mcpServers` block.
//
// The *names* on those pages are already policed, by the workspace-wide census
// in `rg-cli` — a variable the code reads that no operator document names fails
// there. What that census never looks at is the column beside the name. Until
// `DEFAULT_API_BASE` existed there was nothing it could have looked at either:
// the address was a literal inside `unwrap_or_else`, so the two pages restated
// a value that had no name, and the only thing holding all three equal was that
// nobody had changed one of them yet.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod documented_environment_tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    const CRATE_DOC_PATH: &str = "crates/rg-mcp/src/main.rs";

    /// This crate's own doc-comment table.
    ///
    /// Read as prose rather than through its items: `main.rs` is compiled into
    /// the `forgekeep-mcp` *binary* and this file into the library, so the two
    /// never see each other — and it is the prose that has to be checked
    /// anyway. `include_str!` makes a moved file break the build instead of
    /// quietly skipping the checks.
    ///
    /// Prose, but still through a named view. `production_rust_code_with_doc_comments`
    /// keeps `//!` and blanks ordinary comments, every literal and complete
    /// `#[cfg(test)]` items — so a second `//! # Environment` written in a
    /// fixture, or the heading quoted inside a string, cannot decide where the
    /// section this check reads begins. `rg-cli` reads its `--help` prose out
    /// of `cli.rs` through the same view for the same reason.
    fn crate_doc_source() -> String {
        rust_source::production_rust_code_with_doc_comments(include_str!("main.rs"))
    }

    const PRODUCTION_LIB_PATH: &str = "crates/rg-mcp/src/lib.rs";

    /// The page an operator reaches for before running anything.
    const README: (&str, &str) = ("README.md", include_str!("../../../README.md"));

    /// The heading that opens the crate doc's environment table.
    const CRATE_DOC_HEADING: &str = "//! # Environment";

    /// The heading that opens the README's half of this binary. Everything up
    /// to the next `## ` heading is the section these checks read.
    const MCP_SECTION: &str = "## MCP server (`forgekeep-mcp`)";

    /// How both pages spell "this variable has no default at all".
    const NO_DEFAULT: &str = "_(none)_";

    /// The body of the page that follows `heading`, up to the next `## `.
    fn doc_section<'a>((name, content): (&str, &'a str), heading: &str) -> &'a str {
        let (_, rest) = content.split_once(heading).unwrap_or_else(|| {
            panic!(
                "{name} no longer has a `{heading}` section — that section is one of only two \
                 descriptions of this binary's environment, and the other cannot be checked \
                 against a page that is gone"
            )
        });

        match rest.split_once("\n## ") {
            Some((section, _)) => section,
            None => rest,
        }
    }

    /// Both pages, each cut down to the fragment carrying its table.
    ///
    /// Owned rather than `&'static`, because the crate doc reaches this through
    /// a production view built at run time rather than as a `const`.
    fn documented_pages() -> Vec<(&'static str, String)> {
        let crate_doc = crate_doc_source();
        vec![
            (
                CRATE_DOC_PATH,
                doc_section((CRATE_DOC_PATH, &crate_doc), CRATE_DOC_HEADING).to_owned(),
            ),
            (README.0, doc_section(README, MCP_SECTION).to_owned()),
        ]
    }

    /// The row of a `| Variable | Default | … |` table whose first cell names
    /// `variable`, with the `//!` of a doc-comment table stripped.
    ///
    /// The first cell has to *equal* the name: `FORGEKEEP_URL` must not be
    /// answered by a row describing `FORGEKEEP_URL_FILE`.
    fn table_row<'a>(section: &'a str, variable: &str) -> Option<&'a str> {
        let named = format!("`{variable}`");

        section
            .lines()
            .map(|line| line.trim_start().trim_start_matches("//!").trim_start())
            .find(|line| {
                line.strip_prefix('|')
                    .and_then(|row| row.split('|').next())
                    .is_some_and(|first| first.trim() == named)
            })
    }

    /// What a row states in its `Default` column.
    #[derive(Debug, PartialEq, Eq)]
    enum Stated<'a> {
        /// A literal value, spelled in backticks.
        Value(&'a str),
        /// The column says the variable has no default.
        Absent,
        /// Prose: neither a backticked value nor the "no default" spelling.
        /// Kept apart from the two above so a sentence cannot pass for either.
        Prose(&'a str),
    }

    /// The `Default` column of `row`.
    fn stated_default(row: &str) -> Option<Stated<'_>> {
        let cell = row.strip_prefix('|')?.split('|').nth(1)?.trim();
        if cell == NO_DEFAULT {
            return Some(Stated::Absent);
        }

        match cell
            .split_once('`')
            .and_then(|(_, rest)| rest.split_once('`'))
        {
            Some((value, _)) => Some(Stated::Value(value)),
            None => Some(Stated::Prose(cell)),
        }
    }

    /// What the production resolve falls back to when a variable is unset.
    #[derive(Debug, PartialEq, Eq)]
    enum Fallback<'a> {
        /// `unwrap_or_else(|_| NAME.to_string())` — a named constant, the only
        /// shape a page can be bound to.
        Constant(&'a str),
        /// `unwrap_or_default()` — the empty string, i.e. no default.
        Empty,
        /// `unwrap_or_else(|_| "…".to_string())` — the value written into the
        /// resolve itself. This is the shape this whole module exists to stop
        /// coming back.
        Literal(&'a str),
        /// Anything else, carrying the rest of the statement so the failure
        /// names the shape: read it before believing it.
        Unknown(&'a str),
    }

    /// A fallback together with the source line of the `env::var` call that
    /// produced it.  The line remains meaningful because the lexical view is
    /// byte- and newline-aligned with the original source.
    #[derive(Debug, PartialEq, Eq)]
    struct LocatedFallback<'a> {
        line: usize,
        fallback: Fallback<'a>,
    }

    fn matching_close_paren(code: &str, open_paren: usize) -> Option<usize> {
        let bytes = code.as_bytes();
        if bytes.get(open_paren) != Some(&b'(') {
            return None;
        }

        let mut depth = 1usize;
        for (relative, byte) in bytes[open_paren + 1..].iter().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(open_paren + relative + 1);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn statement_end(code: &str, start: usize) -> Option<usize> {
        let mut parens = 0usize;
        let mut brackets = 0usize;
        let mut braces = 0usize;

        for (relative, byte) in code.as_bytes()[start..].iter().enumerate() {
            match byte {
                b'(' => parens += 1,
                b')' => parens = parens.saturating_sub(1),
                b'[' => brackets += 1,
                b']' => brackets = brackets.saturating_sub(1),
                b'{' => braces += 1,
                b'}' => braces = braces.saturating_sub(1),
                b';' if parens == 0 && brackets == 0 && braces == 0 => {
                    return Some(start + relative);
                }
                _ => {}
            }
        }
        None
    }

    fn skip_code_whitespace(code: &str, mut at: usize, end: usize) -> usize {
        while at < end {
            let Some(ch) = code[at..end].chars().next() else {
                break;
            };
            if !ch.is_whitespace() {
                break;
            }
            at += ch.len_utf8();
        }
        at
    }

    /// The direct method call that consumes the `env::var` result.  Requiring
    /// the method call to occupy the rest of the statement keeps a nested
    /// `unwrap_or_*` from being mistaken for the fallback being classified.
    fn direct_method_call(
        code: &str,
        start: usize,
        end: usize,
        method: &str,
    ) -> Option<(usize, usize)> {
        let mut at = skip_code_whitespace(code, start, end);
        if code.as_bytes().get(at) != Some(&b'.') {
            return None;
        }
        at += 1;

        if !code.get(at..end)?.starts_with(method) {
            return None;
        }
        at += method.len();
        if code[at..end]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        {
            return None;
        }

        at = skip_code_whitespace(code, at, end);
        if code.as_bytes().get(at) != Some(&b'(') {
            return None;
        }
        let close = matching_close_paren(code, at)?;
        (close < end && code[close + 1..end].trim().is_empty()).then_some((at, close))
    }

    fn normal_string_contents(source: &str, quote: usize) -> Option<(&str, usize)> {
        let bytes = source.as_bytes();
        if bytes.get(quote) != Some(&b'"') {
            return None;
        }

        let mut at = quote + 1;
        while at < bytes.len() {
            match bytes[at] {
                b'\\' => at = (at + 2).min(bytes.len()),
                b'"' => return Some((&source[quote + 1..at], at + 1)),
                _ => at += 1,
            }
        }
        None
    }

    fn unwrap_or_else_fallback<'a>(
        source: &'a str,
        code: &str,
        open: usize,
        close: usize,
    ) -> Fallback<'a> {
        let mut expression_start = skip_code_whitespace(code, open + 1, close);
        if !code[expression_start..close].starts_with("|_|") {
            return Fallback::Unknown(source[open + 1..close].trim());
        }
        expression_start += "|_|".len();
        expression_start = rust_source::skip_whitespace_and_comments(source, expression_start)
            .filter(|start| *start < close)
            .unwrap_or(close);

        let expression_end = expression_start + code[expression_start..close].trim_end().len();
        let expression = source[expression_start..expression_end].trim();

        if let Some((literal, literal_end)) = normal_string_contents(source, expression_start) {
            if code[literal_end..expression_end].trim() == ".to_string()" {
                return Fallback::Literal(literal);
            }
        }

        let identifier_end = code[expression_start..expression_end]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .map_or(expression_end, |relative| expression_start + relative);
        if identifier_end > expression_start
            && code[identifier_end..expression_end].trim() == ".to_string()"
        {
            return Fallback::Constant(&source[expression_start..identifier_end]);
        }

        Fallback::Unknown(expression)
    }

    /// How `source` resolves `variable`, if it resolves it at all.
    fn env_fallback<'a>(source: &'a str, variable: &str) -> Option<LocatedFallback<'a>> {
        let code = rust_source::rust_code_only(source);
        let call = rust_source::call_sites(source, &["env::var"])
            .into_iter()
            .find(|call| {
                rust_source::first_string_argument(source, *call).as_deref() == Some(variable)
            })?;

        let unknown_line = || LocatedFallback {
            line: call.line,
            fallback: Fallback::Unknown(rust_source::source_line(source, call.line).trim()),
        };
        let Some(call_close) = matching_close_paren(&code, call.open_paren) else {
            return Some(unknown_line());
        };
        let Some(end) = statement_end(&code, call_close + 1) else {
            return Some(unknown_line());
        };
        let statement = source[call_close + 1..end].trim();

        let fallback = if let Some((open, close)) =
            direct_method_call(&code, call_close + 1, end, "unwrap_or_default")
        {
            if code[open + 1..close].trim().is_empty() {
                Fallback::Empty
            } else {
                Fallback::Unknown(statement)
            }
        } else if let Some((open, close)) =
            direct_method_call(&code, call_close + 1, end, "unwrap_or_else")
        {
            unwrap_or_else_fallback(source, &code, open, close)
        } else {
            Fallback::Unknown(statement)
        };

        Some(LocatedFallback {
            line: call.line,
            fallback,
        })
    }

    /// A variable both pages describe, bound to what the resolve really does.
    struct DocumentedVariable {
        /// The variable an operator sets.
        name: &'static str,
        /// The `DEFAULT_*` constant the resolve must fall back to, with its
        /// value read *from* the constant rather than copied beside it — so
        /// renaming it breaks the build and changing it fails every check
        /// below. `None` states that the variable deliberately has no default.
        default: Option<(&'static str, String)>,
    }

    /// The pairing table. The variable spellings have to be written out — no
    /// rule derives `DEFAULT_API_BASE` from `FORGEKEEP_URL` — but no value is.
    fn documented_variables() -> Vec<DocumentedVariable> {
        macro_rules! from_constant {
            ($konst:ident) => {
                Some((stringify!($konst), super::$konst.to_string()))
            };
        }

        vec![
            DocumentedVariable {
                name: "FORGEKEEP_URL",
                default: from_constant!(DEFAULT_API_BASE),
            },
            DocumentedVariable {
                name: "FORGEKEEP_PAT",
                default: None,
            },
            DocumentedVariable {
                name: "FORGEKEEP_ALLOW_INSECURE_HTTP",
                default: from_constant!(DEFAULT_ALLOW_INSECURE_HTTP),
            },
        ]
    }

    /// Built-in defaults of this crate that no operator page states, each with
    /// the reason. Empty today; the list exists so the next unpaired default is
    /// a decision someone wrote down rather than one that slipped past the
    /// census below.
    const DEFAULTS_NOT_OPERATOR_FACING: [(&str, &str); 0] = [];

    /// The `const DEFAULT_*` names `source` declares, whatever their
    /// visibility: a default that is private today is still a default an
    /// operator meets. Read off the declarations rather than listed beside
    /// them — a constant added to the crate joins the census by existing.
    fn declared_default_constants(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .map(str::trim_start)
            .map(|line| line.strip_prefix("pub(crate) ").unwrap_or(line))
            .map(|line| line.strip_prefix("pub ").unwrap_or(line))
            .filter_map(|line| line.strip_prefix("const "))
            .filter_map(|rest| rest.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("DEFAULT_"))
            .collect()
    }

    /// Every production `.rs` file of this crate, with complete `#[cfg(test)]`
    /// items blanked.
    ///
    /// A directory walk rather than a list of `include_str!`s: the question the
    /// census asks is whether a default exists *anywhere* in the crate, and a
    /// fixed list would have to be edited whenever one moves — which is the
    /// remembering these checks exist to remove. The `#[cfg(test)]` cut is what
    /// keeps it honest: a constant declared in a fixture is not a default any
    /// operator can meet.
    fn production_sources() -> Vec<(PathBuf, String)> {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        let mut pending = vec![src];

        while let Some(dir) = pending.pop() {
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));

            for entry in entries {
                let path = entry.expect("a readable directory entry").path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();

                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if !name.ends_with(".rs") {
                    continue;
                }

                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                let production = rust_source::production_rust_code_only(&text);
                sources.push((path, production));
            }
        }

        sources
    }

    /// The production view of this file — where the environment is resolved.
    fn production_lib_source() -> String {
        let production = rust_source::production_rust_source(include_str!("lib.rs"));

        assert!(
            production.contains("pub fn from_env()"),
            "the production view no longer contains `from_env`, so the checks below would \
             read no resolve at all"
        );
        production
    }

    /// The address an operator's agent talks to when the environment says
    /// nothing. Both pages restate it, and neither is generated: what they
    /// state has to be asserted against the constant the binary really uses.
    #[test]
    fn every_default_the_mcp_pages_state_is_the_one_the_binary_falls_back_to() {
        // The readers have to be able to answer "no" before their "yes" is
        // worth anything.
        assert_eq!(
            table_row(
                "//! | `FORGEKEEP_URL` | `http://probe` | base |",
                "FORGEKEEP_URL"
            )
            .and_then(stated_default),
            Some(Stated::Value("http://probe")),
            "the row reader does not see through the doc-comment table's `//!` prefix"
        );
        assert!(
            table_row(
                "| `FORGEKEEP_URL_FILE` | `http://probe` | base |",
                "FORGEKEEP_URL"
            )
            .is_none(),
            "the row reader answers `FORGEKEEP_URL` with a longer name's row, so a page \
             documenting neither could pass for one documenting both"
        );
        assert_eq!(
            table_row("| `FORGEKEEP_PAT` | _(none)_ | token |", "FORGEKEEP_PAT")
                .and_then(stated_default),
            Some(Stated::Absent),
            "the cell reader does not recognise how both pages spell \"no default\""
        );
        assert_eq!(
            table_row(
                "| `FORGEKEEP_PAT` | the machine's hostname | t |",
                "FORGEKEEP_PAT"
            )
            .and_then(stated_default),
            Some(Stated::Prose("the machine's hostname")),
            "the cell reader invents a literal default out of prose that only describes a \
             behaviour"
        );

        let variables = documented_variables();
        let mut checked = 0;

        for (name, section) in documented_pages() {
            for variable in &variables {
                let row = table_row(&section, variable.name).unwrap_or_else(|| {
                    panic!(
                        "{name}: the environment table has no `{}` row — that table is where \
                         the person wiring `forgekeep-mcp` into an agent looks the variable \
                         up, and the source is the only place left without it",
                        variable.name
                    )
                });
                let stated = stated_default(row).unwrap_or_else(|| {
                    panic!(
                        "{name}: the `{}` row has no `Default` column",
                        variable.name
                    )
                });

                match (&variable.default, stated) {
                    (Some((konst, value)), Stated::Value(shown)) => assert_eq!(
                        shown,
                        value.as_str(),
                        "{name}: the `{}` row states the default `{shown}`, but `{konst}` — \
                         the constant the resolve actually falls back to — is `{value}`. An \
                         agent configured from this page then talks to an address the binary \
                         will not use, and the failure surfaces as an unreachable API, not as \
                         a wrong page",
                        variable.name
                    ),
                    (None, Stated::Absent) => {}
                    (Some((konst, value)), other) => panic!(
                        "{name}: the `Default` column of the `{}` row reads {other:?}, while \
                         the resolve falls back to `{konst}` = `{value}` — state that value \
                         in backticks so this check can hold the page to it",
                        variable.name
                    ),
                    (None, other) => panic!(
                        "{name}: the `Default` column of the `{}` row reads {other:?}, but \
                         nothing in the resolve produces a default for it — the page promises \
                         a value the binary has not got. `{NO_DEFAULT}` is how both pages \
                         spell the absence",
                        variable.name
                    ),
                }
                checked += 1;
            }
        }

        // A floor, not a count: three variables on two pages. A scanner that
        // stopped matching would otherwise read as agreement.
        assert!(
            checked >= 6,
            "only {checked} table rows were matched against the resolve — the page scanner \
             has drifted away from how the tables are written"
        );
    }

    /// The other half of the same contract, and the one the pages cannot state:
    /// that the value they name is reached through a *name*. A fallback written
    /// into `unwrap_or_else` as a literal is unreachable from any check — which
    /// is exactly how this crate's address stood for three copies with nothing
    /// holding them equal.
    #[test]
    fn every_environment_default_the_mcp_resolve_uses_comes_from_a_named_constant() {
        const PROBE: &str = "let a = std::env::var(\"PROBE_CONST\")\n\
             .unwrap_or_else(|_| DEFAULT_PROBE.to_string());\n\
             let b = std::env::var(\"PROBE_EMPTY\").unwrap_or_default();\n\
             let c = std::env::var(\"PROBE_LITERAL\").unwrap_or_else(|_| \"http://probe\".to_string());\n\
             let d = std::env::var(\"PROBE_OTHER\").ok();\n";

        const DECOYS: &str = r###"// std::env::var("PROBE_DECOYS").unwrap_or_else(|_| DECOY_LINE.to_string());
/* std::env::var("PROBE_DECOYS").unwrap_or_else(|_| DECOY_BLOCK.to_string()); */
let normal = "std::env::var(\"PROBE_DECOYS\").unwrap_or_else(|_| DECOY_NORMAL.to_string());";
let bytes = b"std::env::var(\"PROBE_DECOYS\").unwrap_or_else(|_| DECOY_BYTES.to_string());";
let raw = r#"std::env::var("PROBE_DECOYS").unwrap_or_else(|_| DECOY_RAW.to_string());"#;
let raw_bytes = br#"std::env::var("PROBE_DECOYS").unwrap_or_else(|_| DECOY_RAW_BYTES.to_string());"#;
let real = std::env::var("PROBE_DECOYS") /* ; .unwrap_or_default() */
    .unwrap_or_else(|_| DEFAULT_PROBE.to_string());
"###;

        assert_eq!(
            env_fallback(PROBE, "PROBE_CONST").map(|located| located.fallback),
            Some(Fallback::Constant("DEFAULT_PROBE")),
            "the resolve reader does not recognise a fallback that comes from a constant"
        );
        assert_eq!(
            env_fallback(PROBE, "PROBE_EMPTY").map(|located| located.fallback),
            Some(Fallback::Empty),
            "the resolve reader does not recognise `unwrap_or_default()` as the absence of a \
             default"
        );
        assert_eq!(
            env_fallback(PROBE, "PROBE_LITERAL").map(|located| located.fallback),
            Some(Fallback::Literal("http://probe")),
            "the resolve reader takes a literal written into the resolve for a named \
             constant, so the one shape these checks exist to catch would pass"
        );
        assert_eq!(
            env_fallback(PROBE, "PROBE_OTHER").map(|located| located.fallback),
            Some(Fallback::Unknown(".ok()")),
            "the resolve reader guesses at a shape it has not been taught to read"
        );
        assert_eq!(
            env_fallback(PROBE, "PROBE_ABSENT"),
            None,
            "the resolve reader claims to read a variable nothing resolves"
        );

        let decoy_proof = env_fallback(DECOYS, "PROBE_DECOYS")
            .expect("the real env::var call after the decoys must remain visible");
        assert_eq!(
            decoy_proof,
            LocatedFallback {
                line: 7,
                fallback: Fallback::Constant("DEFAULT_PROBE"),
            },
            "a comment or normal/raw/byte literal was mistaken for the production call, or \
             comment text between the real call and fallback was parsed as code"
        );

        let source = production_lib_source();

        for variable in &documented_variables() {
            let located = env_fallback(&source, variable.name).unwrap_or_else(|| {
                panic!(
                    "{PRODUCTION_LIB_PATH}: no production source of `rg-mcp` resolves `{}`, \
                     yet both pages document \
                     it — an operator sets a variable that does nothing",
                    variable.name
                )
            });
            let location = format!("{PRODUCTION_LIB_PATH}:{}", located.line);

            match (&variable.default, located.fallback) {
                (Some((konst, _)), Fallback::Constant(used)) => assert_eq!(
                    used, *konst,
                    "{location}: `{}` falls back to `{used}`, but the pairing table binds the pages to \
                     `{konst}` — the tables are then checked against a constant the resolve \
                     no longer uses",
                    variable.name
                ),
                (None, Fallback::Empty) => {}
                (_, Fallback::Literal(value)) => panic!(
                    "{location}: the fallback `{value}` for `{}` is written into the resolve itself. Both \
                     pages restate that value, and a literal has no name for them to be bound \
                     to — give it a `const DEFAULT_*` beside `from_env` and pair it in \
                     documented_variables()",
                    variable.name
                ),
                (Some((konst, value)), other) => panic!(
                    "{location}: `{}` resolves as {other:?}, but the pages are held to `{konst}` = \
                     `{value}` — pair the variable with what it now falls back to, or restore \
                     the constant",
                    variable.name
                ),
                (None, other) => panic!(
                    "{location}: `{}` resolves as {other:?}, and both pages state `{NO_DEFAULT}` for it — \
                     a default the code grew and the pages never learned about",
                    variable.name
                ),
            }
        }
    }

    /// The census, in both directions: a default this crate declares that no
    /// page states, and a row pairing a constant that no longer exists.
    #[test]
    fn every_default_constant_this_crate_declares_is_stated_on_an_operator_page() {
        assert_eq!(
            declared_default_constants(
                "pub const DEFAULT_X: &str = \"1\";\n    const DEFAULT_Y: u8 = 2;\n\
                 pub(crate) const DEFAULT_Z: u8 = 3;\nconst OTHER: u8 = 4;\n"
            ),
            BTreeSet::from(["DEFAULT_X", "DEFAULT_Y", "DEFAULT_Z"]),
            "the declaration scan does not read `const DEFAULT_*` the way this crate writes \
             them — a private one would escape the census entirely"
        );

        let sources = production_sources();
        assert!(
            sources.len() >= 5,
            "the crate walk found only {} production sources — it is looking in the wrong \
             place, and an empty census agrees with anything",
            sources.len()
        );

        let mut declared: BTreeSet<String> = BTreeSet::new();
        for (_, text) in &sources {
            declared.extend(
                declared_default_constants(text)
                    .into_iter()
                    .map(str::to_owned),
            );
        }

        let variables = documented_variables();
        let paired: BTreeSet<&str> = variables
            .iter()
            .filter_map(|variable| variable.default.as_ref().map(|(konst, _)| *konst))
            .collect();
        assert!(
            !paired.is_empty(),
            "documented_variables() pairs no constant at all, so nothing below is checked"
        );

        for name in &declared {
            assert!(
                paired.contains(name.as_str())
                    || DEFAULTS_NOT_OPERATOR_FACING
                        .iter()
                        .any(|(excused, _)| excused == name),
                "`{name}` is a built-in default of `rg-mcp` that no row of \
                 documented_variables() pairs with a variable — pair it with the variable \
                 whose table row states it, or name it in DEFAULTS_NOT_OPERATOR_FACING with \
                 the reason no operator ever meets it"
            );
        }

        // Renaming a paired constant breaks the build, but *moving* one out of
        // the crate would not: it would simply leave the census.
        for name in &paired {
            assert!(
                declared.contains(*name),
                "documented_variables() pairs `{name}`, which no production source of this \
                 crate declares any more — the pages would then be held to a constant that \
                 lives somewhere the census cannot see"
            );
        }

        for (excused, reason) in DEFAULTS_NOT_OPERATOR_FACING {
            assert!(
                !reason.is_empty(),
                "`{excused}` is excused from the census without a reason"
            );
            assert!(
                declared.contains(excused),
                "DEFAULTS_NOT_OPERATOR_FACING still excuses `{excused}`, which this crate no \
                 longer declares — drop the entry so the list keeps meaning something"
            );
        }
    }
}
