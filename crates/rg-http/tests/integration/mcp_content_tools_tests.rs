//! The MCP content-read tools against the router that has to answer them.
//!
//! `read_file` and `read_dir` are the two tools an agent reaches for first, and
//! both addressed routes this server does not mount: `GET /contents/{path}`
//! (mounted for `POST` and `DELETE` — writing a file and deleting one) and
//! `/tree/{ref}` (the ref is a query parameter). Advertised, dispatched, and
//! unable to answer a single call (card_66aa21756448).
//!
//! Pinning the paths in `rg-mcp`'s own unit tests cannot catch that: the crate
//! has no way to know which routes exist. This is the one place both halves are
//! in hand, so the tools are driven through `call_tool` — the same entry point
//! `main.rs` dispatches `tools/call` to — against a live server, and a route
//! that stops matching fails here rather than in an agent's session.

use std::path::Path;

use crate::common::{build_test_app_state, setup_test_db, wait_for_listener};

/// Run git through the sanctioned gateway (the `test_no_raw_git_command_in_crates`
/// regression guard forbids raw git process construction).
fn git(args: &[&str], cwd: Option<&Path>) {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize");
    let output = gateway.run(args, cwd).expect("git invocation failed");
    assert!(
        output.success(),
        "git {args:?} failed: {}",
        output.stderr_str().trim()
    );
}

/// Call one MCP entry point the way `main.rs` does — `tools/call` or
/// `resources/read` — and return the text content the agent receives.
///
/// The handlers are synchronous and reach the network through
/// `Handle::current().block_on`, so they run on a blocking thread rather than
/// on the runtime worker this test occupies.
async fn call_mcp(base_url: &str, method: &str, params: serde_json::Value) -> String {
    let state = rg_mcp::AppState::new(base_url.to_string(), String::new());
    let request = rg_mcp::protocol::JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: serde_json::Value::from(1),
        method: method.into(),
        params: Some(params),
    };
    let is_tool = method == "tools/call";
    let response = tokio::task::spawn_blocking(move || {
        if is_tool {
            rg_mcp::tools::call_tool(&state, &request)
        } else {
            rg_mcp::resources::read_resource(&state, &request)
        }
    })
    .await
    .expect("the handler did not panic");
    assert!(
        response.error.is_none(),
        "{method} answered an error: {}",
        response
            .error
            .map(|error| format!("{} {}", error.code, error.message))
            .unwrap_or_default()
    );
    // `tools/call` answers under `content`, `resources/read` under `contents`;
    // both carry the payload as `text` on their first element.
    response
        .result
        .and_then(|value| {
            value
                .get("content")
                .or_else(|| value.get("contents"))
                .cloned()
        })
        .and_then(|content| content.get(0).cloned())
        .and_then(|first| first.get("text").and_then(|t| t.as_str().map(String::from)))
        .expect("an answer carries one text block")
}

/// `tools/call`, with the tool's arguments.
async fn call_tool(base_url: &str, name: &str, arguments: serde_json::Value) -> String {
    call_mcp(
        base_url,
        "tools/call",
        serde_json::json!({ "name": name, "arguments": arguments }),
    )
    .await
}

/// Both tools, called the way the schema documents them — no `ref` at all — on
/// a repository whose default branch is `master`.
///
/// `master` on purpose: the tools promised "defaults to the default branch"
/// while `read_dir` hardcoded `main`, so a fixture on `main` would pass with
/// the branch name still nailed down in the source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_content_tools_read_a_repository_on_its_own_default_branch() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let user = rg_db::ops::user_ops::create_user(
        &db,
        "mcp-owner",
        "mcp-owner@example.com",
        "",
        "MCP Owner",
    )
    .await
    .unwrap();
    // Public so the tool's anonymous read (no PAT configured) is authorized.
    let repo = rg_core::repo::service::create_repo(
        &db, user.id, "mcp-repo", None, false, &repo_root, None,
    )
    .await
    .unwrap();
    let bare_path = repo_root.join("mcp-owner/mcp-repo.git");
    let bare_str = bare_path.to_string_lossy().to_string();

    let worktree = tempfile::tempdir().unwrap();
    git(&["init", "--initial-branch=master"], Some(worktree.path()));
    git(
        &["config", "user.name", "MCP Integration"],
        Some(worktree.path()),
    );
    git(
        &["config", "user.email", "mcp-integration@example.com"],
        Some(worktree.path()),
    );
    std::fs::create_dir_all(worktree.path().join("src")).unwrap();
    std::fs::write(
        worktree.path().join("src/main.rs"),
        "fn main() { println!(\"mcp\"); }\n",
    )
    .unwrap();
    git(&["add", "."], Some(worktree.path()));
    git(&["commit", "-m", "seed"], Some(worktree.path()));
    git(&["push", &bare_str, "master"], Some(worktree.path()));
    git(
        &[
            "--git-dir",
            &bare_str,
            "symbolic-ref",
            "HEAD",
            "refs/heads/master",
        ],
        None,
    );
    // Keep the row aligned with the repository it describes; the tools read
    // `HEAD` either way, but a fixture that leaves the two disagreeing is one
    // more thing to rule out when this test ever fails.
    rg_db::ops::repo_ops::set_default_branch(&db, repo.id, "master")
        .await
        .unwrap();

    let state = build_test_app_state(db.clone(), repo_root.clone());
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base_url = format!("http://{addr}");

    // ── read_file, no ref: the file's own bytes, not a routing error. ──
    let file = call_tool(
        &base_url,
        "read_file",
        serde_json::json!({
            "owner": "mcp-owner",
            "repo": "mcp-repo",
            "path": "src/main.rs",
        }),
    )
    .await;
    let file: serde_json::Value =
        serde_json::from_str(&file).unwrap_or_else(|_| panic!("read_file answered: {file}"));
    assert_eq!(
        file["content"],
        serde_json::json!("fn main() { println!(\"mcp\"); }\n"),
        "read_file did not return the committed file"
    );

    // ── read_dir, no ref: the entries of the root, on `master`. ──
    let tree = call_tool(
        &base_url,
        "read_dir",
        serde_json::json!({
            "owner": "mcp-owner",
            "repo": "mcp-repo",
        }),
    )
    .await;
    let tree: serde_json::Value =
        serde_json::from_str(&tree).unwrap_or_else(|_| panic!("read_dir answered: {tree}"));
    let names: Vec<&str> = tree["entries"]
        .as_array()
        .expect("read_dir answers an entries array")
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    assert!(
        names.contains(&"src"),
        "read_dir did not list the repository root: {tree}"
    );

    // ── read_dir into a subdirectory, with the ref named explicitly. ──
    let sub = call_tool(
        &base_url,
        "read_dir",
        serde_json::json!({
            "owner": "mcp-owner",
            "repo": "mcp-repo",
            "path": "src",
            "ref": "master",
        }),
    )
    .await;
    let sub: serde_json::Value =
        serde_json::from_str(&sub).unwrap_or_else(|_| panic!("read_dir answered: {sub}"));
    let names: Vec<&str> = sub["entries"]
        .as_array()
        .expect("read_dir answers an entries array")
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec!["main.rs"],
        "read_dir did not list the named subdirectory: {sub}"
    );

    // ── The `file://` resource reads through the same route as the tool. ──
    let resource = call_mcp(
        &base_url,
        "resources/read",
        serde_json::json!({ "uri": "file://mcp-owner/mcp-repo/src/main.rs" }),
    )
    .await;
    let resource: serde_json::Value = serde_json::from_str(&resource)
        .unwrap_or_else(|_| panic!("the file resource answered: {resource}"));
    assert_eq!(
        resource["content"],
        serde_json::json!("fn main() { println!(\"mcp\"); }\n"),
        "the file resource did not return the committed file"
    );

    server.abort();
}
