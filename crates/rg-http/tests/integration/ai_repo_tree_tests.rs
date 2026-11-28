//! Acceptance for card_7fe0684198fc: the agent-facing tree door answers with a
//! tree.
//!
//! `GET /api/v1/ai/repos/{owner}/{name}/tree` was mounted, gated, described in
//! the OpenAPI document and called by the MCP `ai_repo_tree` tool — and its
//! handler answered `501 {"error": "repo_tree not yet implemented"}` on every
//! call, in a body that carried no `error.code` to branch on. It now shares the
//! reader of the REST tree door, so the test holds the two side by side: a
//! door that merely stopped answering `501` without listing anything would not
//! match the REST answer.

use crate::common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";
const OWNER: &str = "ai-tree-owner";
const REPO: &str = "ai-tree-repo";

async fn commit_file(client: &reqwest::Client, base: &str, token: &str, path: &str) {
    let response = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/contents/{path}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "content": "hello\n",
            "message": format!("add {path}"),
        }))
        .send()
        .await
        .expect("commit request");
    assert_eq!(
        response.status(),
        200,
        "the fixture needs a commit: {}",
        response.text().await.unwrap_or_default()
    );
}

async fn get_json(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    query: &[(&str, &str)],
) -> (u16, serde_json::Value) {
    let response = client
        .get(url)
        .bearer_auth(token)
        .query(query)
        .send()
        .await
        .expect("tree request");
    let status = response.status().as_u16();
    let body = response.json().await.expect("tree body is JSON");
    (status, body)
}

#[tokio::test]
async fn the_ai_tree_lists_what_the_rest_tree_lists() {
    let base = spawn_test_app().await;
    let token = register_user(&base, OWNER, "ai-tree-owner@example.test", PW).await;
    create_repo(&base, &token, REPO).await;
    let client = reqwest::Client::new();

    let ai = format!("{base}/api/v1/ai/repos/{OWNER}/{REPO}/tree");
    let rest = format!("{base}/api/v1/repos/{OWNER}/{REPO}/tree");

    // An empty repository is a healthy state on both doors, not a fault.
    let (status, body) = get_json(&client, &ai, &token, &[]).await;
    assert_eq!(status, 200, "an empty repository must list nothing: {body}");
    assert_eq!(body["entries"], serde_json::json!([]), "{body}");

    commit_file(&client, &base, &token, "README.md").await;
    commit_file(&client, &base, &token, "src/lib.rs").await;

    for query in [vec![], vec![("path", "src")], vec![("ref", "main")]] {
        let (ai_status, ai_body) = get_json(&client, &ai, &token, &query).await;
        let (rest_status, rest_body) = get_json(&client, &rest, &token, &query).await;
        assert_eq!(ai_status, 200, "{query:?}: {ai_body}");
        assert_eq!(rest_status, 200, "{query:?}: {rest_body}");
        assert_eq!(
            ai_body, rest_body,
            "the agent door must answer the REST listing for {query:?}"
        );
        assert!(
            ai_body["entries"]
                .as_array()
                .is_some_and(|entries| !entries.is_empty()),
            "{query:?} must list the committed files: {ai_body}"
        );
    }

    let (_, root) = get_json(&client, &ai, &token, &[]).await;
    let names: Vec<&str> = root["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    assert!(
        names.contains(&"README.md") && names.contains(&"src"),
        "root must list the file and the directory: {root}"
    );

    // A miss is a typed refusal in the API envelope, not the old flat body.
    let (status, body) = get_json(&client, &ai, &token, &[("ref", "no-such-branch")]).await;
    assert_eq!(status, 404, "{body}");
    assert!(
        body["error"]["code"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "the refusal must carry `error.code`: {body}"
    );
}
