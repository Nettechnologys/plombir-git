//! Minimal MCP (Model Context Protocol) 1.0 implementation.
//!
//! Implements JSON-RPC 2.0 over **stdio** transport.
//! Only the subset needed for a working server is included:
//!
//! - `initialize` / `notifications/initialized`
//! - `tools/list`  + `tools/call`
//! - `resources/list` + `resources/read`
//! - `notifications/cancelled`
//!
//! Reference: https://modelcontextprotocol.io/specification

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── JSON-RPC 2.0 envelope ───────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String, // always "2.0"
    pub id: Value,       // number | string
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
}

// ── MCP-specific types ───────────────────────────────────────────────────

/// `initialize` result → `serverInfo` + `capabilities`.
#[derive(Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    pub capabilities: Capabilities,
}

#[derive(Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Capabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
}

#[derive(Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ResourcesCapability {
    pub subscribe: bool,
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

// ── Tool types ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value, // JSON Schema object
}

#[derive(Serialize, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    pub arguments: Value,
}

#[derive(Serialize, Deserialize)]
pub struct ToolCallResult {
    pub content: Vec<Content>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    Resource { resource: ResourceContents },
}

// ── Resource types ────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ResourceContents {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

// ── stdio transport helpers ───────────────────────────────────────────

/// Read one JSON-RPC **request** (or notification) from stdin.
///
/// Lines are assumed to be LF-delimited JSON objects.
/// Returns `None` when stdin is closed.
pub fn read_request() -> std::io::Result<Option<String>> {
    use std::io::{BufRead, BufReader};
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
        // skip empty lines (some wrappers add them)
    }
}

/// Write one JSON-RPC **response** to stdout.
///
/// MUST be the only thing written to stdout (agents read from it).
pub fn write_response(resp: &JsonRpcResponse) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let bytes = serde_json::to_vec(resp)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    use std::io::Write;
    handle.write_all(&bytes)?;
    handle.write_all(b"\n")?;
    handle.flush()?;
    Ok(())
}

/// Write a **notification** (no `id`) to stdout.
pub fn write_notification(notif: &JsonRpcNotification) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let bytes = serde_json::to_vec(notif)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    use std::io::Write;
    handle.write_all(&bytes)?;
    handle.write_all(b"\n")?;
    handle.flush()?;
    Ok(())
}

// ── error helpers ──────────────────────────────────────────────────────

pub fn make_error(id: Value, code: i32, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

pub fn make_success(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}
