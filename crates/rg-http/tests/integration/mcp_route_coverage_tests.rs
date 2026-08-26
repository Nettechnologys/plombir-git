//! Every MCP tool must address a route this server mounts (card_3d05153aaca1).
//!
//! `read_file` and `read_dir` each spent their whole life pointed at a path
//! that was never a route — advertised, dispatched, and unable to answer a
//! single call (card_66aa21756448). `mcp_content_tools_tests` pins those two by
//! reading a real repository through them. This file is the other half: it says
//! nothing about what any tool *returns*, only that the request it builds was
//! matched by the router, and it says it about **every** tool and **every**
//! resource the server advertises. On its first run it found a third:
//! `list_repos` addressed `GET /repos`, mounted for `POST` alone.
//!
//! Neither crate can check this alone, which is why the gap lasted. `rg-mcp`'s
//! own `advertised_tools_match_the_dispatch_table` compares two lists that both
//! live inside `rg-mcp`, and neither knows a route from a typo;
//! `api-client-contract-check.mjs` reads the frontend and never opens this
//! crate. Here both halves are in hand.
//!
//! ## What counts as a failure
//!
//! Exactly two answers mean the router refused, and both are unambiguous:
//!
//! - `405` — the path exists under a different method, which is how
//!   `read_file`'s `GET /contents/{path}` was finally noticed;
//! - a `404` carrying the marker the fallback writes for a path no route
//!   claims (card_df4547b3c6f8). Before that fix an unmounted path answered
//!   `200` with the SPA shell, and this sweep would have had to sniff HTML to
//!   tell "there is no such endpoint" from "the endpoint answered".
//!
//! Anything else — `401`, `404 repository not found`, `422` — is the handler
//! talking, which is all this sweep asks for.
//!
//! ## Why the target repository does not exist
//!
//! Thirteen of the twenty-seven tools write. They are driven against
//! `probe/probe`, which is nothing on this instance, so every one of them is
//! turned away while resolving the repository and no request can leave a mark.
//! Routing is decided before any of that, so nothing is lost by it.

use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;

use crate::common::{build_test_app_state, setup_test_db, wait_for_listener};

/// The body the SPA fallback answers a path inside `/api/v1` with, verbatim
/// from `routes::protocol_subtrees_are_not_pages`. Spelled out rather than
/// imported because the point is to notice if it ever stops being produced.
const NO_SUCH_ROUTE: &str = "no endpoint is mounted at this path";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    method: Method,
    path: String,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Seen>>>);

impl Recorder {
    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.0.lock().expect("the recorder mutex is not poisoned"))
    }
}

async fn record(State(recorder): State<Recorder>, request: Request, next: Next) -> Response {
    recorder
        .0
        .lock()
        .expect("the recorder mutex is not poisoned")
        .push(Seen {
            method: request.method().clone(),
            path: request.uri().path().to_string(),
        });
    next.run(request).await
}

/// Routes whose test evidence used to disappear because the probe table names
/// only the MCP tool while the HTTP client builds the URL in another crate.
/// These rows are runtime assertions, not inventory annotations: the recorder
/// below must observe the same method and concrete path from the real tool.
struct RouteExpectation {
    tool: &'static str,
    method: Method,
    route: &'static str,
}

fn inventory_route_expectations() -> [RouteExpectation; 3] {
    [
        RouteExpectation {
            tool: "ai_list_issues",
            method: Method::GET,
            route: "/api/v1/ai/repos/{owner}/{name}/issues",
        },
        RouteExpectation {
            tool: "ai_list_prs",
            method: Method::GET,
            route: "/api/v1/ai/repos/{owner}/{name}/prs",
        },
        RouteExpectation {
            tool: "ai_repo_tree",
            method: Method::GET,
            route: "/api/v1/ai/repos/{owner}/{name}/tree",
        },
    ]
}

fn probe_path(route: &str) -> String {
    route.replace("{owner}", "probe").replace("{name}", "probe")
}

/// One call per tool, with arguments its schema calls required.
///
/// Values are deliberately dull — the repository does not exist, the numbers
/// are 1, the text is a word. What the handler makes of them is not this
/// sweep's business; that the router matched the path they build is.
fn tool_probes() -> Vec<(&'static str, serde_json::Value)> {
    let repo = serde_json::json!({ "owner": "probe", "repo": "probe" });
    // `owner`/`repo` plus whatever else the schema marks required.
    let with = |extra: serde_json::Value| -> serde_json::Value {
        let mut merged = repo.as_object().cloned().expect("an object");
        for (key, value) in extra.as_object().expect("an object") {
            merged.insert(key.clone(), value.clone());
        }
        serde_json::Value::Object(merged)
    };

    vec![
        ("list_repos", serde_json::json!({})),
        (
            "read_file",
            with(serde_json::json!({ "path": "README.md" })),
        ),
        ("read_dir", with(serde_json::json!({}))),
        ("get_issue", with(serde_json::json!({ "number": 1 }))),
        ("get_pr", with(serde_json::json!({ "number": 1 }))),
        ("get_pr_diff", with(serde_json::json!({ "number": 1 }))),
        (
            "create_issue",
            with(serde_json::json!({ "title": "probe" })),
        ),
        ("update_issue", with(serde_json::json!({ "number": 1 }))),
        (
            "comment_issue",
            with(serde_json::json!({ "number": 1, "body": "probe" })),
        ),
        (
            "set_issue_labels",
            with(serde_json::json!({ "number": 1, "labels": ["probe"] })),
        ),
        (
            "create_pr",
            with(serde_json::json!({ "title": "probe", "head": "probe", "base": "main" })),
        ),
        (
            "merge_pr",
            with(serde_json::json!({ "number": 1, "strategy": "merge" })),
        ),
        (
            "request_reviewers",
            with(serde_json::json!({ "number": 1, "username": "probe" })),
        ),
        (
            "create_review",
            with(serde_json::json!({ "number": 1, "action": "comment" })),
        ),
        (
            "create_review_comment",
            with(serde_json::json!({ "number": 1, "path": "README.md", "body": "probe" })),
        ),
        (
            "apply_suggestion",
            with(serde_json::json!({ "number": 1, "comment_id": 1 })),
        ),
        ("list_pipelines", with(serde_json::json!({}))),
        ("get_pipeline", with(serde_json::json!({ "id": 1 }))),
        ("retry_pipeline", with(serde_json::json!({ "id": 1 }))),
        ("cancel_pipeline", with(serde_json::json!({ "id": 1 }))),
        (
            "get_ci_job",
            with(serde_json::json!({ "id": 1, "job_id": 1 })),
        ),
        ("search", serde_json::json!({ "q": "probe" })),
        ("ai_repo_summary", with(serde_json::json!({}))),
        ("ai_list_issues", with(serde_json::json!({}))),
        ("ai_list_prs", with(serde_json::json!({}))),
        ("ai_repo_tree", with(serde_json::json!({}))),
        ("ai_search_code", with(serde_json::json!({ "q": "probe" }))),
    ]
}

/// The tool names the server advertises to an agent, read out of `tools/list`
/// itself so the sweep cannot fall out of step with what is on offer.
fn advertised_tool_names(state: &rg_mcp::AppState) -> Vec<String> {
    let request = rg_mcp::protocol::JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: serde_json::Value::from(1),
        method: "tools/list".into(),
        params: None,
    };
    rg_mcp::tools::list_tools(state, &request)
        .result
        .and_then(|value| value.get("tools").cloned())
        .and_then(|tools| tools.as_array().cloned())
        .expect("tools/list answers an array")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(String::from))
        .collect()
}

/// Call one tool the way `main.rs` dispatches `tools/call` and return the text
/// the agent would see. The handlers block on the current runtime handle, so
/// they run on a blocking thread rather than on this test's worker.
async fn call_tool(base_url: &str, name: &str, arguments: serde_json::Value) -> String {
    let state = rg_mcp::AppState::new(base_url.to_string(), String::new());
    let request = rg_mcp::protocol::JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: serde_json::Value::from(1),
        method: "tools/call".into(),
        params: Some(serde_json::json!({ "name": name, "arguments": arguments })),
    };
    let response = tokio::task::spawn_blocking(move || rg_mcp::tools::call_tool(&state, &request))
        .await
        .expect("the handler did not panic");
    assert!(
        response.error.is_none(),
        "{name} answered a JSON-RPC error: {:?}",
        response.error.map(|error| error.message)
    );
    response
        .result
        .and_then(|value| value.get("content").cloned())
        .and_then(|content| content.get(0).cloned())
        .and_then(|first| first["text"].as_str().map(String::from))
        .expect("a tool answers one text block")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_mcp_tool_addresses_a_route_this_server_mounts() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create the test repo root");

    let state = build_test_app_state(db, repo_root);
    let recorder = Recorder::default();
    let app = rg_http::create_router_for_test(state).layer(axum::middleware::from_fn_with_state(
        recorder.clone(),
        record,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base_url = format!("http://{addr}");

    let probes = tool_probes();

    // A tool nobody wrote a probe for would otherwise be swept in silence,
    // which is the failure mode this whole file exists to end.
    let mut advertised =
        advertised_tool_names(&rg_mcp::AppState::new(base_url.clone(), String::new()));
    let mut probed: Vec<String> = probes.iter().map(|(name, _)| (*name).into()).collect();
    advertised.sort();
    probed.sort();
    assert!(!advertised.is_empty(), "tools/list advertises nothing");
    assert_eq!(
        probed, advertised,
        "the probe table and the advertised tools have drifted apart"
    );

    let expectations = inventory_route_expectations();
    for (name, arguments) in probes {
        let unread = recorder.take();
        assert!(
            unread.is_empty(),
            "{name} started with {} unread request(s) recorded",
            unread.len()
        );
        let answer = call_tool(&base_url, name, arguments).await;
        let seen = recorder.take();

        // The tool has to have got as far as sending a request: a handler that
        // turns its own arguments away never touches the router, and a sweep
        // that accepts that answer proves nothing about the path it builds.
        assert!(
            !answer.contains("is required") && !answer.contains("are required"),
            "{name} rejected the probe's arguments and never reached the server: {answer}"
        );

        assert!(
            !answer.contains("status=405"),
            "{name} addressed a path this server mounts under other methods only: {answer}"
        );
        assert!(
            !answer.contains(NO_SUCH_ROUTE),
            "{name} addressed a path no route claims: {answer}"
        );

        if let Some(expected) = expectations.iter().find(|expected| expected.tool == name) {
            assert_eq!(
                seen,
                [Seen {
                    method: expected.method.clone(),
                    path: probe_path(expected.route),
                }],
                "{name} no longer addresses the route credited to its executable coverage"
            );
        }
    }

    server.abort();
}

/// Call one resource the way `main.rs` dispatches `resources/read`, and return
/// whichever of the two answers came back: the text content on success, the
/// JSON-RPC error message on failure. Both carry what this sweep reads.
async fn call_resource(base_url: &str, uri: &str) -> String {
    let state = rg_mcp::AppState::new(base_url.to_string(), String::new());
    let request = rg_mcp::protocol::JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: serde_json::Value::from(1),
        method: "resources/read".into(),
        params: Some(serde_json::json!({ "uri": uri })),
    };
    let response =
        tokio::task::spawn_blocking(move || rg_mcp::resources::read_resource(&state, &request))
            .await
            .expect("the handler did not panic");
    if let Some(error) = response.error {
        return error.message;
    }
    response
        .result
        .and_then(|value| value.get("contents").cloned())
        .and_then(|contents| contents.get(0).cloned())
        .and_then(|first| first["text"].as_str().map(String::from))
        .expect("a resource answers one text block")
}

/// One URI per advertised scheme, filled in the way the templates in
/// `resources::list_resources` document them.
fn resource_probes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("repo", "repo://probe/probe"),
        ("file", "file://probe/probe/README.md"),
        ("issue", "issue://probe/probe/1"),
    ]
}

/// The same claim for the resource half of the MCP surface, which reaches the
/// same REST API through its own three handlers. `file://` is the one already
/// covered end to end by `mcp_content_tools_tests`; the other two were only
/// ever read by eye.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_mcp_resource_addresses_a_route_this_server_mounts() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create the test repo root");

    let state = build_test_app_state(db, repo_root);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base_url = format!("http://{addr}");

    let probes = resource_probes();

    let request = rg_mcp::protocol::JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: serde_json::Value::from(1),
        method: "resources/list".into(),
        params: None,
    };
    let listing_state = rg_mcp::AppState::new(base_url.clone(), String::new());
    let mut advertised: Vec<String> = rg_mcp::resources::list_resources(&listing_state, &request)
        .result
        .and_then(|value| value.get("resources").cloned())
        .and_then(|resources| resources.as_array().cloned())
        .expect("resources/list answers an array")
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .filter_map(|uri| uri.split("://").next().map(String::from))
        .collect();
    let mut probed: Vec<String> = probes.iter().map(|(scheme, _)| (*scheme).into()).collect();
    advertised.sort();
    probed.sort();
    assert!(!advertised.is_empty(), "resources/list advertises nothing");
    assert_eq!(
        probed, advertised,
        "the probe table and the advertised resources have drifted apart"
    );

    for (scheme, uri) in probes {
        let answer = call_resource(&base_url, uri).await;

        // As with the tools: a handler that turns its own URI away never
        // touches the router, and accepting that answer would prove nothing.
        assert!(
            !answer.contains(&format!("invalid {scheme} URI format")),
            "{scheme} rejected the probe URI and never reached the server: {answer}"
        );
        assert!(
            !answer.contains("status=405"),
            "{scheme} addressed a path this server mounts under other methods only: {answer}"
        );
        assert!(
            !answer.contains(NO_SUCH_ROUTE),
            "{scheme} addressed a path no route claims: {answer}"
        );
    }

    server.abort();
}
