//! `POST /api/v1/mcp` — the MCP tools, served by the server itself
//! (card_60a80311d512).
//!
//! The `forgekeep-mcp` binary speaks MCP over stdio and calls this API over the
//! network with a PAT. That works, and it is also why the server could never
//! tell an agent's request from its owner's, nor which tool a request served:
//! all it saw was REST calls. Here the same tools (`rg-mcp`) run inside the
//! server, and each API call a tool makes is dispatched in-process through the
//! very router that serves this endpoint — every middleware layer, every gate,
//! with the caller's own credential. Two things ride along that no client can
//! forge, because they exist only as request extensions:
//!
//! - [`McpToolCall`] names the tool, so a token confined to some tools is
//!   refused the rest, and the audit rows those calls write say which tool
//!   they came through;
//! - the caller's [`TokenGrant`], so the narrowing the PAT middleware resolved
//!   once applies to every inner call without the raw token travelling again.
//!
//! Transport: the "Streamable HTTP" shape without a stream — one JSON-RPC
//! request per `POST`, answered with one JSON body; a notification is answered
//! `202` with no body; `GET` is `405`, which the specification allows a server
//! that offers no server-initiated stream to answer.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use tower::ServiceExt;

use super::auth::AuthUser;
use crate::agent_scope::{McpToolCall, TokenGrant};
use crate::AppState;
use rg_mcp::protocol::{make_error, JsonRpcRequest};

/// Largest JSON-RPC message the endpoint reads. A tool call is a handful of
/// arguments — an issue body, a review comment — so this is generous; it is a
/// ceiling this router chose rather than Axum's 2 MiB default.
pub const MCP_REQUEST_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Largest body one in-process API call may answer a tool with. The tools
/// buffer whole responses, exactly as they do over HTTP; this bounds what one
/// call can make the server hold for them.
const INNER_RESPONSE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Per-call ceiling, the in-process twin of `forgekeep-mcp`'s HTTP timeout.
const INNER_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// JSON-RPC error code for a tool the caller's token does not admit.
const TOOL_NOT_PERMITTED: i32 = -32003;

/// The router this state is mounted in.
///
/// Filled once the router exists — it contains this endpoint, so it cannot be
/// passed in while it is being built. The router then holds the state that
/// holds the slot that holds the router: a cycle, deliberately never
/// collected, because a router lives exactly as long as the process serving it.
#[derive(Clone, Default)]
pub struct McpRouterSlot(Arc<OnceLock<axum::Router>>);

impl McpRouterSlot {
    pub(crate) fn publish(&self, router: &axum::Router) {
        // A second publish into one slot would mean two routers share it;
        // `routes` gives every router a fresh slot, so the first one stands.
        if self.0.set(router.clone()).is_err() {
            tracing::warn!("the MCP router slot was published twice; keeping the first router");
        }
    }

    fn router(&self) -> Option<axum::Router> {
        self.0.get().cloned()
    }
}

/// What a tool's API call is sent with: the caller's credential and the
/// request facts the gates and the audit log read, nothing else.
struct InProcessTransport {
    router: axum::Router,
    /// `Authorization` as the PAT middleware left it — a JWT the handlers
    /// accept, never the raw token.
    authorization: Option<HeaderValue>,
    forwarded: Vec<(header::HeaderName, HeaderValue)>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    grant: Option<TokenGrant>,
    tool: Option<String>,
}

impl rg_mcp::ApiTransport for InProcessTransport {
    fn exchange(
        &self,
        method: rg_mcp::Method,
        path: String,
        body: Option<serde_json::Value>,
    ) -> rg_mcp::ApiFuture<'_> {
        Box::pin(async move {
            let transport_error = |what: String| rg_mcp::Error::Transport(what);
            let method = axum::http::Method::from_bytes(method.as_str().as_bytes())
                .map_err(|error| transport_error(error.to_string()))?;
            let mut builder = axum::http::Request::builder().method(method).uri(&path);
            if let Some(value) = &self.authorization {
                builder = builder.header(header::AUTHORIZATION, value.clone());
            }
            for (name, value) in &self.forwarded {
                builder = builder.header(name.clone(), value.clone());
            }
            let body = match body {
                Some(json) => {
                    builder = builder.header(header::CONTENT_TYPE, "application/json");
                    axum::body::Body::from(serde_json::to_vec(&json)?)
                }
                None => axum::body::Body::empty(),
            };
            let mut request = builder
                .body(body)
                .map_err(|error| transport_error(error.to_string()))?;
            let extensions = request.extensions_mut();
            if let Some(tool) = &self.tool {
                extensions.insert(McpToolCall { tool: tool.clone() });
            }
            if let Some(grant) = &self.grant {
                extensions.insert(grant.clone());
            }
            if let Some(connect_info) = self.connect_info {
                extensions.insert(connect_info);
            }

            let response =
                tokio::time::timeout(INNER_CALL_TIMEOUT, self.router.clone().oneshot(request))
                    .await
                    .map_err(|_| transport_error(format!("{path} did not answer in time")))?
                    .unwrap_or_else(|never: std::convert::Infallible| match never {});
            let status = response.status().as_u16();
            let body = axum::body::to_bytes(response.into_body(), INNER_RESPONSE_MAX_BYTES)
                .await
                .map_err(|error| transport_error(format!("reading {path}: {error}")))?;
            Ok(rg_mcp::ApiResponse {
                status,
                body: body.to_vec(),
            })
        })
    }
}

/// The headers an inner call carries over from the agent's request: what the
/// audit log reads for the address and client, and the request id that ties
/// the inner calls to the outer one in the logs.
const FORWARDED_HEADERS: &[&str] = &["user-agent", "x-forwarded-for", "x-real-ip", "x-request-id"];

fn json_rpc(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

/// Serve one MCP JSON-RPC message.
#[utoipa::path(
    post,
    path = "/mcp",
    tag = "MCP",
    request_body(content = serde_json::Value, description = "One JSON-RPC 2.0 message of the Model Context Protocol"),
    responses(
        (status = 200, description = "The JSON-RPC response", body = serde_json::Value),
        (status = 202, description = "A notification was accepted; no body"),
        (status = 400, description = "Not a single JSON-RPC message", body = serde_json::Value),
        (status = 401, description = "No bearer credential", body = serde_json::Value),
        (status = 403, description = "The token does not admit the requested tool", body = serde_json::Value),
    ),
)]
pub async fn mcp_endpoint(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    grant: Option<Extension<TokenGrant>>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let grant = grant.map(|Extension(grant)| grant);
    // An agent presents a bearer credential. A browser session cookie is not
    // accepted here: this endpoint acts on the caller's behalf across the whole
    // API, and a cookie is what another site can make a browser send.
    let Some(authorization) = headers.get(header::AUTHORIZATION).cloned() else {
        return crate::error::AppError::unauthorized(
            "the MCP endpoint takes a bearer credential: Authorization: Bearer <token>",
        )
        .into_response();
    };

    let request: JsonRpcRequest = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(serde_json::Value::Array(_)) => {
            return json_rpc(
                StatusCode::BAD_REQUEST,
                serde_json::to_value(make_error(
                    serde_json::Value::Null,
                    -32600,
                    "batched JSON-RPC requests are not supported; send one message per request",
                ))
                .unwrap_or_default(),
            );
        }
        Ok(value) => match serde_json::from_value(value) {
            Ok(request) => request,
            Err(error) => {
                return json_rpc(
                    StatusCode::BAD_REQUEST,
                    serde_json::to_value(make_error(
                        serde_json::Value::Null,
                        -32600,
                        &format!("invalid JSON-RPC request: {error}"),
                    ))
                    .unwrap_or_default(),
                );
            }
        },
        Err(error) => {
            return json_rpc(
                StatusCode::BAD_REQUEST,
                serde_json::to_value(make_error(
                    serde_json::Value::Null,
                    -32700,
                    &format!("parse error: {error}"),
                ))
                .unwrap_or_default(),
            );
        }
    };
    if request.is_notification() {
        return StatusCode::ACCEPTED.into_response();
    }

    let tool = (request.method == "tools/call")
        .then(|| {
            request
                .params
                .as_ref()
                .and_then(|params| params.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .flatten();
    if let (Some(tool), Some(grant)) = (&tool, &grant) {
        if !grant.admits_tool(tool) {
            grant
                .record_denial(
                    &headers,
                    serde_json::json!({ "reason": "mcp_tool", "tool": tool }),
                )
                .await;
            return json_rpc(
                StatusCode::FORBIDDEN,
                serde_json::to_value(make_error(
                    request.id.clone(),
                    TOOL_NOT_PERMITTED,
                    &format!("this token may not use the tool '{tool}'"),
                ))
                .unwrap_or_default(),
            );
        }
    }

    let Some(router) = state.mcp_router.router() else {
        return crate::error::AppError::internal("the MCP endpoint has no router to dispatch to")
            .into_response();
    };
    let transport = InProcessTransport {
        router,
        authorization: Some(authorization),
        forwarded: FORWARDED_HEADERS
            .iter()
            .filter_map(|name| {
                headers
                    .get(*name)
                    .map(|value| (header::HeaderName::from_static(name), value.clone()))
            })
            .collect(),
        connect_info: connect_info.map(|Extension(info)| info),
        grant: grant.clone(),
        tool: tool.clone(),
    };
    let mcp_state = rg_mcp::AppState::in_process(Arc::new(transport));

    // The tools are synchronous and drive their API calls with `block_on`, so
    // they run on the blocking pool, never on a runtime worker.
    let request = Arc::new(request);
    let dispatched = {
        let request = Arc::clone(&request);
        tokio::task::spawn_blocking(move || rg_mcp::dispatch(&mcp_state, &request)).await
    };
    let response = match dispatched {
        Ok(Some(response)) => response,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(error) => {
            return crate::error::AppError::internal(format!("MCP dispatch failed: {error}"))
                .into_response();
        }
    };
    let mut response = match serde_json::to_value(&response) {
        Ok(value) => value,
        Err(error) => return crate::error::AppError::internal(error).into_response(),
    };

    if request.method == "tools/list" {
        if let Some(allowed) = grant.as_ref().and_then(TokenGrant::mcp_tools) {
            if let Some(tools) = response
                .pointer_mut("/result/tools")
                .and_then(|tools| tools.as_array_mut())
            {
                tools.retain(|tool| {
                    tool.get("name")
                        .and_then(|name| name.as_str())
                        .is_some_and(|name| allowed.contains(&name))
                });
            }
        }
    }

    if let Some(tool) = &tool {
        let is_error = response
            .pointer("/result/isError")
            .and_then(|flag| flag.as_bool())
            .unwrap_or(response.get("error").is_some());
        let actor = rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, user_id).await;
        rg_core::audit::record(
            &state.db,
            &actor,
            MCP_TOOL_CALL_ACTION,
            None,
            None,
            None,
            Some(&headers),
            Some(serde_json::json!({ "tool": tool, "is_error": is_error })),
        )
        .await;
    }

    json_rpc(StatusCode::OK, response)
}

/// The audit action every tool call through this endpoint writes.
pub const MCP_TOOL_CALL_ACTION: &str = "agent.mcp_tool_call";
