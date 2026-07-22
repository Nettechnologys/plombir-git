//! Integration tests for issue time-tracking endpoints.
//!
//! Guards:
//!   POST   /repos/:o/:r/issues/:n/time        — add time entry
//!   GET    /repos/:o/:r/issues/:n/time        — list entries (paginated)
//!   GET    /repos/:o/:r/issues/:n/time/total  — total minutes + formatted
//!   DELETE /repos/:o/:r/issues/:n/time/:id    — delete entry

mod common;

use common::{create_issue, create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

async fn setup(suffix: &str) -> (String, String, String, String) {
    let base = spawn_test_app().await;
    let owner = format!("ttuser{suffix}");
    let token = register_user(&base, &owner, &format!("ttuser{suffix}@example.com"), PW).await;
    let repo = format!("ttrepo{suffix}");
    create_repo(&base, &token, &repo).await;
    (base, token, owner, repo)
}

#[tokio::test]
async fn test_add_and_list_time_entries() {
    let (base, token, owner, repo) = setup("1").await;
    let (_id, number) = create_issue(&base, &token, &owner, &repo, "Fix login").await;
    let client = reqwest::Client::new();

    // Add two entries
    for (mins, desc) in [(90i64, "research"), (30, "fix")] {
        let resp = client
            .post(format!(
                "{}/api/v1/repos/{}/{}/issues/{}/time",
                base, owner, repo, number
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"duration_minutes": mins, "description": desc}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "add_time failed: {}", resp.status());
    }

    // List entries
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time",
            base, owner, repo, number
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let entries = body["data"].as_array().unwrap();
    assert_eq!(
        entries.len(),
        2,
        "expected 2 time entries, got {}",
        entries.len()
    );
}

#[tokio::test]
async fn test_time_tracking_total() {
    let (base, token, owner, repo) = setup("2").await;
    let (_id, number) = create_issue(&base, &token, &owner, &repo, "Big task").await;
    let client = reqwest::Client::new();

    for mins in [60i64, 90, 30] {
        client
            .post(format!(
                "{}/api/v1/repos/{}/{}/issues/{}/time",
                base, owner, repo, number
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"duration_minutes": mins}))
            .send()
            .await
            .unwrap();
    }

    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/total",
            base, owner, repo, number
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["total_minutes"], 180, "total should be 60+90+30=180");
    // formatted: "3h"
    assert_eq!(body["total_formatted"], "3h");
}

#[tokio::test]
async fn test_time_tracking_delete_entry() {
    let (base, token, owner, repo) = setup("3").await;
    let (_id, number) = create_issue(&base, &token, &owner, &repo, "Tracked").await;
    let client = reqwest::Client::new();

    // Add entry
    let entry_id: i64 = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time",
            base, owner, repo, number
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"duration_minutes": 45}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["id"]
        .as_i64()
        .unwrap();

    // Delete
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/{}",
            base, owner, repo, number, entry_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        204,
        "delete_time_entry failed: {}",
        resp.status()
    );

    // List should be empty
    let body: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time",
            base, owner, repo, number
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        0,
        "entry should be deleted"
    );
}

#[tokio::test]
async fn test_total_time_empty_issue() {
    let (base, token, owner, repo) = setup("4").await;
    let (_id, number) = create_issue(&base, &token, &owner, &repo, "No time logged").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time/total",
            base, owner, repo, number
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["total_minutes"], 0);
}

#[tokio::test]
async fn test_add_time_requires_auth() {
    let (base, _token, owner, repo) = setup("5").await;
    let (_id, number) = create_issue(&base, &_token, &owner, &repo, "Auth test").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/time",
            base, owner, repo, number
        ))
        // no bearer auth
        .json(&serde_json::json!({"duration_minutes": 30}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "unauthenticated add_time should return 401"
    );
}
