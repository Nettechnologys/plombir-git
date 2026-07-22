//! MCP **Tools** – dispatched from `main.rs::dispatch`.
//!
//! Each tool:
//! 1. parses `req.params` → arguments JSON
//! 2. calls IronForge REST API
//! 3. returns `JsonRpcResponse` with `ToolCallResult`

use super::protocol::*;
use super::AppState;
use serde_json::Value;

// ── public: list tools ─────────────────────────────────

pub fn list_tools(_state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let tools = serde_json::json!([
        {
            "name": "list_repos",
            "description": "List Git repositories the caller can access.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Optional owner filter" }
                },
                "required": []
            }
        },
        {
            "name": "read_file",
            "description": "Read a file's content from a repository (UTF-8 text files only).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "path":  { "type": "string", "description": "File path, e.g. 'src/main.rs'" },
                    "ref":   { "type": "string", "description": "Git ref (branch/tag/commit). Defaults to default branch." }
                },
                "required": ["owner", "repo", "path"]
            }
        },
        {
            "name": "read_dir",
            "description": "List files and directories at a given path in a repository.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "path":  { "type": "string", "description": "Directory path ('' for repo root)" },
                    "ref":   { "type": "string", "description": "Git ref. Defaults to 'main'." }
                },
                "required": ["owner", "repo"]
            }
        },
        {
            "name": "get_issue",
            "description": "Get a single issue by number.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "number":{ "type": "number", "description": "Issue number" }
                },
                "required": ["owner", "repo", "number"]
            }
        },
        {
            "name": "get_pr",
            "description": "Get a single pull request by number (includes diff when available).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "number":{ "type": "number", "description": "Pull request number" }
                },
                "required": ["owner", "repo", "number"]
            }
        }
    ]);

    make_success(req.id.clone(), serde_json::json!({ "tools": tools }))
}

// ── public: call tool ─────────────────────────────────────

/// Dispatch a JSON-RPC tool call to the appropriate tool handler.
pub fn call_tool(state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let params = match &req.params {
        Some(v) => v.clone(),
        None => {
            return make_error(req.id.clone(), -32602, "missing params");
        }
    };

    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return make_error(req.id.clone(), -32602, "missing tool name");
        }
    };

    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let result = match name {
        "list_repos" => tool_list_repos(state, &args),
        "read_file" => tool_read_file(state, &args),
        "read_dir" => tool_read_dir(state, &args),
        "get_issue" => tool_get_issue(state, &args),
        "get_pr" => tool_get_pr(state, &args),
        _ => {
            return make_error(req.id.clone(), -32601, &format!("unknown tool: {}", name));
        }
    };

    let content = serde_json::json!({
        "content": [ { "type": "text", "text": result } ]
    });
    make_success(req.id.clone(), content)
}

// ── tool implementations ─────────────────────────────────

fn tool_list_repos(state: &AppState, _args: &Value) -> String {
    let client = crate::client::ApiClient::new(state);
    match tokio::runtime::Handle::current().block_on(client.get::<Value>("/repos")) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|e| e.to_string()),
        Err(e) => format!("Error: {}", e),
    }
}

fn tool_read_file(state: &AppState, args: &Value) -> String {
    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
    let repo = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let ref_ = args.get("ref").and_then(|v| v.as_str()).unwrap_or("");

    if owner.is_empty() || repo.is_empty() || path.is_empty() {
        return "Error: owner, repo and path are required".into();
    }

    let api_path = format!("/repos/{}/{}/contents/{}?ref={}", owner, repo, path, ref_);
    let client = crate::client::ApiClient::new(state);
    match tokio::runtime::Handle::current().block_on(client.get_raw(&api_path)) {
        Ok(text) => text,
        Err(e) => format!("Error: {}", e),
    }
}

fn tool_read_dir(state: &AppState, args: &Value) -> String {
    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
    let repo = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let ref_ = args.get("ref").and_then(|v| v.as_str()).unwrap_or("main");

    if owner.is_empty() || repo.is_empty() {
        return "Error: owner and repo are required".into();
    }

    let api_path = format!(
        "/repos/{}/{}/tree/{}?path={}",
        owner,
        repo,
        ref_,
        urlencoding::encode(path)
    );
    let client = crate::client::ApiClient::new(state);
    match tokio::runtime::Handle::current().block_on(client.get_raw(&api_path)) {
        Ok(text) => text,
        Err(e) => format!("Error: {}", e),
    }
}

fn tool_get_issue(state: &AppState, args: &Value) -> String {
    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
    let repo = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let number: i64 = args.get("number").and_then(|v| v.as_i64()).unwrap_or(0);

    if owner.is_empty() || repo.is_empty() || number == 0 {
        return "Error: owner, repo and number are required".into();
    }

    let api_path = format!("/repos/{}/{}/issues/{}", owner, repo, number);
    let client = crate::client::ApiClient::new(state);
    match tokio::runtime::Handle::current().block_on(client.get_raw(&api_path)) {
        Ok(text) => text,
        Err(e) => format!("Error: {}", e),
    }
}

fn tool_get_pr(state: &AppState, args: &Value) -> String {
    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
    let repo = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let number: i64 = args.get("number").and_then(|v| v.as_i64()).unwrap_or(0);

    if owner.is_empty() || repo.is_empty() || number == 0 {
        return "Error: owner, repo and number are required".into();
    }

    let api_path = format!("/repos/{}/{}/pulls/{}", owner, repo, number);
    let client = crate::client::ApiClient::new(state);
    match tokio::runtime::Handle::current().block_on(client.get_raw(&api_path)) {
        Ok(text) => text,
        Err(e) => format!("Error: {}", e),
    }
}
