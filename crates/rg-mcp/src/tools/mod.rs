//! MCP **Tools** – dispatched from [`crate::dispatch`].
//!
//! Each tool:
//! 1. parses `req.params` → arguments JSON
//! 2. calls Plombir Git REST API
//! 3. returns `JsonRpcResponse` with `ToolCallResult`
//!
//! Every handler is a thin wrapper around one REST call (the `tool_get_pr`
//! pattern): parse args → build path/body → `ApiClient` → return text. Write
//! tools construct an explicit JSON body from a whitelist of keys so path
//! parameters (owner/repo/number) never leak into the request body.

use super::protocol::*;
use super::ApiRoute;
use super::AppState;
use serde_json::Value;

// ── public: list tools ─────────────────────────────────

pub fn list_tools(_state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let tools = serde_json::json!([
        // ── Read: repos & content ──────────────────────────
        {
            "name": "list_repos",
            "description": "List Git repositories of an owner, or the public repositories of the instance when no owner is named.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "User or organization whose repositories to list; omitted, the public explore listing is returned" }
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
        {
            "name": "list_reviews",
            "description": "List the reviews submitted on a pull request — who approved, who requested changes, and on which commit.",
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
            "name": "list_review_comments",
            "description": "List the inline review comments on a pull request, replies included (`reply_to_id` names the comment a reply answers).",
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
        // ── Write: content ─────────────────────────────────
        {
            "name": "write_file",
            "description": "Create or update a file and commit it to a branch. A branch that does not exist yet is created from the repository's default branch, so this is also how a change starts its own branch. Updating an existing file needs its current blob `sha` (from read_file).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":   { "type": "string", "description": "Repository owner" },
                    "repo":    { "type": "string", "description": "Repository name" },
                    "path":    { "type": "string", "description": "File path, e.g. 'src/main.rs'" },
                    "content": { "type": "string", "description": "The whole new file content (UTF-8 text)" },
                    "message": { "type": "string", "description": "Commit message" },
                    "branch":  { "type": "string", "description": "Branch to commit to. Defaults to the default branch." },
                    "sha":     { "type": "string", "description": "Blob SHA of the file being replaced; omit to create a new file" }
                },
                "required": ["owner", "repo", "path", "content", "message"]
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
            "description": "Submit a review on a pull request (comment / approve / request_changes).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner":     { "type": "string", "description": "Repository owner" },
                    "repo":      { "type": "string", "description": "Repository name" },
                    "number":    { "type": "number", "description": "Pull request number" },
                    "action":    { "type": "string", "description": "comment / approve / request_changes" },
                    "body":      { "type": "string", "description": "Review body (Markdown)" },
                    "commit_id": { "type": "string", "description": "Commit SHA the review pins to (optional)" }
                },
                "required": ["owner", "repo", "number", "action"]
            }
        },
        {
            "name": "create_review_comment",
            "description": "Create an inline review comment on a pull request diff (optionally with a suggestion), or reply in an existing comment's thread with `reply_to_id`.",
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
                    "suggestion": { "type": "string", "description": "Suggested replacement text (optional)" },
                    "reply_to_id":{ "type": "number", "description": "Id of the comment this one replies to (optional); `path` repeats that comment's path" },
                    "commit_id":  { "type": "string", "description": "Commit SHA the comment pins to (optional, defaults to the review's)" }
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
        {
            "name": "get_commit_status",
            "description": "Get the combined status that external CI systems reported for a commit (success / pending / failure, with each reported status). The instance's own CI is read with list_pipelines.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo":  { "type": "string", "description": "Repository name" },
                    "sha":   { "type": "string", "description": "Commit SHA, e.g. a pull request's head_sha" }
                },
                "required": ["owner", "repo", "sha"]
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

/// A route the API mounts, as a tool row declares it.
const fn route(method: &'static str, path: &'static str) -> ApiRoute {
    ApiRoute { method, path }
}

/// The table [`call_tool`] dispatches through — and the machine-readable answer
/// to "which tools does this server actually implement, and which API routes
/// does each one call?".
///
/// It is data rather than `match` arms on purpose: arms are invisible at
/// runtime, so nothing could compare the surface advertised by [`list_tools`]
/// with the surface that answers. Both drift directions are silent —
/// advertised-but-undispatched (the agent sees a tool, every call answers
/// `-32601`) and dispatched-but-unadvertised (implemented and unreachable).
/// With the dispatcher as a table, `advertised_tools_match_the_dispatch_table`
/// compares the two sets instead of re-listing the names by hand.
///
/// The third column is the tool's whole reach into the API. The server that
/// embeds these tools refuses an in-process call whose matched route is not on
/// it (card_5b6ce4ccc0a7): a token confined to `retry_pipeline` used to reach
/// `POST …/pulls/{number}/ci-approval` by naming its repository
/// `app/pulls/12/ci-approval?`. Escaping the arguments closed that path; this
/// column is what stops the next argument nobody thought to escape.
/// `rg-http`'s `every_mcp_tool_call_stays_on_the_routes_it_declares` drives
/// every tool through that binding, so a row that forgets a route is a red
/// test, not a tool that quietly stops working.
const TOOL_DISPATCH: &[(&str, ToolHandler, &[ApiRoute])] = &[
    // read
    (
        "list_repos",
        tool_list_repos,
        &[
            route("GET", "/api/v1/repos/explore"),
            route("GET", "/api/v1/repos/{owner}"),
        ],
    ),
    (
        "read_file",
        tool_read_file,
        &[route("GET", "/api/v1/repos/{owner}/{name}/blob/{*path}")],
    ),
    (
        "read_dir",
        tool_read_dir,
        &[route("GET", "/api/v1/repos/{owner}/{name}/tree")],
    ),
    (
        "get_issue",
        tool_get_issue,
        &[route("GET", "/api/v1/repos/{owner}/{name}/issues/{number}")],
    ),
    (
        "get_pr",
        tool_get_pr,
        &[route("GET", "/api/v1/repos/{owner}/{name}/pulls/{number}")],
    ),
    (
        "get_pr_diff",
        tool_get_pr_diff,
        &[route(
            "GET",
            "/api/v1/repos/{owner}/{name}/pulls/{number}/diff",
        )],
    ),
    (
        "list_reviews",
        tool_list_reviews,
        &[route(
            "GET",
            "/api/v1/repos/{owner}/{name}/pulls/{number}/reviews",
        )],
    ),
    (
        "list_review_comments",
        tool_list_review_comments,
        &[route(
            "GET",
            "/api/v1/repos/{owner}/{name}/pulls/{number}/comments",
        )],
    ),
    // content write
    (
        "write_file",
        tool_write_file,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/contents/{*path}",
        )],
    ),
    // issues write
    (
        "create_issue",
        tool_create_issue,
        &[route("POST", "/api/v1/repos/{owner}/{name}/issues")],
    ),
    (
        "update_issue",
        tool_update_issue,
        &[route(
            "PATCH",
            "/api/v1/repos/{owner}/{name}/issues/{number}",
        )],
    ),
    (
        "comment_issue",
        tool_comment_issue,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/issues/{number}/comments",
        )],
    ),
    (
        "set_issue_labels",
        tool_set_issue_labels,
        &[route(
            "PATCH",
            "/api/v1/repos/{owner}/{name}/issues/{number}",
        )],
    ),
    // pulls write
    (
        "create_pr",
        tool_create_pr,
        &[route("POST", "/api/v1/repos/{owner}/{name}/pulls")],
    ),
    (
        "merge_pr",
        tool_merge_pr,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/pulls/{number}/merge",
        )],
    ),
    (
        "request_reviewers",
        tool_request_reviewers,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/pulls/{number}/reviewers",
        )],
    ),
    // reviews write
    (
        "create_review",
        tool_create_review,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/pulls/{number}/reviews",
        )],
    ),
    (
        "create_review_comment",
        tool_create_review_comment,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/pulls/{number}/comments",
        )],
    ),
    (
        "apply_suggestion",
        tool_apply_suggestion,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply",
        )],
    ),
    // CI
    (
        "list_pipelines",
        tool_list_pipelines,
        &[route("GET", "/api/v1/repos/{owner}/{name}/pipelines")],
    ),
    (
        "get_pipeline",
        tool_get_pipeline,
        &[route("GET", "/api/v1/repos/{owner}/{name}/pipelines/{id}")],
    ),
    (
        "retry_pipeline",
        tool_retry_pipeline,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/pipelines/{id}/retry",
        )],
    ),
    (
        "cancel_pipeline",
        tool_cancel_pipeline,
        &[route(
            "POST",
            "/api/v1/repos/{owner}/{name}/pipelines/{id}/cancel",
        )],
    ),
    (
        "get_ci_job",
        tool_get_ci_job,
        &[route(
            "GET",
            "/api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}",
        )],
    ),
    (
        "get_commit_status",
        tool_get_commit_status,
        &[route(
            "GET",
            "/api/v1/repos/{owner}/{name}/commits/{sha}/status",
        )],
    ),
    // search
    ("search", tool_search, &[route("GET", "/api/v1/search")]),
    // AI
    (
        "ai_repo_summary",
        tool_ai_repo_summary,
        &[route("GET", "/api/v1/ai/repos/{owner}/{name}/summary")],
    ),
    (
        "ai_list_issues",
        tool_ai_list_issues,
        &[route("GET", "/api/v1/ai/repos/{owner}/{name}/issues")],
    ),
    (
        "ai_list_prs",
        tool_ai_list_prs,
        &[route("GET", "/api/v1/ai/repos/{owner}/{name}/prs")],
    ),
    (
        "ai_repo_tree",
        tool_ai_repo_tree,
        &[route("GET", "/api/v1/ai/repos/{owner}/{name}/tree")],
    ),
    (
        "ai_search_code",
        tool_ai_search_code,
        &[route("GET", "/api/v1/ai/repos/{owner}/{name}/search/code")],
    ),
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

    let result = match TOOL_DISPATCH.iter().find(|(tool, _, _)| *tool == name) {
        Some((_, handler, _)) => handler(state, &args),
        None => {
            return make_error(req.id.clone(), -32601, &format!("unknown tool: {}", name));
        }
    };

    // Every handler reports a failure as text starting with `Error:` — the
    // backend's status and body included. `isError` is how MCP tells the agent
    // the call failed rather than returned that text as data.
    let is_error = result.starts_with("Error:");
    let content = serde_json::json!({
        "content": [ { "type": "text", "text": result } ],
        "isError": is_error,
    });
    make_success(req.id.clone(), content)
}

/// Every tool this server implements, by name — the dispatch table's keys.
///
/// For callers that confine a credential to some of them: the server validates
/// a token's allowed tools against this list, so a typo is refused when the
/// token is minted rather than silently matching nothing.
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    TOOL_DISPATCH.iter().map(|(name, _, _)| *name)
}

/// The API routes `tool` calls — all of them, and nothing else. `None` for a
/// tool this server does not implement.
pub fn tool_routes(tool: &str) -> Option<&'static [ApiRoute]> {
    TOOL_DISPATCH
        .iter()
        .find(|(name, _, _)| *name == tool)
        .map(|(_, _, routes)| *routes)
}

// ── helpers ───────────────────────────────────────────────

/// Extract a non-empty string argument.
fn arg_str<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// One path segment of an API route, built from a value an agent supplied.
///
/// Percent-encoded, so `/`, `?`, `#` and `%` stay inside the segment instead of
/// starting a new one, a query or a fragment (card_5b6ce4ccc0a7). `.` and `..`
/// are refused outright: escaping leaves them as they are, and the HTTP
/// backend's URL parser resolves them as directory steps — `…/blob/../../x`
/// would leave the route the tool named before the server ever saw it.
pub(crate) fn path_segment(key: &str, value: &str) -> Result<String, String> {
    if value == "." || value == ".." {
        return Err(format!("Error: {key} may not be '.' or '..'"));
    }
    Ok(urlencoding::encode(value).into_owned())
}

/// A repository an agent named, kept only in its escaped form.
///
/// The raw `owner` / `repo` strings never reach a `format!` that builds an API
/// path: every tool gets its path from [`RepoPath::api`] or [`RepoPath::ai`],
/// so there is no spelling of a repository-scoped path that skips the escaping.
pub(crate) struct RepoPath {
    owner: String,
    name: String,
}

impl RepoPath {
    /// Both parts required and non-empty, each escaped as one segment.
    pub(crate) fn new(owner: &str, name: &str) -> Result<Self, String> {
        if owner.is_empty() || name.is_empty() {
            return Err("Error: owner and repo are required".into());
        }
        Ok(Self {
            owner: path_segment("owner", owner)?,
            name: path_segment("repo", name)?,
        })
    }

    /// `/repos/{owner}/{name}` followed by `rest`, which the tool spells itself
    /// from literals and numbers.
    pub(crate) fn api(&self, rest: &str) -> String {
        format!("/repos/{}/{}{rest}", self.owner, self.name)
    }

    /// The same repository under the agent-oriented `/ai/repos` family.
    fn ai(&self, rest: &str) -> String {
        format!("/ai/repos/{}/{}{rest}", self.owner, self.name)
    }
}

/// Extract `owner` + `repo` (both required, non-empty).
fn owner_repo(args: &Value) -> Result<RepoPath, String> {
    RepoPath::new(arg_str(args, "owner"), arg_str(args, "repo"))
}

/// Extract `owner` + `repo` + a required positive integer path parameter.
fn owner_repo_i64(args: &Value, key: &str) -> Result<(RepoPath, i64), String> {
    let repo = owner_repo(args)?;
    let n = args.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
    if n == 0 {
        return Err(format!("Error: owner, repo and {} are required", key));
    }
    Ok((repo, n))
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

/// Run a synchronous Plombir Git read/write returning the raw response text.
fn run(fut: impl std::future::Future<Output = crate::Result<String>>) -> String {
    match tokio::runtime::Handle::current().block_on(fut) {
        Ok(text) => text,
        Err(e) => format!("Error: {}", e),
    }
}

/// `GET path`, reported as text.
fn get(state: &AppState, path: String) -> String {
    let client = crate::client::ApiClient::new(state);
    run(async move { client.get_raw(&path).await })
}

// ── read implementations ──────────────────────────────────

/// The API path `list_repos` lists through, with and without an owner.
///
/// `GET /repos` is not a route: `/repos` is mounted for `POST` alone — creating
/// a repository — so every call this tool ever made was answered by the router
/// with `405` and never by the server (card_3d05153aaca1, the same defect
/// card_66aa21756448 fixed in `read_file` and `read_dir`). The two listings that
/// do exist are an owner's repositories, filtered to what the caller may read,
/// and the public explore page — which is what "no owner given" can honestly
/// mean, since the server mounts no "everything I can reach" listing.
fn list_repos_path(owner: &str) -> Result<String, String> {
    if owner.is_empty() {
        Ok("/repos/explore".to_string())
    } else {
        Ok(format!("/repos/{}", path_segment("owner", owner)?))
    }
}

fn tool_list_repos(state: &AppState, args: &Value) -> String {
    let api_path = match list_repos_path(arg_str(args, "owner")) {
        Ok(path) => path,
        Err(e) => return e,
    };
    let client = crate::client::ApiClient::new(state);
    match tokio::runtime::Handle::current().block_on(client.get::<Value>(&api_path)) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|e| e.to_string()),
        Err(e) => format!("Error: {}", e),
    }
}

/// Percent-encode a repository path for the `{*path}` segment of a content
/// route: every segment is escaped, the separators stay separators, and a `.`
/// or `..` component — which no path inside a Git tree has — is refused, for
/// the reason [`path_segment`] gives.
fn encode_repo_path(path: &str) -> Result<String, String> {
    path.split('/')
        .map(|segment| path_segment("path", segment))
        .collect::<Result<Vec<_>, _>>()
        .map(|segments| segments.join("/"))
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
pub(crate) fn read_file_path(repo: &RepoPath, path: &str, ref_: &str) -> Result<String, String> {
    Ok(with_query(
        repo.api(&format!("/blob/{}", encode_repo_path(path)?)),
        &[ref_param(ref_)],
    ))
}

/// The API path `read_dir` lists a directory through.
///
/// The ref is a query parameter, not a path segment: `/tree/{ref}` is not a
/// route this server has ever mounted.
fn read_dir_path(repo: &RepoPath, path: &str, ref_: &str) -> String {
    let sub_path =
        (!path.is_empty()).then(|| format!("path={}", urlencoding::encode(path).into_owned()));
    with_query(repo.api("/tree"), &[ref_param(ref_), sub_path])
}

fn tool_read_file(state: &AppState, args: &Value) -> String {
    let path = arg_str(args, "path");
    let ref_ = arg_str(args, "ref");

    if arg_str(args, "owner").is_empty() || arg_str(args, "repo").is_empty() || path.is_empty() {
        return "Error: owner, repo and path are required".into();
    }
    let api_path = match owner_repo(args).and_then(|repo| read_file_path(&repo, path, ref_)) {
        Ok(path) => path,
        Err(e) => return e,
    };
    get(state, api_path)
}

fn tool_read_dir(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = read_dir_path(&repo, arg_str(args, "path"), arg_str(args, "ref"));
    get(state, api_path)
}

fn tool_get_issue(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    get(state, repo.api(&format!("/issues/{number}")))
}

fn tool_get_pr(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    get(state, repo.api(&format!("/pulls/{number}")))
}

fn tool_get_pr_diff(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    get(state, repo.api(&format!("/pulls/{number}/diff")))
}

fn tool_list_reviews(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    get(state, repo.api(&format!("/pulls/{number}/reviews")))
}

fn tool_list_review_comments(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    get(state, repo.api(&format!("/pulls/{number}/comments")))
}

// ── content write ─────────────────────────────────────────

/// The API path `write_file` commits a file through: `POST /contents/{path}`,
/// the write half of the route `read_file` must not address.
fn write_file_path(repo: &RepoPath, path: &str) -> Result<String, String> {
    Ok(repo.api(&format!("/contents/{}", encode_repo_path(path)?)))
}

fn tool_write_file(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = arg_str(args, "path");
    // Empty content is an empty file, not a missing argument.
    let has_content = args.get("content").is_some_and(Value::is_string);
    if path.is_empty() || !has_content || arg_str(args, "message").is_empty() {
        return "Error: path, content and message are required".into();
    }
    let api_path = match write_file_path(&repo, path) {
        Ok(path) => path,
        Err(e) => return e,
    };
    let body = body_from(args, &["branch", "content", "message", "sha"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

// ── issues write ──────────────────────────────────────────

fn tool_create_issue(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "title").is_empty() {
        return "Error: title is required".into();
    }
    let api_path = repo.api("/issues");
    let body = body_from(args, &["title", "body", "labels", "milestone_id"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_update_issue(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = repo.api(&format!("/issues/{number}"));
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
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "body").is_empty() {
        return "Error: body is required".into();
    }
    let api_path = repo.api(&format!("/issues/{number}/comments"));
    let body = body_from(args, &["body"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_set_issue_labels(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !args.get("labels").map(|v| v.is_array()).unwrap_or(false) {
        return "Error: labels must be an array".into();
    }
    let api_path = repo.api(&format!("/issues/{number}"));
    let body = body_from(args, &["labels"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.patch_raw(&api_path, &body).await })
}

// ── pulls write ───────────────────────────────────────────

fn tool_create_pr(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "title").is_empty()
        || arg_str(args, "head").is_empty()
        || arg_str(args, "base").is_empty()
    {
        return "Error: title, head and base are required".into();
    }
    let api_path = repo.api("/pulls");
    let body = body_from(args, &["title", "body", "head", "base", "draft"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_merge_pr(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "strategy").is_empty() {
        return "Error: strategy is required (merge / squash / rebase)".into();
    }
    let api_path = repo.api(&format!("/pulls/{number}/merge"));
    let body = body_from(args, &["strategy"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_request_reviewers(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "username").is_empty() {
        return "Error: username is required".into();
    }
    let api_path = repo.api(&format!("/pulls/{number}/reviewers"));
    let body = body_from(args, &["username"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

// ── reviews write ─────────────────────────────────────────

fn tool_create_review(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "action").is_empty() {
        return "Error: action is required (comment / approve / request_changes)".into();
    }
    let api_path = repo.api(&format!("/pulls/{number}/reviews"));
    let body = body_from(args, &["action", "body", "commit_id"]);
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_create_review_comment(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if arg_str(args, "path").is_empty() || arg_str(args, "body").is_empty() {
        return "Error: path and body are required".into();
    }
    let api_path = repo.api(&format!("/pulls/{number}/comments"));
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
            "reply_to_id",
            "commit_id",
        ],
    );
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_raw(&api_path, &body).await })
}

fn tool_apply_suggestion(state: &AppState, args: &Value) -> String {
    let (repo, number) = match owner_repo_i64(args, "number") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let comment_id = args.get("comment_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if comment_id == 0 {
        return "Error: comment_id is required".into();
    }
    let api_path = repo.api(&format!(
        "/pulls/{number}/comments/{comment_id}/suggestion/apply"
    ));
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_empty(&api_path).await })
}

// ── CI/CD ─────────────────────────────────────────────────

fn tool_list_pipelines(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
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
    get(state, repo.api(&format!("/pipelines{query}")))
}

fn tool_get_pipeline(state: &AppState, args: &Value) -> String {
    let (repo, id) = match owner_repo_i64(args, "id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    get(state, repo.api(&format!("/pipelines/{id}")))
}

fn tool_retry_pipeline(state: &AppState, args: &Value) -> String {
    let (repo, id) = match owner_repo_i64(args, "id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = repo.api(&format!("/pipelines/{id}/retry"));
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_empty(&api_path).await })
}

fn tool_cancel_pipeline(state: &AppState, args: &Value) -> String {
    let (repo, id) = match owner_repo_i64(args, "id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let api_path = repo.api(&format!("/pipelines/{id}/cancel"));
    let client = crate::client::ApiClient::new(state);
    run(async move { client.post_empty(&api_path).await })
}

fn tool_get_ci_job(state: &AppState, args: &Value) -> String {
    let (repo, id) = match owner_repo_i64(args, "id") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let job_id = args.get("job_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if job_id == 0 {
        return "Error: job_id is required".into();
    }
    get(state, repo.api(&format!("/pipelines/{id}/jobs/{job_id}")))
}

fn tool_get_commit_status(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let sha = arg_str(args, "sha");
    if sha.is_empty() {
        return "Error: sha is required".into();
    }
    let sha = match path_segment("sha", sha) {
        Ok(sha) => sha,
        Err(e) => return e,
    };
    get(state, repo.api(&format!("/commits/{sha}/status")))
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
    get(state, format!("/search?{}", qs.join("&")))
}

// ── AI agent endpoints ────────────────────────────────────

fn tool_ai_repo_summary(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
        Ok(v) => v,
        Err(e) => return e,
    };
    get(state, repo.ai("/summary"))
}

fn tool_ai_list_issues(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
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
    get(state, repo.ai(&format!("/issues{query}")))
}

fn tool_ai_list_prs(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
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
    get(state, repo.ai(&format!("/prs{query}")))
}

fn tool_ai_repo_tree(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
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
    get(state, repo.ai(&format!("/tree{query}")))
}

fn tool_ai_search_code(state: &AppState, args: &Value) -> String {
    let repo = match owner_repo(args) {
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
    get(state, repo.ai(&format!("/search/code?{}", qs.join("&"))))
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
        let dispatched: Vec<&str> = TOOL_DISPATCH.iter().map(|(name, _, _)| *name).collect();

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
        let widgets = RepoPath::new("acme", "widgets").unwrap();
        assert_eq!(
            read_file_path(&widgets, "src/main.rs", "").unwrap(),
            "/repos/acme/widgets/blob/src/main.rs"
        );
        assert_eq!(
            read_file_path(&widgets, "src/main.rs", "master").unwrap(),
            "/repos/acme/widgets/blob/src/main.rs?ref=master"
        );
        assert_eq!(read_dir_path(&widgets, "", ""), "/repos/acme/widgets/tree");
        assert_eq!(
            read_dir_path(&widgets, "src/api", "master"),
            "/repos/acme/widgets/tree?ref=master&path=src%2Fapi"
        );
    }

    /// card_3d05153aaca1: `list_repos` addressed `GET /repos`, which is mounted
    /// for `POST` alone — so the router answered every call with `405` and the
    /// server answered none. Found by the sweep in
    /// `rg-http/tests/integration/mcp_route_coverage_tests.rs`, which is also
    /// where the live-server proof lives.
    #[test]
    fn listing_repositories_addresses_routes_the_server_mounts() {
        assert_eq!(list_repos_path("").unwrap(), "/repos/explore");
        assert_eq!(list_repos_path("acme").unwrap(), "/repos/acme");
        // An owner is one path segment, so a name that is not one cannot be
        // spliced in raw.
        assert_eq!(list_repos_path("a/b").unwrap(), "/repos/a%2Fb");
        assert!(list_repos_path("..").is_err());
    }

    /// A path segment carrying `?`, `#` or a space is a file name, not query
    /// syntax — but the separators between segments must survive, since the
    /// route matches them as a wildcard.
    #[test]
    fn a_file_path_is_escaped_per_segment_not_wholesale() {
        let widgets = RepoPath::new("acme", "widgets").unwrap();
        assert_eq!(
            read_file_path(&widgets, "docs/read me?.md", "feature/new").unwrap(),
            "/repos/acme/widgets/blob/docs/read%20me%3F.md?ref=feature%2Fnew"
        );
    }

    /// `write_file` posts to the write half of `/contents/{path}` — the route
    /// `read_file` must never address — with the same per-segment escaping.
    #[test]
    fn write_file_addresses_the_contents_write_route() {
        let widgets = RepoPath::new("acme", "widgets").unwrap();
        assert_eq!(
            write_file_path(&widgets, "docs/read me?.md").unwrap(),
            "/repos/acme/widgets/contents/docs/read%20me%3F.md"
        );
    }

    #[test]
    fn write_file_validates_its_arguments_before_any_network_call() {
        let no_content =
            serde_json::json!({ "owner": "o", "repo": "r", "path": "a.txt", "message": "m" });
        assert!(call_text("write_file", no_content).starts_with("Error: path, content"));
        let no_message = serde_json::json!({
            "owner": "o", "repo": "r", "path": "a.txt", "content": "x", "message": ""
        });
        assert!(call_text("write_file", no_message).starts_with("Error: path, content"));
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

    // ── card_5b6ce4ccc0a7: agent-supplied strings stay in their segment ─────

    use std::sync::{Arc, Mutex};

    /// An in-process transport that sends nothing: it writes down every
    /// request a tool builds and answers `200 {}`.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<(String, String)>>);

    impl crate::ApiTransport for Recorder {
        fn exchange(
            &self,
            method: crate::Method,
            path: String,
            _body: Option<Value>,
        ) -> crate::ApiFuture<'_> {
            self.0
                .lock()
                .expect("the recorder mutex is not poisoned")
                .push((method.as_str().to_string(), path));
            Box::pin(async {
                Ok(crate::ApiResponse {
                    status: 200,
                    body: b"{}".to_vec(),
                })
            })
        }
    }

    /// Call `name` through an in-process state whose transport records what it
    /// was asked to send: the tool's answer and the requests it built.
    fn call_recorded(name: &str, arguments: Value) -> (String, Vec<(String, String)>) {
        let recorder = Arc::new(Recorder::default());
        let state = AppState::in_process(recorder.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a test runtime");
        let _entered = runtime.enter();
        let response = call_tool(
            &state,
            &req(
                "tools/call",
                serde_json::json!({ "name": name, "arguments": arguments }),
            ),
        );
        let text = response
            .result
            .and_then(|v| v.get("content").cloned())
            .and_then(|c| c.get(0).cloned())
            .and_then(|c| c.get("text").and_then(|t| t.as_str().map(String::from)))
            .unwrap_or_default();
        let sent = std::mem::take(&mut *recorder.0.lock().expect("not poisoned"));
        (text, sent)
    }

    /// Arguments that get every tool as far as its request: `owner`/`repo`
    /// plus whatever its schema requires. Kept in step with the dispatch table
    /// by `every_tool_keeps_owner_and_repo_inside_their_own_segments`.
    fn probe_arguments(name: &str, owner: &str, repo: &str) -> Value {
        let mut args = serde_json::json!({ "owner": owner, "repo": repo });
        let extra = match name {
            "read_file" => serde_json::json!({ "path": "README.md" }),
            "write_file" => {
                serde_json::json!({ "path": "README.md", "content": "x", "message": "m" })
            }
            "create_issue" => serde_json::json!({ "title": "t" }),
            "comment_issue" => serde_json::json!({ "number": 1, "body": "b" }),
            "set_issue_labels" => serde_json::json!({ "number": 1, "labels": ["l"] }),
            "create_pr" => serde_json::json!({ "title": "t", "head": "h", "base": "main" }),
            "merge_pr" => serde_json::json!({ "number": 1, "strategy": "merge" }),
            "request_reviewers" => serde_json::json!({ "number": 1, "username": "u" }),
            "create_review" => serde_json::json!({ "number": 1, "action": "comment" }),
            "create_review_comment" => {
                serde_json::json!({ "number": 1, "path": "README.md", "body": "b" })
            }
            "apply_suggestion" => serde_json::json!({ "number": 1, "comment_id": 1 }),
            "get_pipeline" | "retry_pipeline" | "cancel_pipeline" => {
                serde_json::json!({ "id": 1 })
            }
            "get_ci_job" => serde_json::json!({ "id": 1, "job_id": 1 }),
            "get_commit_status" => serde_json::json!({ "sha": "abc" }),
            "search" | "ai_search_code" => serde_json::json!({ "q": "probe" }),
            "get_issue"
            | "get_pr"
            | "get_pr_diff"
            | "list_reviews"
            | "list_review_comments"
            | "update_issue" => serde_json::json!({ "number": 1 }),
            _ => serde_json::json!({}),
        };
        for (key, value) in extra.as_object().expect("an object") {
            args[key] = value.clone();
        }
        args
    }

    /// Match a concrete request path against a declared route template,
    /// segment by segment, and return what each `{param}` captured — decoded,
    /// the way the server's router hands it to a handler.
    fn captures(template: &str, path: &str) -> Option<Vec<(String, String)>> {
        let path = path.split('?').next().unwrap_or_default();
        let mut concrete = path.split('/');
        let mut captured = Vec::new();
        for part in template.split('/') {
            if part.starts_with("{*") {
                return concrete.next().map(|_| captured);
            }
            let segment = concrete.next()?;
            if let Some(name) = part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                if segment.is_empty() {
                    return None;
                }
                let decoded = urlencoding::decode(segment).ok()?.into_owned();
                captured.push((name.to_string(), decoded));
            } else if part != segment {
                return None;
            }
        }
        concrete.next().is_none().then_some(captured)
    }

    /// Layer one of card_5b6ce4ccc0a7, on its own — no server and no route
    /// binding behind it: whatever an agent puts in `owner` or `repo`, the
    /// request every tool builds is one of the routes that tool declares, and
    /// the router would hand the handler back exactly the string the agent
    /// sent. The card's exploit was `repo = "app/pulls/12/ci-approval?"`
    /// turning `retry_pipeline` into `POST …/pulls/12/ci-approval`.
    #[test]
    fn every_tool_keeps_owner_and_repo_inside_their_own_segments() {
        let hostile = [
            "app/pulls/12/ci-approval?",
            "app#frag",
            "a%2Fb",
            "a\\b",
            "%2e%2e",
            "..app",
        ];
        let mut probed = 0;
        for (name, _, routes) in TOOL_DISPATCH {
            for value in hostile {
                for (owner, repo) in [("alice", value), (value, "app")] {
                    let (answer, sent) = call_recorded(name, probe_arguments(name, owner, repo));
                    assert!(
                        !answer.contains("is required") && !answer.contains("are required"),
                        "{name} rejected the probe's arguments: {answer}"
                    );
                    if *name == "search" {
                        // The one tool that names no repository.
                        assert_eq!(sent.len(), 1, "{name}: {sent:?}");
                        continue;
                    }
                    if *name == "list_repos" && owner == "alice" {
                        // `list_repos` reads the owner alone.
                        continue;
                    }
                    assert_eq!(sent.len(), 1, "{name} sent {sent:?}");
                    let (method, path) = &sent[0];
                    let matched: Vec<Vec<(String, String)>> = routes
                        .iter()
                        .filter(|route| route.method == method.as_str())
                        .filter_map(|route| captures(route.path, path))
                        .collect();
                    assert_eq!(
                        matched.len(),
                        1,
                        "{name} with owner={owner:?} repo={repo:?} sent {method} {path}, \
                         which is none of its routes {routes:?}"
                    );
                    let params = &matched[0];
                    let param = |key: &str| {
                        params
                            .iter()
                            .find(|(k, _)| k == key)
                            .map(|(_, v)| v.as_str())
                    };
                    assert_eq!(param("owner"), Some(owner), "{name}: {path}");
                    if *name != "list_repos" {
                        assert_eq!(param("name"), Some(repo), "{name}: {path}");
                    }
                    probed += 1;
                }
            }
        }
        assert!(probed > TOOL_DISPATCH.len(), "the sweep probed nothing");
    }

    /// `.` and `..` survive percent-encoding unchanged, and the HTTP backend's
    /// URL parser resolves them: `…/blob/../../../admin/users` is
    /// `…/admin/users` by the time it leaves. So they are refused before any
    /// request is built — in `owner`, `repo`, a file path and a commit SHA.
    #[test]
    fn dot_segments_are_refused_before_any_request_is_sent() {
        let cases = [
            (
                "retry_pipeline",
                probe_arguments("retry_pipeline", "alice", ".."),
            ),
            ("get_issue", probe_arguments("get_issue", ".", "app")),
            ("list_repos", serde_json::json!({ "owner": ".." })),
            (
                "read_file",
                serde_json::json!({ "owner": "alice", "repo": "app", "path": "../../../admin/users" }),
            ),
            (
                "write_file",
                serde_json::json!({
                    "owner": "alice", "repo": "app", "path": "a/./b",
                    "content": "x", "message": "m",
                }),
            ),
            (
                "get_commit_status",
                serde_json::json!({ "owner": "alice", "repo": "app", "sha": ".." }),
            ),
        ];
        for (name, arguments) in cases {
            let (answer, sent) = call_recorded(name, arguments.clone());
            assert!(
                sent.is_empty(),
                "{name} {arguments} sent a request: {sent:?}"
            );
            assert!(
                answer.starts_with("Error:") && answer.contains("'.' or '..'"),
                "{name} {arguments}: {answer}"
            );
        }
    }

    /// Every declared route is spelled the way the server's router spells
    /// one: an upper-case method and a path under `/api/v1`. Whether each is
    /// actually mounted is `rg-http`'s to prove, with the router in hand.
    #[test]
    fn every_tool_declares_its_routes_in_the_servers_spelling() {
        for (name, _, routes) in TOOL_DISPATCH {
            assert!(!routes.is_empty(), "{name} declares no route");
            for route in *routes {
                assert!(
                    ["GET", "POST", "PATCH", "PUT", "DELETE"].contains(&route.method),
                    "{name}: {route:?}"
                );
                assert!(route.path.starts_with("/api/v1/"), "{name}: {route:?}");
            }
            assert_eq!(tool_routes(name), Some(*routes));
        }
        assert_eq!(tool_routes("does_not_exist"), None);
    }
}
