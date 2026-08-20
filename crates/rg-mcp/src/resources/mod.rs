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

/// Signature shared by every resource handler.
type ResourceHandler = fn(&AppState, &JsonRpcRequest, &str) -> JsonRpcResponse;

/// URI schemes [`read_resource`] can actually serve, as data.
///
/// Same reason as `tools::TOOL_DISPATCH`: an `if / else if` chain of
/// `starts_with` is invisible at runtime, so nothing could compare the schemes
/// advertised by [`list_resources`] with the schemes that answer. As a table
/// the two sets are comparable, and `advertised_resources_match_the_dispatch_table`
/// fails on either drift direction.
const RESOURCE_DISPATCH: &[(&str, ResourceHandler)] = &[
    ("repo://", handle_repo_meta),
    ("file://", handle_file_content),
    ("issue://", handle_issue_details),
];

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
    match RESOURCE_DISPATCH
        .iter()
        .find(|(scheme, _)| uri.starts_with(scheme))
    {
        Some((_, handler)) => handler(state, req, &uri),
        None => make_error(
            req.id.clone(),
            -32602,
            &format!("unsupported URI scheme: {}", uri),
        ),
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
    // The same route the `read_file` tool reads through, for the same reason:
    // `/contents/{path}` is mounted for `POST` and `DELETE` only, so this
    // resource answered a router error for every URI it advertised
    // (card_66aa21756448).
    let api_path = crate::tools::read_file_path(owner, name, path, "");
    match tokio::runtime::Handle::current().block_on(client.get_raw(&api_path)) {
        Ok(text) => {
            let contents = serde_json::json!([{
                "uri": uri,
                // The blob endpoint answers with the file's metadata around its
                // content, not with the bare bytes — the same JSON envelope
                // `repo://` is declared with.
                "mimeType": "application/json",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn state() -> AppState {
        AppState::new("http://localhost:8080".into(), String::new())
    }

    fn req(method: &str) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Value::from(1),
            method: method.into(),
            params: None,
        }
    }

    /// The `resources/list` twin of the `tools/list` gate: what the server
    /// advertises and what it can actually serve must be the same set of URI
    /// schemes, compared against each other rather than against a hand-written
    /// third list.
    #[test]
    fn advertised_resources_match_the_dispatch_table() {
        let resp = list_resources(&state(), &req("resources/list"));
        let advertised: Vec<String> = resp
            .result
            .unwrap()
            .get("resources")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let uri = r["uri"].as_str().unwrap();
                let end = uri.find("://").expect("advertised uri carries no scheme") + 3;
                uri[..end].to_string()
            })
            .collect();

        assert!(!advertised.is_empty(), "resources/list advertises nothing");
        assert!(!RESOURCE_DISPATCH.is_empty(), "dispatch table is empty");

        let advertised_set: BTreeSet<&str> = advertised.iter().map(String::as_str).collect();
        let dispatched_set: BTreeSet<&str> = RESOURCE_DISPATCH
            .iter()
            .map(|(scheme, _)| *scheme)
            .collect();

        assert_eq!(
            dispatched_set.len(),
            RESOURCE_DISPATCH.len(),
            "duplicate scheme in RESOURCE_DISPATCH"
        );

        let undispatched: Vec<&&str> = advertised_set.difference(&dispatched_set).collect();
        assert!(
            undispatched.is_empty(),
            "advertised by resources/list but not served by read_resource — every read \
             answers 'unsupported URI scheme': {undispatched:?}"
        );

        let unadvertised: Vec<&&str> = dispatched_set.difference(&advertised_set).collect();
        assert!(
            unadvertised.is_empty(),
            "served by read_resource but never advertised — implemented and undiscoverable: \
             {unadvertised:?}"
        );
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        let mut r = req("resources/read");
        r.params = Some(serde_json::json!({ "uri": "gopher://o/n" }));
        let resp = read_resource(&state(), &r);
        assert_eq!(resp.error.unwrap().code, -32602);
    }
}
