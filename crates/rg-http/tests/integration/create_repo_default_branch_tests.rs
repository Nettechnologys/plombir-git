//! card_e2204808d420: creating an empty repository recorded the requested
//! `default_branch` in the database while the bare repository kept gix's
//! template HEAD (`refs/heads/main`). A clone therefore saw an unborn `main`
//! instead of the branch the API had confirmed.

use crate::common::{register_full, spawn_test_app_with_repo_root};
use gix::bstr::ByteSlice;

#[tokio::test]
async fn creating_an_empty_repository_keeps_database_and_git_default_branches_in_sync() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(
        &base,
        "default-branch-owner",
        "default-branch-owner@example.com",
    )
    .await;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": "unborn-develop",
            "default_branch": "develop",
        }))
        .send()
        .await
        .expect("create repository request");
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("create repository response");
    assert_eq!(status.as_u16(), 201, "repository creation failed: {body}");

    let database_branch = body["default_branch"]
        .as_str()
        .expect("201 response carries the database default branch");
    let repository = gix::open(repo_root.join("default-branch-owner/unborn-develop.git"))
        .expect("the reported repository exists on disk");
    let head = repository.head().expect("read bare repository HEAD");
    let git_branch = head
        .referent_name()
        .expect("HEAD is symbolic")
        .shorten()
        .to_str()
        .expect("HEAD branch is UTF-8");

    assert_eq!(database_branch, "develop");
    assert_eq!(
        git_branch, database_branch,
        "the API recorded `{database_branch}`, but `git symbolic-ref HEAD` names `{git_branch}`"
    );
}
