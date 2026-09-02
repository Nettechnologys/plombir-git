//! card_4c22ff9b1282: a client ref whose spelling Git's own grammar rejects is
//! a deterministic client mistake, and every REST surface that took one used to
//! hand it to gix and report the resulting anonymous validation error as a
//! `500` — the one answer that invites a retry of a request that can never
//! succeed. Worse on the write surfaces: repository creation reached that
//! failure only after claiming a directory and running `gix init`, and PR
//! creation never checked the base branch at all, so a name no Git operation
//! can ever resolve was persisted on the row.
//!
//! Each surface is asserted in all three directions, because the fix is only
//! correct if it moves exactly one of them:
//!
//! - malformed ref → typed `400`,
//! - well-formed but absent ref → the endpoint's documented negative answer,
//! - unreadable repository → still `5xx`, with operator context kept.

use crate::common::{create_repo, register_full, spawn_test_app_with_repo_root};

/// The spellings the card names. `main^` and `@{-1}` are revspec syntax, `a..b`
/// is a range, and `refs/heads/-x` is a branch component Git refuses because it
/// reads as an option to every command that takes a branch.
const MALFORMED_REFS: [&str; 4] = ["main^", "@{-1}", "refs/heads/-x", "a..b"];

/// Put one commit in the repository through the contents API, so the read
/// endpoints reach their ref resolution instead of the empty-repository path.
async fn commit_a_file(client: &reqwest::Client, base: &str, token: &str, owner: &str, repo: &str) {
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/contents/README.md"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "content": "# hello\n",
            "message": "add README.md",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "the fixture needs a commit: {}",
        resp.text().await.unwrap_or_default()
    );
}

/// Make the bare repository unopenable while leaving its directory in place, so
/// the handler still reaches the git layer the 5xx assertions are about.
fn break_repository(bare: &std::path::Path) {
    std::fs::remove_dir_all(bare.join("objects")).expect("bare repo must have an objects dir");
    assert!(
        gix::open(bare).is_err(),
        "fixture must produce a repository gix refuses to open"
    );
}

/// `GET .../tree`, `.../blob/{path}` and `.../log` all resolve their `?ref=`
/// through the same helper, so all three are asserted: a fix applied to one
/// call site instead of the helper would leave the other two at `500`.
#[tokio::test]
async fn malformed_content_refs_are_client_errors_on_every_read_endpoint() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "contentref-owner", "contentref@example.com").await;
    create_repo(&base, &token, "contentref-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(
        &client,
        &base,
        &token,
        "contentref-owner",
        "contentref-repo",
    )
    .await;

    let endpoints = [
        "/api/v1/repos/contentref-owner/contentref-repo/tree",
        "/api/v1/repos/contentref-owner/contentref-repo/blob/README.md",
        "/api/v1/repos/contentref-owner/contentref-repo/log",
    ];

    for endpoint in endpoints {
        for git_ref in MALFORMED_REFS {
            let resp = client
                .get(format!("{base}{endpoint}"))
                .query(&[("ref", git_ref)])
                .bearer_auth(&token)
                .send()
                .await
                .expect("request");
            let status = resp.status();
            let body: serde_json::Value = resp.json().await.expect("json body");
            assert_eq!(
                status.as_u16(),
                400,
                "{endpoint}?ref={git_ref} must be a client error, got {status} (body: {body})"
            );
            assert_eq!(
                body["error"]["message"], "invalid ref name",
                "the refusal must be the fixed text, not an echo of the ref: {body}"
            );
        }

        // A well-formed name that names nothing is the endpoint's honest
        // negative answer, and must not be swallowed by the new gate.
        let resp = client
            .get(format!("{base}{endpoint}"))
            .query(&[("ref", "no-such-branch")])
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status().as_u16(),
            404,
            "{endpoint} must still answer 404 for a well-formed absent ref"
        );
    }

    // The third outcome: a repository that cannot be read is the server's
    // failure even when the ref the caller named is perfectly well formed.
    break_repository(&repo_root.join("contentref-owner/contentref-repo.git"));
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/contentref-owner/contentref-repo/blob/README.md"
        ))
        .query(&[("ref", "main")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    assert!(
        status.is_server_error(),
        "an unreadable repository must stay a 5xx, got {status}"
    );
}

/// An empty repository answers `200 {entries: []}` on `.../tree` for *any*
/// unresolvable ref. That fallback must not absorb the new 400: a malformed ref
/// is the caller's mistake whatever the repository happens to hold.
#[tokio::test]
async fn a_malformed_tree_ref_is_not_hidden_by_the_empty_repository_answer() {
    let (base, _repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "emptyref-owner", "emptyref@example.com").await;
    create_repo(&base, &token, "emptyref-repo").await;
    let client = reqwest::Client::new();

    let url = format!("{base}/api/v1/repos/emptyref-owner/emptyref-repo/tree");

    // Baseline: on an empty repository a well-formed ref really does answer the
    // empty-tree 200, so the assertion below is about the malformed ref alone.
    let resp = client
        .get(&url)
        .query(&[("ref", "main")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status().as_u16(),
        200,
        "an unborn HEAD is still the empty-repository answer"
    );

    let resp = client
        .get(&url)
        .query(&[("ref", "a..b")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status.as_u16(),
        400,
        "a malformed ref must not be answered with an empty tree (body: {body})"
    );
}

/// The manual CI surfaces — the dispatch-form probe and the trigger itself —
/// share one resolver, and both take the ref straight from the caller.
#[tokio::test]
async fn malformed_refs_are_client_errors_on_the_manual_ci_surfaces() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "ciref-owner", "ciref@example.com").await;
    create_repo(&base, &token, "ciref-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "ciref-owner", "ciref-repo").await;

    for git_ref in MALFORMED_REFS {
        let resp = client
            .get(format!(
                "{base}/api/v1/repos/ciref-owner/ciref-repo/pipelines/workflow-dispatch"
            ))
            .query(&[("ref", git_ref)])
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(
            status.as_u16(),
            400,
            "workflow-dispatch schema for ref {git_ref} must be a client error, got {status} \
             (body: {body})"
        );
        assert_eq!(body["error"]["message"], "invalid ref name", "body: {body}");

        let resp = client
            .post(format!(
                "{base}/api/v1/repos/ciref-owner/ciref-repo/pipelines"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "ref": git_ref }))
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(
            status.as_u16(),
            400,
            "triggering ref {git_ref} must be a client error, got {status} (body: {body})"
        );
        assert_eq!(body["error"]["message"], "invalid ref name", "body: {body}");
    }

    // Well-formed but absent stays the documented negative answer: the endpoint
    // names the ref it could not resolve.
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/ciref-owner/ciref-repo/pipelines/workflow-dispatch"
        ))
        .query(&[("ref", "no-such-branch")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status.as_u16(), 400, "body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot resolve commit SHA for ref"),
        "an absent ref keeps its own wording, distinct from a malformed one: {body}"
    );

    // And a repository that cannot be opened stays a server failure.
    break_repository(&repo_root.join("ciref-owner/ciref-repo.git"));
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/ciref-owner/ciref-repo/pipelines/workflow-dispatch"
        ))
        .query(&[("ref", "main")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    assert!(
        status.is_server_error(),
        "an unreadable repository must stay a 5xx, got {status}"
    );
}

/// The requested default branch reaches `git init -b` and the symbolic HEAD
/// edit. Refusing it late meant a directory and a `gix init` had already
/// happened; refusing it early must leave neither storage nor a row behind.
#[tokio::test]
async fn a_malformed_default_branch_is_refused_before_any_repository_exists() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "branchref-owner", "branchref@example.com").await;
    let client = reqwest::Client::new();

    for (index, branch) in ["main^", "@{-1}", "-x", "a..b"].into_iter().enumerate() {
        let name = format!("defaultbranch-{index}");
        let resp = client
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "name": name, "default_branch": branch }))
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(
            status.as_u16(),
            400,
            "default_branch {branch} must be a client error, got {status} (body: {body})"
        );
        assert_eq!(
            body["error"]["message"], "invalid default branch name",
            "body: {body}"
        );

        assert!(
            !repo_root
                .join(format!("branchref-owner/{name}.git"))
                .exists(),
            "the rejected create must leave no repository storage behind"
        );
        let resp = client
            .get(format!("{base}/api/v1/repos/branchref-owner/{name}"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status().as_u16(),
            404,
            "the rejected create must leave no database row behind"
        );
    }

    // The same endpoint still creates a repository on a well-formed branch, so
    // the gate above is about the spelling and not about the parameter.
    let resp = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "name": "defaultbranch-ok", "default_branch": "develop" }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status().as_u16(),
        201,
        "a valid default branch still creates"
    );
}

/// `head` used to fail inside `try_get_branch_sha` as a `500`, and `base` was
/// never validated at all — it was written to the row, where the next merge,
/// diff or CI trigger inherited a name no Git operation can resolve.
#[tokio::test]
async fn malformed_pull_request_branches_are_refused_before_the_row_is_written() {
    let (base, _repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "prref-owner", "prref@example.com").await;
    create_repo(&base, &token, "prref-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "prref-owner", "prref-repo").await;

    let url = format!("{base}/api/v1/repos/prref-owner/prref-repo/pulls");

    for (side, head, base_branch) in [
        ("head", "main^", "main"),
        ("head", "a..b", "main"),
        ("base", "feature", "main^"),
        ("base", "feature", "a..b"),
    ] {
        let resp = client
            .post(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "title": "malformed ref",
                "head": head,
                "base": base_branch,
            }))
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(
            status.as_u16(),
            400,
            "{side} branch {head}/{base_branch} must be a client error, got {status} \
             (body: {body})"
        );
        assert_eq!(
            body["error"]["message"],
            format!("invalid {side} branch name"),
            "the refusal must name which side is wrong: {body}"
        );
    }

    // A well-formed head that names no branch keeps its own answer — the
    // distinction between "this name cannot exist" and "no such branch here".
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "absent head",
            "head": "no-such-branch",
            "base": "main",
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status.as_u16(), 400, "body: {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not found"),
        "an absent branch keeps its own wording: {body}"
    );

    // Nothing above may have been persisted.
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.expect("json body");
    let pulls = body
        .get("pull_requests")
        .or_else(|| body.get("data"))
        .and_then(|value| value.as_array())
        .or_else(|| body.as_array())
        .expect("the pull request listing is an array");
    assert!(
        pulls.is_empty(),
        "no refused pull request may have reached the database: {body}"
    );
}
