//! Regression coverage for card_02784dd6dab7: the issue-metadata read surface of
//! a private repository must not answer anonymous callers.
//!
//! Five handlers took no `HeaderMap` at all, so they could not tell the owner
//! from a passer-by: the label list and a single label (`api/labels.rs`), the
//! labels of an issue (`api/issues.rs`) and both time-tracking reads
//! (`api/time_tracking.rs`). `GET issues/{n}` was already gated, which is what
//! made the leak easy to miss — the issue was closed, its metadata was not.
//!
//! The public half is asserted too: the gate has to keep anonymous reads of a
//! public repository working, or it has traded a leak for an outage.

use crate::common::{create_issue, register_full, spawn_test_app};

/// Create a repository with an explicit `is_private`.
async fn create_repo_with_visibility(base: &str, token: &str, name: &str, private: bool) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "is_private": private,
        }))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "create_repo({name}) should succeed");
}

/// GET `path` with an optional bearer token; returns the status code.
async fn get_status(base: &str, path: &str, token: Option<&str>) -> u16 {
    let mut req = reqwest::Client::new().get(format!("{base}{path}"));
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    req.send().await.expect("request").status().as_u16()
}

/// Create a label in `owner/repo`; returns its id.
async fn create_label(base: &str, token: &str, owner: &str, repo: &str, name: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/labels"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "color": "#ff0000"}))
        .send()
        .await
        .expect("create label");
    assert_eq!(resp.status(), 201, "create_label({name}) should succeed");
    resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("label id")
}

/// Log a time entry against an issue.
async fn add_time(base: &str, token: &str, owner: &str, repo: &str, number: i64) {
    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{number}/time"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "duration_minutes": 42,
            "description": "secret work",
        }))
        .send()
        .await
        .expect("add time");
    assert_eq!(resp.status(), 201, "add_time should succeed");
}

#[tokio::test]
async fn private_repo_issue_metadata_is_closed_to_anonymous_and_outsiders() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "meta-owner", "meta-owner@example.com").await;
    let (collab_token, collab_id) =
        register_full(&base, "meta-collab", "meta-collab@example.com").await;
    let (outsider_token, _) = register_full(&base, "meta-thief", "meta-thief@example.com").await;

    create_repo_with_visibility(&base, &owner_token, "secret-repo", true).await;

    let added = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/meta-owner/secret-repo/collaborators"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"user_id": collab_id, "permission": "read"}))
        .send()
        .await
        .expect("add collaborator");
    assert_eq!(added.status(), 201, "adding a collaborator should succeed");

    let (_id, number) = create_issue(
        &base,
        &owner_token,
        "meta-owner",
        "secret-repo",
        "Classified",
    )
    .await;
    let label_id = create_label(&base, &owner_token, "meta-owner", "secret-repo", "urgent").await;
    add_time(&base, &owner_token, "meta-owner", "secret-repo", number).await;

    let paths = [
        "/api/v1/repos/meta-owner/secret-repo/labels".to_string(),
        format!("/api/v1/repos/meta-owner/secret-repo/labels/{label_id}"),
        format!("/api/v1/repos/meta-owner/secret-repo/issues/{number}/labels"),
        format!("/api/v1/repos/meta-owner/secret-repo/issues/{number}/time"),
        format!("/api/v1/repos/meta-owner/secret-repo/issues/{number}/time/total"),
    ];

    for path in &paths {
        assert_eq!(
            get_status(&base, path, None).await,
            401,
            "anonymous reached {path} on a private repo"
        );
        assert_eq!(
            get_status(&base, path, Some(&outsider_token)).await,
            403,
            "an outsider reached {path} on a private repo"
        );

        for (label, token) in [
            ("the owner", owner_token.as_str()),
            ("a collaborator", collab_token.as_str()),
        ] {
            assert_eq!(
                get_status(&base, path, Some(token)).await,
                200,
                "{label} lost access to {path}"
            );
        }
    }
}

#[tokio::test]
async fn public_repo_issue_metadata_stays_readable_anonymously() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "open-owner", "open-owner@example.com").await;

    create_repo_with_visibility(&base, &owner_token, "open-repo", false).await;

    let (_id, number) = create_issue(
        &base,
        &owner_token,
        "open-owner",
        "open-repo",
        "Public defect",
    )
    .await;
    let label_id = create_label(&base, &owner_token, "open-owner", "open-repo", "bug").await;
    add_time(&base, &owner_token, "open-owner", "open-repo", number).await;

    for path in [
        "/api/v1/repos/open-owner/open-repo/labels".to_string(),
        format!("/api/v1/repos/open-owner/open-repo/labels/{label_id}"),
        format!("/api/v1/repos/open-owner/open-repo/issues/{number}/labels"),
        format!("/api/v1/repos/open-owner/open-repo/issues/{number}/time"),
        format!("/api/v1/repos/open-owner/open-repo/issues/{number}/time/total"),
    ] {
        assert_eq!(
            get_status(&base, &path, None).await,
            200,
            "the gate broke anonymous reads of a public repo at {path}"
        );
    }
}
