//! MCP **Tools** – dispatched from `main.rs::dispatch`.
//!
//! Each tool:
//! 1. parses `req.params` → arguments JSON
//! 2. calls ForgeKeep REST API
//! 3. returns `JsonRpcResponse` with `ToolCallResult`
//!
//! Every handler is a thin wrapper around one REST call (the `tool_get_pr`
//! pattern): parse args → build path/body → `ApiClient` → return text. Write
//! tools construct an explicit JSON body from a whitelist of keys so path
//! parameters (owner/repo/number) never leak into the request body.

use super::protocol::*;
use super::AppState;
use serde_json::Value;

// ── public: list tools ─────────────────────────────────

pub fn list_tools(_state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let tools = serde_json::json!([
        // ── Read: repos & content ──────────────────────────
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
                    "ref":   { "type": "string", "description": "Git ref (branch/tag/commit). Defaults to the repository's default branch." }
                },
                "required": ["owner", "repo"]
            }
        },
        // ── Read: issues & PRs ─────────────────────────────
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
        },
        {
            "name": "get_pr_diff",
            "description": "Get the unified diff of a pull request.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "number":{ "type": "number", "description": "Pull request number" }
                },
                "required": ["owner", "repo", "number"]
            }
        },
        // ── Write: issues ──────────────────────────────────
        {
            "name": "create_issue",
            "description": "Create a new issue in a repository.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":       { "type": "string", "description": "Repository owner" },
                    "repo":        { "type": "string", "description": "Repository name" },
                    "title":       { "type": "string", "description": "Issue title" },
                    "body":        { "type": "string", "description": "Issue body (Markdown)" },
                    "labels":      { "type": "array", "items": { "type": "string" }, "description": "Label names" },
                    "milestone_id":{ "type": "number", "description": "Milestone id" }
                },
                "required": ["owner", "repo", "title"]
            }
        },
        {
            "name": "update_issue",
            "description": "Update an issue: title, body, state (open/closed), labels, assignee or milestone.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":       { "type": "string", "description": "Repository owner" },
                    "repo":        { "type": "string", "description": "Repository name" },
                    "number":      { "type": "number", "description": "Issue number" },
                    "title":       { "type": "string", "description": "New title" },
                    "body":        { "type": "string", "description": "New body" },
                    "state":       { "type": "string", "description": "'open' or 'closed'" },
                    "labels":      { "type": "array", "items": { "type": "string" }, "description": "Replace label set" },
                    "assignee_id": { "type": ["number", "null"], "description": "Assignee user id (null to clear)" },
                    "milestone_id":{ "type": ["number", "null"], "description": "Milestone id (null to clear)" }
                },
                "required": ["owner", "repo", "number"]
            }
        },
        {
            "name": "comment_issue",
            "description": "Add a comment to an issue.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "number":{ "type": "number", "description": "Issue number" },
                    "body":  { "type": "string", "description": "Comment body (Markdown)" }
                },
                "required": ["owner", "repo", "number", "body"]
            }
        },
        {
            "name": "set_issue_labels",
            "description": "Replace the label set on an issue.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "number":{ "type": "number", "description": "Issue number" },
                    "labels":{ "type": "array", "items": { "type": "string" }, "description": "New label set" }
                },
                "required": ["owner", "repo", "number", "labels"]
            }
        },
        // ── Write: pull requests ───────────────────────────
        {
            "name": "create_pr",
            "description": "Open a new pull request.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "title": { "type": "string", "description": "PR title" },
                    "body":  { "type": "string", "description": "PR body (Markdown)" },
                    "head":  { "type": "string", "description": "Head branch. 'owner:branch' for a fork, or 'branch' for same-repo." },
                    "base":  { "type": "string", "description": "Base branch to merge into" },
                    "draft": { "type": "boolean", "description": "Open as draft" }
                },
                "required": ["owner", "repo", "title", "head", "base"]
            }
        },
        {
            "name": "merge_pr",
            "description": "Merge a pull request.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":   { "type": "string", "description": "Repository owner" },
                    "repo":    { "type": "string", "description": "Repository name" },
                    "number":  { "type": "number", "description": "Pull request number" },
                    "strategy":{ "type": "string", "description": "merge / squash / rebase" }
                },
                "required": ["owner", "repo", "number", "strategy"]
            }
        },
        {
            "name": "request_reviewers",
            "description": "Request a reviewer on a pull request.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":   { "type": "string", "description": "Repository owner" },
                    "repo":    { "type": "string", "description": "Repository name" },
                    "number":  { "type": "number", "description": "Pull request number" },
                    "username":{ "type": "string", "description": "Reviewer username" }
                },
                "required": ["owner", "repo", "number", "username"]
            }
        },
        // ── Write: reviews ─────────────────────────────────
        {
            "name": "create_review",
            "description": "Submit a review on a pull request (comment / approve / request_changes / dismiss).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":     { "type": "string", "description": "Repository owner" },
                    "repo":      { "type": "string", "description": "Repository name" },
                    "number":    { "type": "number", "description": "Pull request number" },
                    "action":    { "type": "string", "description": "comment / approve / request_changes / dismiss" },
                    "body":      { "type": "string", "description": "Review body (Markdown)" },
                    "commit_id": { "type": "string", "description": "Commit SHA the review pins to (optional)" }
                },
                "required": ["owner", "repo", "number", "action"]
            }
        },
        {
            "name": "create_review_comment",
            "description": "Create an inline review comment on a pull request diff (optionally with a suggestion).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":      { "type": "string", "description": "Repository owner" },
                    "repo":       { "type": "string", "description": "Repository name" },
                    "number":     { "type": "number", "description": "Pull request number" },
                    "path":       { "type": "string", "description": "File path the comment targets" },
                    "body":       { "type": "string", "description": "Comment body (Markdown)" },
                    "line":       { "type": "number", "description": "Line number in the diff" },
                    "start_line": { "type": "number", "description": "Start line for a multi-line comment" },
                    "side":       { "type": "string", "description": "'LEFT' or 'RIGHT'" },
                    "start_side": { "type": "string", "description": "Side of the start line" },
                    "review_id":  { "type": "number", "description": "Attach to an existing review (optional)" },
                    "suggestion": { "type": "string", "description": "Suggested replacement text (optional)" }
                },
                "required": ["owner", "repo", "number", "path", "body"]
            }
        },
        {
            "name": "apply_suggestion",
            "description": "Apply a review comment's suggestion, committing it to the PR head branch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":     { "type": "string", "description": "Repository owner" },
                    "repo":      { "type": "string", "description": "Repository name" },
                    "number":    { "type": "number", "description": "Pull request number" },
                    "comment_id":{ "type": "number", "description": "Review comment id carrying the suggestion" }
                },
                "required": ["owner", "repo", "number", "comment_id"]
            }
        },
        // ── CI/CD pipelines ────────────────────────────────
        {
            "name": "list_pipelines",
            "description": "List CI pipelines for a repository.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":   { "type": "string", "description": "Repository owner" },
                    "repo":    { "type": "string", "description": "Repository name" },
                    "page":    { "type": "number", "description": "Page number (optional)" },
                    "per_page":{ "type": "number", "description": "Page size (optional)" }
                },
                "required": ["owner", "repo"]
            }
        },
        {
            "name": "get_pipeline",
            "description": "Get a single CI pipeline by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "id":    { "type": "number", "description": "Pipeline id" }
                },
                "required": ["owner", "repo", "id"]
            }
        },
        {
            "name": "retry_pipeline",
            "description": "Retry a CI pipeline (re-run failed jobs).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "id":    { "type": "number", "description": "Pipeline id" }
                },
                "required": ["owner", "repo", "id"]
            }
        },
        {
            "name": "cancel_pipeline",
            "description": "Cancel a running CI pipeline.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "id":    { "type": "number", "description": "Pipeline id" }
                },
                "required": ["owner", "repo", "id"]
            }
        },
        {
            "name": "get_ci_job",
            "description": "Get a single CI job within a pipeline.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":  { "type": "string", "description": "Repository owner" },
                    "repo":   { "type": "string", "description": "Repository name" },
                    "id":     { "type": "number", "description": "Pipeline id" },
                    "job_id": { "type": "number", "description": "Job id" }
                },
                "required": ["owner", "repo", "id", "job_id"]
            }
        },
        // ── Search ─────────────────────────────────────────
        {
            "name": "search",
            "description": "Global search across repos, issues and wiki.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "q":       { "type": "string", "description": "Search query" },
                    "type":    { "type": "string", "description": "all / repos / issues / wiki (default all)" },
                    "page":    { "type": "number", "description": "Page number (optional)" },
                    "per_page":{ "type": "number", "description": "Page size (optional)" }
                },
                "required": ["q"]
            }
        },
        // ── AI agent endpoints ─────────────────────────────
        {
            "name": "ai_repo_summary",
            "description": "Agent-oriented repository summary.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" }
                },
                "required": ["owner", "repo"]
            }
        },
        {
            "name": "ai_list_issues",
            "description": "Agent-oriented compact list of issues.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "state": { "type": "string", "description": "open / closed / all (default open)" },
                    "limit": { "type": "number", "description": "Max results (default 20)" }
                },
                "required": ["owner", "repo"]
            }
        },
        {
            "name": "ai_list_prs",
            "description": "Agent-oriented compact list of pull requests.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "state": { "type": "string", "description": "open / closed / all (default open)" },
                    "limit": { "type": "number", "description": "Max results (default 20)" }
                },
                "required": ["owner", "repo"]
            }
        },
        {
            "name": "ai_repo_tree",
            "description": "Agent-oriented repository file tree.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "ref":   { "type": "string", "description": "Git ref (optional)" },
                    "path":  { "type": "string", "description": "Subtree path (optional)" }
                },
                "required": ["owner", "repo"]
            }
        },
        {
            "name": "ai_search_code",
            "description": "Agent-oriented full-text code search within a repository (requires the repo to be indexed).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "q":     { "type": "string", "description": "Code search query" },
                    "ref":   { "type": "string", "description": "Git ref (optional)" },
                    "limit": { "type": "number", "description": "Max results (default 20, max 100)" }
                },
                "required": ["owner", "repo", "q"]
            }
        }
    ]);

    make_success(req.id.clone(), serde_json::json!({ "tools": tools }))
}

// ── public: call tool ─────────────────────────────────────

/// Signature shared by every tool handler: arguments in, response text out.
type ToolHandler = fn(&AppState, &Value) -> String;

/// The table [`call_tool`] dispatches through — and the machine-readable answer
/// to "which tools does this server actually implement?".
///
/// It is data rather than `match` arms on purpose: arms are invisible at
/// runtime, so nothing could compare the surface advertised by [`list_tools`]
/// with the surface that answers. Both drift directions are silent —
/// advertised-but-undispatched (the agent sees a tool, every call answers
/// `-32601`) and dispatched-but-unadvertised (implemented and unreachable).
/// With the dispatcher as a table, `advertised_tools_match_the_dispatch_table`
/// compares the two sets instead of re-listing the names by hand.
const TOOL_DISPATCH: &[(&str, ToolHandler)] = &[
    // read
    ("list_repos", tool_list_repos),
    ("read_file", tool_read_file),
    ("read_dir", tool_read_dir),
    ("get_issue", tool_get_issue),
    ("get_pr", tool_get_pr),
    ("get_pr_diff", tool_get_pr_diff),
    // issues write
    ("create_issue", tool_create_issue),
    ("update_issue", tool_update_issue),
    ("comment_issue", tool_comment_issue),
    ("set_issue_labels", tool_set_issue_labels),
    // pulls write
    ("create_pr", tool_create_pr),
    ("merge_pr", tool_merge_pr),
    ("request_reviewers", tool_request_reviewers),
    // reviews write
    ("create_review", tool_create_review),
    ("create_review_comment", tool_create_review_comment),
    ("apply_suggestion", tool_apply_suggestion),
    // CI
    ("list_pipelines", tool_list_pipelines),
    ("get_pipeline", tool_get_pipeline),
    ("retry_pipeline", tool_retry_pipeline),
    ("cancel_pipeline", tool_cancel_pipeline),
    ("get_ci_job", tool_get_ci_job),
    // search
    ("search", tool_search),
    // AI
    ("ai_repo_summary", tool_ai_repo_summary),
    ("ai_list_issues", tool_ai_list_issues),
    ("ai_list_prs", tool_ai_list_prs),
    ("ai_repo_tree", tool_ai_repo_tree),
    ("ai_search_code", tool_ai_search_code),
];

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

    let result = match TOOL_DISPATCH.iter().find(|(tool, _)| *tool == name) {
        Some((_, handler)) => handler(state, &args),
        None => {
            return make_error(req.id.clone(), -32601, &format!("unknown tool: {}", name));
        }
    };

    let content = serde_json::json!({
        "content": [ { "type": "text", "text": result } ]
    });
    make_success(req.id.clone(), content)
}

// ── helpers ───────────────────────────────────────────────

/// Extract a non-empty string argument.
fn arg_str<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Extract `owner` + `repo` (both required, non-empty).
fn owner_repo(args: &Value) -> Result<(&str, &str), String> {
    let owner = arg_str(args, "owner");
    let repo = arg_str(args, "repo");
    if owner.is_empty() || repo.is_empty() {
        return Err("Error: owner and repo are required".into());
    }
    Ok((owner, repo))
}

/// Extract `owner` + `repo` + a required positive integer path parameter.
fn owner_repo_i64<'a>(args: &'a Value, key: &str) -> Result<(&'a str, &'a str, i64), String> {
    let (owner, repo) = owner_repo(args)?;
    let n = args.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
    if n == 0 {
        return Err(format!("Error: owner, repo and {} are required", key));
    }
    Ok((owner, repo, n))
}

/// Build a JSON object body from the keys present in `args`, ignoring the rest.
/// Keeps path parameters (owner/repo/number) out of the request body and
/// preserves explicit `null`s (needed for clear-on-null fields).
fn body_from(args: &Value, keys: &[&str]) -> Value {
    let mut map = serde_json::Map::new();
    for key in keys {
        if let Some(v) = args.get(*key) {
            map.insert((*key).to_string(), v.clone());
        }
    }
    Value::Object(map)
}

/// Append `key=value` (URL-encoded value) to a query-string accumulator.
fn push_query(qs: &mut Vec<String>, key: &str, val: &str) {
    qs.push(format!("{}={}", key, urlencoding::encode(val)));
}

/// Run a synchronous ForgeKeep read/write returning the raw response text.
fn run(fut: impl std::future::Future<Output = crate::Result<String>>) -> String {
    match tokio::runtime::Handle::current().block_on(fut) {
        Ok(text) => text,
        Err(e) => format!("Error: {}", e),
    }
}

// ── read implementations ──────────────────────────────────

fn tool_list_repos(state: &AppState, _args: &Value) -> String {
    let client = crate::client::ApiClient::new(state);
    match tokio::runtime::Handle::current().block_on(client.get::<Value>("/repos")) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|e| e.to_string()),
        Err(e) => format!("Error: {}", e),
    }
}

/// Percent-encode a repository path for the `{*path}` segment of a content
/// route: every segment is escaped, the separators stay separators.
fn encode_repo_path(path: &str) -> String {
    path.split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// `ref=…`, but only when a ref was actually named.
///
/// An unnamed ref used to be interpolated anyway, and `?ref=` is not an absent
/// parameter to the server — it is a ref whose name is the empty string, which
/// resolves to neither a branch nor a tag. Omitting it is what makes the
/// schema's "defaults to the default branch" true, since that is what the
/// server does with no `ref` at all.
fn ref_param(ref_: &str) -> Option<String> {
    (!ref_.is_empty()).then(|| format!("ref={}", urlencoding::encode(ref_)))
}

/// Join query parameters onto a path, leaving the path bare when there are none.
fn with_query(path: String, params: &[Option<String>]) -> String {
    let query = params
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    }
}

/// The API path `read_file` reads a file through.
///
/// `/contents/{path}` is mounted for `POST` and `DELETE` only — writing a file
/// and deleting one. Reading is `/blob/{path}`, so every `read_file` call used
/// to be answered by the router, not by the repository (card_66aa21756448).
pub(crate) fn read_file_path(owner: &str, repo: &str, path: &str, ref_: &str) -> String {
    with_query(
        format!("/repos/{owner}/{repo}/blob/{}", encode_repo_path(path)),
        &[ref_param(ref_)],
    )
}

/// The API path `read_dir` lists a directory through.
///
/// The ref is a query parameter, not a path segment: `/tree/{ref}` is not a
/// route this server has ever mounted.
fn read_dir_path(owner: &str, repo: &str, path: &str, ref_: &str) -> String {
    let sub_path =
        (!path.is_empty()).then(|| format!("path={}", urlencoding::encode(path).into_owned()));
    with_query(
        format!("/repos/{owner}/{repo}/tree"),
        &[ref_param(ref_), sub_path],
    )
}

fn tool_read_file(state: &AppState, args: &Value) -> String {
    let owner = arg_str(args, "owner");
    let repo = arg_str(args, "repo");
    let path = arg_str(args, "path");
    let ref_ = arg_str(args, "ref");

    if owner.is_empty() || repo.is_empty() || path.is_empty() {
        return "Error: owner, repo and path are required".into();
    }

    let api_path = read_file_path(owner, repo, path, ref_);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_read_dir(state: &AppState, args: &Value) -> String {
    let owner = arg_str(args, "owner");
    let repo = arg_str(args, "repo");
    let path = arg_str(args, "path");
    let ref_ = arg_str(args, "ref");

    if owner.is_empty() || repo.is_empty() {
        return "Error: owner and repo are required".into();
    }

    let api_path = read_dir_path(owner, repo, path, ref_);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_get_issue(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = format!("/repos/{}/{}/issues/{}", owner, repo, number);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_get_pr(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = format!("/repos/{}/{}/pulls/{}", owner, repo, number);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_get_pr_diff(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = format!("/repos/{}/{}/pulls/{}/diff", owner, repo, number);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

// ── issues write ──────────────────────────────────────────

fn tool_create_issue(state: &AppState, args: &Value) -> String {
    let (owner, repo) = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "title").is_empty() {
        return "Error: title is required".into();
    }
    let api_path = format!("/repos/{}/{}/issues", owner, repo);
    let body = body_from(args, &["title", "body", "labels", "milestone_id"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_update_issue(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = format!("/repos/{}/{}/issues/{}", owner, repo, number);
    let body = body_from(
        args,
        &[
            "title",
            "body",
            "state",
            "labels",
            "assignee_id",
            "milestone_id",
        ],
    );
    let client = crate::client::ApiClient::new(state);
    run(async move { client.patch_raw(&api_path, &body).await })
}

fn tool_comment_issue(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "body").is_empty() {
        return "Error: body is required".into();
    }
    let api_path = format!("/repos/{}/{}/issues/{}/comments", owner, repo, number);
    let body = body_from(args, &["body"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_set_issue_labels(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !args.get("labels").map(|v| v.is_array()).unwrap_or(false) {
        return "Error: labels must be an array".into();
    }
    let api_path = format!("/repos/{}/{}/issues/{}", owner, repo, number);
    let body = body_from(args, &["labels"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.patch_raw(&api_path, &body).await })
}

// ── pulls write ───────────────────────────────────────────

fn tool_create_pr(state: &AppState, args: &Value) -> String {
    let (owner, repo) = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "title").is_empty()
        || arg_str(args, "head").is_empty()
        || arg_str(args, "base").is_empty()
    {
        return "Error: title, head and base are required".into();
    }
    let api_path = format!("/repos/{}/{}/pulls", owner, repo);
    let body = body_from(args, &["title", "body", "head", "base", "draft"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_merge_pr(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "strategy").is_empty() {
        return "Error: strategy is required (merge / squash / rebase)".into();
    }
    let api_path = format!("/repos/{}/{}/pulls/{}/merge", owner, repo, number);
    let body = body_from(args, &["strategy"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_request_reviewers(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "username").is_empty() {
        return "Error: username is required".into();
    }
    let api_path = format!("/repos/{}/{}/pulls/{}/reviewers", owner, repo, number);
    let body = body_from(args, &["username"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

// ── reviews write ─────────────────────────────────────────

fn tool_create_review(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "action").is_empty() {
        return "Error: action is required (comment / approve / request_changes / dismiss)".into();
    }
    let api_path = format!("/repos/{}/{}/pulls/{}/reviews", owner, repo, number);
    let body = body_from(args, &["action", "body", "commit_id"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_create_review_comment(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "path").is_empty() || arg_str(args, "body").is_empty() {
        return "Error: path and body are required".into();
    }
    let api_path = format!("/repos/{}/{}/pulls/{}/comments", owner, repo, number);
    let body = body_from(
        args,
        &[
            "review_id",
            "path",
            "line",
            "start_line",
            "side",
            "start_side",
            "body",
            "suggestion",
        ],
    );
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_apply_suggestion(state: &AppState, args: &Value) -> String {
    let (owner, repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let comment_id = args.get("comment_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if comment_id == 0 {
        return "Error: comment_id is required".into();
    }
    let api_path = format!(
        "/repos/{}/{}/pulls/{}/comments/{}/suggestion/apply",
        owner, repo, number, comment_id
    );
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_empty(&api_path).await })
}

// ── CI/CD ─────────────────────────────────────────────────

fn tool_list_pipelines(state: &AppState, args: &Value) -> String {
    let (owner, repo) = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut qs: Vec<String> = Vec::new();
    if let Some(page) = args.get("page").and_then(|v| v.as_i64()) {
        qs.push(format!("page={}", page));
    }
    if let Some(per_page) = args.get("per_page").and_then(|v| v.as_i64()) {
        qs.push(format!("per_page={}", per_page));
    }
    let query = if qs.is_empty() {
        String::new()
    } else {
        format!("?{}", qs.join("&"))
    };
    let api_path = format!("/repos/{}/{}/pipelines{}", owner, repo, query);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_get_pipeline(state: &AppState, args: &Value) -> String {
    let (owner, repo, id) = match owner_repo_i64(args, "id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = format!("/repos/{}/{}/pipelines/{}", owner, repo, id);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_retry_pipeline(state: &AppState, args: &Value) -> String {
    let (owner, repo, id) = match owner_repo_i64(args, "id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = format!("/repos/{}/{}/pipelines/{}/retry", owner, repo, id);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_empty(&api_path).await })
}

fn tool_cancel_pipeline(state: &AppState, args: &Value) -> String {
    let (owner, repo, id) = match owner_repo_i64(args, "id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = format!("/repos/{}/{}/pipelines/{}/cancel", owner, repo, id);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_empty(&api_path).await })
}

fn tool_get_ci_job(state: &AppState, args: &Value) -> String {
    let (owner, repo, id) = match owner_repo_i64(args, "id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let job_id = args.get("job_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if job_id == 0 {
        return "Error: job_id is required".into();
    }
    let api_path = format!("/repos/{}/{}/pipelines/{}/jobs/{}", owner, repo, id, job_id);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

// ── search ────────────────────────────────────────────────

fn tool_search(state: &AppState, args: &Value) -> String {
    let q = arg_str(args, "q");
    if q.is_empty() {
        return "Error: q is required".into();
    }
    let mut qs: Vec<String> = Vec::new();
    push_query(&mut qs, "q", q);
    let type_ = arg_str(args, "type");
    if !type_.is_empty() {
        push_query(&mut qs, "type", type_);
    }
    if let Some(page) = args.get("page").and_then(|v| v.as_i64()) {
        qs.push(format!("page={}", page));
    }
    if let Some(per_page) = args.get("per_page").and_then(|v| v.as_i64()) {
        qs.push(format!("per_page={}", per_page));
    }
    let api_path = format!("/search?{}", qs.join("&"));
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

// ── AI agent endpoints ────────────────────────────────────

fn tool_ai_repo_summary(state: &AppState, args: &Value) -> String {
    let (owner, repo) = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = format!("/ai/repos/{}/{}/summary", owner, repo);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_ai_list_issues(state: &AppState, args: &Value) -> String {
    let (owner, repo) = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut qs: Vec<String> = Vec::new();
    let st = arg_str(args, "state");
    if !st.is_empty() {
        push_query(&mut qs, "state", st);
    }
    if let Some(limit) = args.get("limit").and_then(|v| v.as_i64()) {
        qs.push(format!("limit={}", limit));
    }
    let query = if qs.is_empty() {
        String::new()
    } else {
        format!("?{}", qs.join("&"))
    };
    let api_path = format!("/ai/repos/{}/{}/issues{}", owner, repo, query);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_ai_list_prs(state: &AppState, args: &Value) -> String {
    let (owner, repo) = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut qs: Vec<String> = Vec::new();
    let st = arg_str(args, "state");
    if !st.is_empty() {
        push_query(&mut qs, "state", st);
    }
    if let Some(limit) = args.get("limit").and_then(|v| v.as_i64()) {
        qs.push(format!("limit={}", limit));
    }
    let query = if qs.is_empty() {
        String::new()
    } else {
        format!("?{}", qs.join("&"))
    };
    let api_path = format!("/ai/repos/{}/{}/prs{}", owner, repo, query);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_ai_repo_tree(state: &AppState, args: &Value) -> String {
    let (owner, repo) = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut qs: Vec<String> = Vec::new();
    let ref_ = arg_str(args, "ref");
    if !ref_.is_empty() {
        push_query(&mut qs, "ref", ref_);
    }
    let path = arg_str(args, "path");
    if !path.is_empty() {
        push_query(&mut qs, "path", path);
    }
    let query = if qs.is_empty() {
        String::new()
    } else {
        format!("?{}", qs.join("&"))
    };
    let api_path = format!("/ai/repos/{}/{}/tree{}", owner, repo, query);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

fn tool_ai_search_code(state: &AppState, args: &Value) -> String {
    let (owner, repo) = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let q = arg_str(args, "q");
    if q.is_empty() {
        return "Error: q is required".into();
    }
    let mut qs: Vec<String> = Vec::new();
    push_query(&mut qs, "q", q);
    let ref_ = arg_str(args, "ref");
    if !ref_.is_empty() {
        push_query(&mut qs, "ref", ref_);
    }
    if let Some(limit) = args.get("limit").and_then(|v| v.as_i64()) {
        qs.push(format!("limit={}", limit));
    }
    let api_path = format!("/ai/repos/{}/{}/search/code?{}", owner, repo, qs.join("&"));
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&api_path).await })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn state() -> AppState {
        AppState::new("http://localhost:8080".into(), String::new())
    }

    fn req(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Value::from(1),
            method: method.into(),
            params: Some(params),
        }
    }

    fn call_text(name: &str, arguments: Value) -> String {
        let r = call_tool(
            &state(),
            &req(
                "tools/call",
                serde_json::json!({ "name": name, "arguments": arguments }),
            ),
        );
        r.result
            .and_then(|v| v.get("content").cloned())
            .and_then(|c| c.get(0).cloned())
            .and_then(|c| c.get("text").and_then(|t| t.as_str().map(String::from)))
            .unwrap_or_default()
    }

    /// Tool names as an MCP client learns them, straight from the `tools/list` reply.
    fn advertised_tool_names() -> Vec<String> {
        let resp = list_tools(&state(), &req("tools/list", Value::Null));
        resp.result
            .unwrap()
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    /// The gate this crate's agent surface hangs on: what `tools/list` advertises
    /// and what `call_tool` dispatches must be the *same* set — compared against
    /// each other, never against a third list written by hand (such a list is
    /// exactly what a new tool forgets to update, leaving both drift directions
    /// green).
    #[test]
    fn advertised_tools_match_the_dispatch_table() {
        let advertised = advertised_tool_names();
        let dispatched: Vec<&str> = TOOL_DISPATCH.iter().map(|(name, _)| *name).collect();

        // Two empty sets compare equal; a vacuous pass would be worse than no gate.
        assert!(!dispatched.is_empty(), "dispatch table is empty");
        assert!(!advertised.is_empty(), "tools/list advertises nothing");

        let advertised_set: BTreeSet<&str> = advertised.iter().map(String::as_str).collect();
        let dispatched_set: BTreeSet<&str> = dispatched.iter().copied().collect();

        // Duplicates would let a set comparison pass while a name is served twice
        // (in `TOOL_DISPATCH` the second entry is dead — `find` stops at the first).
        assert_eq!(
            advertised_set.len(),
            advertised.len(),
            "duplicate tool names in tools/list"
        );
        assert_eq!(
            dispatched_set.len(),
            dispatched.len(),
            "duplicate tool names in TOOL_DISPATCH"
        );

        let undispatched: Vec<&&str> = advertised_set.difference(&dispatched_set).collect();
        assert!(
            undispatched.is_empty(),
            "advertised by tools/list but absent from TOOL_DISPATCH — an agent sees these \
             and every call answers -32601: {undispatched:?}"
        );

        let unadvertised: Vec<&&str> = dispatched_set.difference(&advertised_set).collect();
        assert!(
            unadvertised.is_empty(),
            "dispatched by call_tool but never advertised — implemented and unreachable: \
             {unadvertised:?}"
        );
    }

    /// card_66aa21756448: the content-read tools addressed routes this server
    /// does not mount — `GET /contents/{path}` (mounted for POST/DELETE only)
    /// and `/tree/{ref}` (the ref is a query parameter). Neither tool could
    /// ever answer, so both paths are pinned here against the routes in
    /// `rg-http/src/routes.rs`; the live-server proof is
    /// `rg-http/tests/integration/mcp_content_tools_tests.rs`.
    #[test]
    fn content_read_tools_address_routes_the_server_mounts() {
        assert_eq!(
            read_file_path("acme", "widgets", "src/main.rs", ""),
            "/repos/acme/widgets/blob/src/main.rs"
        );
        assert_eq!(
            read_file_path("acme", "widgets", "src/main.rs", "master"),
            "/repos/acme/widgets/blob/src/main.rs?ref=master"
        );
        assert_eq!(
            read_dir_path("acme", "widgets", "", ""),
            "/repos/acme/widgets/tree"
        );
        assert_eq!(
            read_dir_path("acme", "widgets", "src/api", "master"),
            "/repos/acme/widgets/tree?ref=master&path=src%2Fapi"
        );
    }

    /// A path segment carrying `?`, `#` or a space is a file name, not query
    /// syntax — but the separators between segments must survive, since the
    /// route matches them as a wildcard.
    #[test]
    fn a_file_path_is_escaped_per_segment_not_wholesale() {
        assert_eq!(
            read_file_path("acme", "widgets", "docs/read me?.md", "feature/new"),
            "/repos/acme/widgets/blob/docs/read%20me%3F.md?ref=feature%2Fnew"
        );
    }

    #[test]
    fn unknown_tool_is_rejected() {
        let resp = call_tool(
            &state(),
            &req(
                "tools/call",
                serde_json::json!({ "name": "does_not_exist" }),
            ),
        );
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn write_tools_validate_required_args_before_any_network_call() {
        // Each returns a validation error string without touching the network,
        // so these run with no live backend.
        assert!(call_text(
            "create_issue",
            serde_json::json!({ "owner": "o", "repo": "r" })
        )
        .starts_with("Error:"));
        assert!(call_text(
            "comment_issue",
            serde_json::json!({ "owner": "o", "repo": "r", "number": 1 })
        )
        .starts_with("Error:"));
        assert!(call_text(
            "merge_pr",
            serde_json::json!({ "owner": "o", "repo": "r", "number": 1 })
        )
        .starts_with("Error:"));
        assert!(call_text(
            "create_review",
            serde_json::json!({ "owner": "o", "repo": "r", "number": 1 })
        )
        .starts_with("Error:"));
        assert!(call_text(
            "create_pr",
            serde_json::json!({ "owner": "o", "repo": "r", "title": "t" })
        )
        .starts_with("Error:"));
        assert!(call_text(
            "get_ci_job",
            serde_json::json!({ "owner": "o", "repo": "r", "id": 1 })
        )
        .starts_with("Error:"));
        assert!(call_text("search", serde_json::json!({})).starts_with("Error:"));
        assert!(call_text(
            "set_issue_labels",
            serde_json::json!({ "owner": "o", "repo": "r", "number": 1 })
        )
        .starts_with("Error:"));
    }

    #[test]
    fn body_from_keeps_whitelist_and_preserves_null() {
        let args = serde_json::json!({
            "owner": "o", "repo": "r", "number": 5,
            "title": "t", "assignee_id": null, "labels": ["bug"]
        });
        let body = body_from(&args, &["title", "assignee_id", "labels", "milestone_id"]);
        // Path params never leak into the body.
        assert!(body.get("owner").is_none());
        assert!(body.get("number").is_none());
        // Present keys copied, explicit null preserved, absent key omitted.
        assert_eq!(body["title"], serde_json::json!("t"));
        assert_eq!(body["assignee_id"], Value::Null);
        assert_eq!(body["labels"], serde_json::json!(["bug"]));
        assert!(body.get("milestone_id").is_none());
    }
}
