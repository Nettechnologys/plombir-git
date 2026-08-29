//! Integration tests for organization & team endpoints.
//!
//! These specifically guard against the singular/plural migration table-name
//! regression (entities expect `organizations`/`teams`/`organization_members`,
//! while the phase8 migration originally created singular tables). Before the
//! `m20260616_0000015_rename_org_team_plural` fix, every assertion here would
//! fail at runtime with "no such table".

use sea_orm::{ConnectionTrait, Statement};

use crate::common::{register_user, spawn_test_app, spawn_test_app_with_db};

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
async fn update_deleted_after_the_scope_read_is_404() {
    let (base, db) = spawn_test_app_with_db().await;
    let token = register_user(&base, "orgupdateowner", "orgupdateowner@example.com", PW).await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "org-update-race"}))
        .send()
        .await
        .expect("create organization");
    assert_eq!(created.status(), 201);
    let organization = created
        .json::<serde_json::Value>()
        .await
        .expect("decode organization");
    let org_id = organization["id"].as_i64().expect("organization id");

    let updated = client
        .patch(format!("{base}/api/v1/orgs/org-update-race"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "display_name": "Before the race",
            "description": "ordinary PATCH still works",
            "visibility": "private"
        }))
        .send()
        .await
        .expect("update organization normally");
    assert_eq!(updated.status(), 200);
    let updated = updated
        .json::<serde_json::Value>()
        .await
        .expect("decode updated organization");
    assert_eq!(updated["display_name"], "Before the race");
    assert_eq!(updated["description"], "ordinary PATCH still works");
    assert_eq!(updated["visibility"], "private");

    // The trigger runs inside the real UPDATE statement, after OrgAdmin has
    // already resolved and authorized the row. It makes the interleaving
    // deterministic without a timing race or a production-only test seam.
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_org_before_update \
             BEFORE UPDATE ON organizations WHEN OLD.id = {org_id} \
             BEGIN DELETE FROM organizations WHERE id = OLD.id; END"
        ),
    ))
    .await
    .expect("install the competing organization delete");

    let raced = client
        .patch(format!("{base}/api/v1/orgs/org-update-race"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"display_name": "Too late"}))
        .send()
        .await
        .expect("race organization update against delete");
    assert_eq!(
        raced.status(),
        404,
        "a DELETE after the scoped read must stay a typed missing resource"
    );
    assert!(
        rg_db::ops::org_ops::get_org(&db, org_id)
            .await
            .expect("read organization after the race")
            .is_none(),
        "the losing PATCH must not recreate the deleted organization"
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

/// Taking somebody back out of an organization — the other half of
/// `test_add_org_member`, and the one no test named.
///
/// Membership is what the permission cache answers repository access from, so
/// a removal that reports success without deleting the row leaves a former
/// colleague reading private repositories, and the member list — the only place
/// an owner can check — keeps saying they belong.
#[tokio::test]
async fn test_remove_org_member() {
    let base = spawn_test_app().await;
    let owner_token = register_user(&base, "exitowner", "exitowner@example.com", PW).await;
    let (member_token, member_id) = register_full(&base, "leaver", "leaver@example.com").await;
    let client = reqwest::Client::new();

    client
        .post(format!("{}/api/v1/orgs", base))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": "hooli"}))
        .send()
        .await
        .unwrap();
    let added = client
        .post(format!("{}/api/v1/orgs/hooli/members", base))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"user_id": member_id, "role": "member"}))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), 201, "baseline: the member is in");

    // A plain member is not an org admin, so they cannot show anybody the door
    // — including themselves.
    let by_member = client
        .delete(format!("{}/api/v1/orgs/hooli/members/{}", base, member_id))
        .bearer_auth(&member_token)
        .send()
        .await
        .unwrap();
    assert_eq!(by_member.status(), 403);

    let removed = client
        .delete(format!("{}/api/v1/orgs/hooli/members/{}", base, member_id))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), 200);
    assert_eq!(
        removed.json::<serde_json::Value>().await.unwrap()["removed"],
        serde_json::json!(true)
    );

    let members = client
        .get(format!("{}/api/v1/orgs/hooli/members", base))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(members.status(), 200);
    let ids: Vec<i64> = members
        .json::<serde_json::Value>()
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["user_id"].as_i64())
        .collect();
    assert!(
        !ids.contains(&member_id),
        "the member list must stop naming {member_id}, got {ids:?}"
    );

    // Nothing was removed the second time, and saying otherwise would let a
    // UI report a departure that never happened.
    let again = client
        .delete(format!("{}/api/v1/orgs/hooli/members/{}", base, member_id))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);
}
