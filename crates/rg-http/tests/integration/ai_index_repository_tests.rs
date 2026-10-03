//! Acceptance for card_928d72df493a: the AI code index has a producer that a
//! client can reach.
//!
//! `ai_search_code` was mounted and `ai_index_repository` was not, so every
//! hosted instance answered AI code searches out of an index only the server's
//! own `plombir-git index-repo` shell command could fill. The handler existed in
//! full, carried a complete `#[utoipa::path]` annotation, and no route led to
//! it — a door described but never cut.
//!
//! So the test exercises the pair, not the endpoint: index over HTTP, then read
//! back what the index made findable. A `200` on the write half proves nothing
//! on its own if the read half cannot see the result.

use crate::common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";
const OWNER: &str = "ai-index-owner";
const REPO: &str = "ai-index-repo";
const OUTSIDER: &str = "ai-index-outsider";

/// Put a file through the contents API so the repository has a commit to walk.
/// A freshly created repository has an unborn HEAD, and the indexer resolves the
/// default branch before it traverses anything.
async fn commit_file(client: &reqwest::Client, base: &str, token: &str, path: &str, content: &str) {
    let response = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/contents/{path}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "content": content,
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

#[tokio::test]
async fn indexing_over_http_makes_the_repository_searchable() {
    let base = spawn_test_app().await;
    let token = register_user(&base, OWNER, "ai-index-owner@example.test", PW).await;
    create_repo(&base, &token, REPO).await;
    let client = reqwest::Client::new();

    commit_file(
        &client,
        &base,
        &token,
        "src/needle.rs",
        "pub fn find_the_needle() {}\n",
    )
    .await;

    // Nothing has filled the index yet, and `ai_search_code` says so in as many
    // words — it refuses and tells the caller to "call the index endpoint",
    // which is exactly the endpoint that had no route. This is the baseline that
    // makes the assertion below mean something: without it, a search that always
    // answered would pass.
    let before = client
        .get(format!("{base}/api/v1/ai/repos/{OWNER}/{REPO}/search/code"))
        .bearer_auth(&token)
        .query(&[("q", "find_the_needle")])
        .send()
        .await
        .expect("pre-index search request");
    let status = before.status();
    let body: serde_json::Value = before.json().await.expect("pre-index search body");
    assert_eq!(
        status, 400,
        "an un-indexed repository must refuse the code search: {body}"
    );

    let response = client
        .post(format!("{base}/api/v1/ai/repos/{OWNER}/{REPO}/index"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("index request");
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("index response body");
    assert_eq!(status, 200, "indexing must succeed: {body}");
    let indexed = body["indexed_files"]
        .as_u64()
        .unwrap_or_else(|| panic!("index response must report indexed_files: {body}"));
    assert!(
        indexed > 0,
        "indexing a repository with files must report them: {body}"
    );

    let after = client
        .get(format!("{base}/api/v1/ai/repos/{OWNER}/{REPO}/search/code"))
        .bearer_auth(&token)
        .query(&[("q", "find_the_needle")])
        .send()
        .await
        .expect("post-index search request");
    assert_eq!(after.status(), 200, "search after indexing must succeed");
    let after: Vec<serde_json::Value> = after.json().await.expect("post-index search results");
    assert!(
        after.iter().any(|hit| hit["file_path"] == "src/needle.rs"),
        "the file committed above must be findable once indexed: {after:?}"
    );
}

/// A repository nobody has pushed to is a healthy state, not a server fault.
///
/// The indexer resolves the default branch before it walks anything, so an
/// unborn HEAD failed the whole request — mounting the route turned a `500` on
/// every freshly created repository into something a client could actually hit.
#[tokio::test]
async fn indexing_a_repository_without_commits_reports_nothing_indexed() {
    let base = spawn_test_app().await;
    let token = register_user(&base, OWNER, "ai-index-owner3@example.test", PW).await;
    create_repo(&base, &token, REPO).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base}/api/v1/ai/repos/{OWNER}/{REPO}/index"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("index request");
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("index response body");
    assert_eq!(
        status, 200,
        "an empty repository must be answerable, not a server error: {body}"
    );
    assert_eq!(
        body["indexed_files"], 0,
        "an empty repository has nothing to index: {body}"
    );
}

/// The route is mounted at `RepoWrite`, and the handler asks for it too.
///
/// Indexing replaces the repository's entire `code_fts` snapshot and walks the
/// whole tree to do it. The handler originally took `RepoRead`, which would have
/// let any reader of a public repository trigger both — so the level is asserted
/// from the outside, where a later edit to either declaration shows up.
#[tokio::test]
async fn a_reader_without_write_access_cannot_trigger_indexing() {
    let base = spawn_test_app().await;
    let owner = register_user(&base, OWNER, "ai-index-owner2@example.test", PW).await;
    create_repo(&base, &owner, REPO).await;
    let outsider = register_user(&base, OUTSIDER, "ai-index-outsider@example.test", PW).await;
    let client = reqwest::Client::new();

    // The outsider can read the public repository — that is what makes this a
    // test of the write boundary rather than of visibility.
    let readable = client
        .get(format!("{base}/api/v1/repos/{OWNER}/{REPO}"))
        .bearer_auth(&outsider)
        .send()
        .await
        .expect("repo read request");
    assert_eq!(
        readable.status(),
        200,
        "the fixture repository must be readable by the outsider"
    );

    let response = client
        .post(format!("{base}/api/v1/ai/repos/{OWNER}/{REPO}/index"))
        .bearer_auth(&outsider)
        .send()
        .await
        .expect("index request");
    assert_eq!(
        response.status(),
        403,
        "a reader must not be able to rewrite the repository's code index: {}",
        response.text().await.unwrap_or_default()
    );

    let anonymous = client
        .post(format!("{base}/api/v1/ai/repos/{OWNER}/{REPO}/index"))
        .send()
        .await
        .expect("anonymous index request");
    assert_eq!(
        anonymous.status(),
        401,
        "an anonymous caller must not be able to trigger indexing: {}",
        anonymous.text().await.unwrap_or_default()
    );
}
