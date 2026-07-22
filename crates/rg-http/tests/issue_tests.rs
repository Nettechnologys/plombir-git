//! Integration tests for Issues, Labels, and Milestones.
//!
//! Guards:
//!   POST   /repos/:o/:r/issues             — create issue
//!   GET    /repos/:o/:r/issues             — list issues (state filter)
//!   PATCH  /repos/:o/:r/issues/:n          — update issue (close/reopen)
//!   POST   /repos/:o/:r/issues/:n/comments — add comment
//!   GET    /repos/:o/:r/issues/:n/comments — list comments
//!   POST   /repos/:o/:r/labels             — create label
//!   GET    /repos/:o/:r/labels             — list labels
//!   POST   /repos/:o/:r/milestones         — create milestone
//!   GET    /repos/:o/:r/milestones         — list milestones

mod common;

use common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

async fn setup(suffix: &str) -> (String, String, String, String) {
    let base = spawn_test_app().await;
    let owner = format!("issueuser{suffix}");
    let token = register_user(&base, &owner, &format!("issueuser{suffix}@example.com"), PW).await;
    let repo = format!("issuerepo{suffix}");
    create_repo(&base, &token, &repo).await;
    (base, token, owner, repo)
}

// ── Issues ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_and_list_issues() {
    let (base, token, owner, repo) = setup("1").await;
    let client = reqwest::Client::new();

    for title in &[
        "Bug: crash on startup",
        "Feature: dark mode",
        "Docs: update readme",
    ] {
        let resp = client
            .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"title": title}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "create issue '{}' failed", title);
    }

    let resp = client
        .get(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let issues = body["data"].as_array().unwrap();
    assert_eq!(issues.len(), 3);
}

#[tokio::test]
async fn test_close_and_reopen_issue() {
    let (base, token, owner, repo) = setup("2").await;
    let client = reqwest::Client::new();

    let issue: serde_json::Value = client
        .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Close me"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let number = issue["number"].as_i64().unwrap();

    // Close
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, number
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "closed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["state"], "closed");

    // Reopen
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, number
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "open"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let reopened: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(reopened["state"], "open");
}

#[tokio::test]
async fn test_issue_state_filter() {
    let (base, token, owner, repo) = setup("3").await;
    let client = reqwest::Client::new();

    // Create 3 issues
    let mut numbers = vec![];
    for i in 1..=3 {
        let issue: serde_json::Value = client
            .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"title": format!("Issue {}", i)}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        numbers.push(issue["number"].as_i64().unwrap());
    }

    // Close the first one
    client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, numbers[0]
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "closed"}))
        .send()
        .await
        .unwrap();

    // List open — should be 2
    let body: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues?state=open",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        2,
        "should have 2 open issues"
    );

    // List closed — should be 1
    let body: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues?state=closed",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        1,
        "should have 1 closed issue"
    );
}

#[tokio::test]
async fn test_issue_comments() {
    let (base, token, owner, repo) = setup("4").await;
    let client = reqwest::Client::new();

    let issue: serde_json::Value = client
        .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Commented issue"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let number = issue["number"].as_i64().unwrap();

    // Add two comments
    for body_text in &["First comment", "Second comment"] {
        let resp = client
            .post(format!(
                "{}/api/v1/repos/{}/{}/issues/{}/comments",
                base, owner, repo, number
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"body": body_text}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "add comment failed: {}", resp.status());
    }

    // List comments
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/comments",
            base, owner, repo, number
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let comments: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(comments.as_array().unwrap().len(), 2);
}

// ── Labels ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_and_list_labels() {
    let (base, token, owner, repo) = setup("5").await;
    let client = reqwest::Client::new();

    for (name, color) in &[
        ("bug", "#ee0701"),
        ("enhancement", "#84b6eb"),
        ("docs", "#0075ca"),
    ] {
        let resp = client
            .post(format!("{}/api/v1/repos/{}/{}/labels", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": name, "color": color}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "create label '{}' failed", name);
    }

    let resp = client
        .get(format!("{}/api/v1/repos/{}/{}/labels", base, owner, repo))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let labels: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(labels.as_array().unwrap().len(), 3);
}

// ── Milestones ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_and_list_milestones() {
    let (base, token, owner, repo) = setup("6").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/milestones",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "v1.0",
            "description": "First stable release",
            "due_date": "2026-12-31"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "create milestone failed: {}",
        resp.status()
    );
    let ms: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(ms["title"], "v1.0");

    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/milestones",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mss: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(mss.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_update_and_delete_milestone() {
    let (base, token, owner, repo) = setup("7").await;
    let client = reqwest::Client::new();

    let ms: serde_json::Value = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/milestones",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "beta"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ms_id = ms["id"].as_i64().unwrap();

    // Update
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/milestones/{}",
            base, owner, repo, ms_id
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "closed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["state"], "closed");

    // Delete
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/milestones/{}",
            base, owner, repo, ms_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete milestone failed: {}",
        resp.status()
    );
}
