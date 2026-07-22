//! MCP **Resources** – dispatched from `main.rs::dispatch`.
//!
//! ## Requests handled
//! |`method`            | purpose                       |
//! |--------------------|--------------------------------|
//! |`resources/list`    | list available resource types  |
//! |`resources/read`    | read a resource by URI         |

use super::protocol::*;
use super::AppState;
use serde_json::Value;

// ── public: list ────────────────────────────────────

pub fn list_resources(_state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let list = serde_json::json!([
        {
            "uri": "repo://{owner}/{name}",
            "name": "Repository metadata",
            "description": "Returns JSON with repo name, description, default branch, etc.",
            "mimeType": "application/json"
        },
        {
            "uri": "file://{owner}/{name}/{path}",
            "name": "File content",
            "description": "Returns the UTF-8 text content of the requested file.",
            "mimeType": "text/plain; charset=utf-8"
        },
        {
            "uri": "issue://{owner}/{name}/{number}",
            "name": "Issue details",
            "description": "Returns JSON with issue title, body, state, labels, etc.",
            "mimeType": "application/json"
        }
    ]);
    make_success(req.id.clone(), serde_json::json!({ "resources": list }))
}

// ── public: read ────────────────────────────────────

pub fn read_resource(state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let params = match &req.params {
        Some(v) => v.clone(),
        None => {
            return make_error(req.id.clone(), -32602, "missing params");
        }
    };

    let uri = match params.get("uri").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return make_error(req.id.clone(), -32602, "missing uri parameter");
        }
    };

    // dispatch by URI scheme
    if uri.starts_with("repo://") {
        handle_repo_meta(state, req, &uri)
    } else if uri.starts_with("file://") {
        handle_file_content(state, req, &uri)
    } else if uri.starts_with("issue://") {
        handle_issue_details(state, req, &uri)
    } else {
        make_error(
            req.id.clone(),
            -32602,
            &format!("unsupported URI scheme: {}", uri),
        )
    }
}

// ── handlers ──────────────────────────────────────────

fn handle_repo_meta(state: &AppState, req: &JsonRpcRequest, uri: &str) -> JsonRpcResponse {
    // parse owner/name from "repo://owner/name"
    let parts: Vec<&str> = uri.trim_start_matches("repo://").splitn(2, '/').collect();
    if parts.len() != 2 {
        return make_error(req.id.clone(), -32602, "invalid repo URI format");
    }
    let owner = parts[0];
    let name = parts[1];

    let client = crate::client::ApiClient::new(state);
    let path = format!("/repos/{}/{}", owner, name);
    match tokio::runtime::Handle::current().block_on(client.get::<Value>(&path)) {
        Ok(v) => {
            let contents = serde_json::json!([{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&v).unwrap_or_default()
            }]);
            make_success(req.id.clone(), serde_json::json!({ "contents": contents }))
        }
        Err(e) => make_error(req.id.clone(), -32000, &e.to_string()),
    }
}

fn handle_file_content(state: &AppState, req: &JsonRpcRequest, uri: &str) -> JsonRpcResponse {
    // parse owner/name/path from "file://owner/name/path/to/file"
    let stripped = uri.trim_start_matches("file://");
    let parts: Vec<&str> = stripped.splitn(3, '/').collect();
    if parts.len() < 3 {
        return make_error(req.id.clone(), -32602, "invalid file URI format");
    }
    let owner = parts[0];
    let name = parts[1];
    let path = parts[2];

    let client = crate::client::ApiClient::new(state);
    let api_path = format!("/repos/{}/{}/contents/{}", owner, name, path);
    match tokio::runtime::Handle::current().block_on(client.get_raw(&api_path)) {
        Ok(text) => {
            let contents = serde_json::json!([{
                "uri": uri,
                "mimeType": "text/plain; charset=utf-8",
                "text": text
            }]);
            make_success(req.id.clone(), serde_json::json!({ "contents": contents }))
        }
        Err(e) => make_error(req.id.clone(), -32000, &e.to_string()),
    }
}

fn handle_issue_details(state: &AppState, req: &JsonRpcRequest, uri: &str) -> JsonRpcResponse {
    // parse owner/name/number from "issue://owner/name/number"
    let stripped = uri.trim_start_matches("issue://");
    let parts: Vec<&str> = stripped.rsplitn(2, '/').collect();
    // parts = [number, "owner/name"]
    if parts.len() != 2 {
        return make_error(req.id.clone(), -32602, "invalid issue URI format");
    }
    let number: i64 = match parts[0].parse() {
        Ok(n) => n,
        Err(_) => {
            return make_error(req.id.clone(), -32602, "invalid issue number");
        }
    };
    let owner_name = parts[1];
    let on_parts: Vec<&str> = owner_name.rsplitn(2, '/').collect();
    if on_parts.len() != 2 {
        return make_error(req.id.clone(), -32602, "invalid issue URI format");
    }
    let owner = on_parts[1];
    let name = on_parts[0];

    let client = crate::client::ApiClient::new(state);
    let path = format!("/repos/{}/{}/issues/{}", owner, name, number);
    match tokio::runtime::Handle::current().block_on(client.get::<Value>(&path)) {
        Ok(v) => {
            let contents = serde_json::json!([{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&v).unwrap_or_default()
            }]);
            make_success(req.id.clone(), serde_json::json!({ "contents": contents }))
        }
        Err(e) => make_error(req.id.clone(), -32000, &e.to_string()),
    }
}
