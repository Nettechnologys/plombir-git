//! Integration tests for organization & team endpoints.
//!
//! These specifically guard against the singular/plural migration table-name
//! regression (entities expect `organizations`/`teams`/`organization_members`,
//! while the phase8 migration originally created singular tables). Before the
//! `m20260616_0000015_rename_org_team_plural` fix, every assertion here would
//! fail at runtime with "no such table".

mod common;

use common::{register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

/// Register a user and return (token, user_id).
async fn register_full(base: &str, username: &str, email: &str) -> (String, i64) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/users/register", base))
        .json(&serde_json::json!({"username": username, "email": email, "password": PW}))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "register failed: {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    (
        body["token"].as_str().unwrap().to_string(),
        body["user_id"].as_i64().unwrap(),
    )
}

#[tokio::test]
async fn test_create_and_list_org() {
    let base = spawn_test_app().await;
    let token = register_user(&base, "orgowner", "orgowner@example.com", PW).await;
    let client = reqwest::Client::new();

    // Create org — exercises the `organizations` table.
    let resp = client
        .post(format!("{}/api/v1/orgs", base))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "acme", "display_name": "Acme Inc"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_org should succeed");
    let org: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(org["name"], "acme");

    // List orgs — should contain the new org.
    let resp = client
        .get(format!("{}/api/v1/orgs", base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let orgs: serde_json::Value = resp.json().await.unwrap();
    let names: Vec<&str> = orgs
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|o| o["name"].as_str())
        .collect();
    assert!(
        names.contains(&"acme"),
        "list_orgs should include 'acme', got {names:?}"
    );
}

#[tokio::test]
async fn test_add_org_member() {
    let base = spawn_test_app().await;
    let owner_token = register_user(&base, "memowner", "memowner@example.com", PW).await;
    let (_member_token, member_id) = register_full(&base, "teammate", "teammate@example.com").await;
    let client = reqwest::Client::new();

    client
        .post(format!("{}/api/v1/orgs", base))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": "globex"}))
        .send()
        .await
        .unwrap();

    // Add member — exercises the `organization_members` table.
    let resp = client
        .post(format!("{}/api/v1/orgs/globex/members", base))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"user_id": member_id, "role": "member"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "add_org_member should succeed");

    // List members — should include the newly added member.
    let resp = client
        .get(format!("{}/api/v1/orgs/globex/members", base))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let members: serde_json::Value = resp.json().await.unwrap();
    let ids: Vec<i64> = members
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["user_id"].as_i64())
        .collect();
    assert!(
        ids.contains(&member_id),
        "member list should include {member_id}, got {ids:?}"
    );
}

#[tokio::test]
async fn test_create_and_list_team() {
    let base = spawn_test_app().await;
    let token = register_user(&base, "teamowner", "teamowner@example.com", PW).await;
    let client = reqwest::Client::new();

    client
        .post(format!("{}/api/v1/orgs", base))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "initech"}))
        .send()
        .await
        .unwrap();

    // Create team — exercises the `teams` table.
    let resp = client
        .post(format!("{}/api/v1/orgs/initech/teams", base))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "developers", "permission": "write"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_team should succeed");
    let team: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(team["name"], "developers");

    // List teams — should contain the new team.
    let resp = client
        .get(format!("{}/api/v1/orgs/initech/teams", base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let teams: serde_json::Value = resp.json().await.unwrap();
    let names: Vec<&str> = teams
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"developers"),
        "team list should include 'developers', got {names:?}"
    );
}
