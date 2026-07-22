mod common;

use common::{register_full, spawn_test_app_with_db};

#[tokio::test]
async fn admin_orgs_list_requires_auth() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/admin/orgs", base))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn admin_orgs_list_requires_admin() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (user_token, _) = register_full(&base, "org_member", "org_member@example.com").await;
    let (admin_token, admin_id) = register_full(&base, "org_admin", "org_admin@example.com").await;

    let user_org_create = client
        .post(format!("{}/api/v1/orgs", base))
        .bearer_auth(&user_token)
        .json(&serde_json::json!({ "name": "normal-org", "display_name": "Normal Org" }))
        .send()
        .await
        .unwrap();
    assert_eq!(user_org_create.status(), 201);

    let admin_org_create = client
        .post(format!("{}/api/v1/orgs", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({ "name": "admin-org", "display_name": "Admin Org" }))
        .send()
        .await
        .unwrap();
    assert_eq!(admin_org_create.status(), 201);

    let normal_resp = client
        .get(format!("{}/api/v1/admin/orgs", base))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(normal_resp.status(), 403);

    // promote org_admin to admin in db for the authorization check
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let admin_resp = client
        .get(format!("{}/api/v1/admin/orgs", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_resp.status(), 200);
    let list: serde_json::Value = admin_resp.json().await.unwrap();
    let items = list["data"]
        .as_array()
        .expect("admin org list should have data array");

    let names: Vec<&str> = items
        .iter()
        .filter_map(|item| item["name"].as_str())
        .collect();
    assert!(names.contains(&"normal-org"));
    assert!(names.contains(&"admin-org"));
    assert_eq!(list["pagination"]["total"], 2);
}

#[tokio::test]
async fn admin_orgs_get_not_found() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "admin_org_get", "admin_org_get@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let get_resp = client
        .get(format!("{}/api/v1/admin/orgs/nope-org", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();

    assert_eq!(get_resp.status(), 404);
}

#[tokio::test]
async fn admin_orgs_delete() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (owner_token, _) = register_full(
        &base,
        "admin_del_org_owner",
        "admin_del_org_owner@example.com",
    )
    .await;
    let (admin_token, admin_id) =
        register_full(&base, "admin_del_org", "admin_del_org@example.com").await;

    let create_resp = client
        .post(format!("{}/api/v1/orgs", base))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "name": "victim-org", "display_name": "Victim Org" }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 201);

    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let del_resp = client
        .delete(format!("{}/api/v1/admin/orgs/victim-org", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del_resp.status(), 200);
    let body: serde_json::Value = del_resp.json().await.unwrap();
    assert_eq!(body["deleted"], true);

    let get_after = client
        .get(format!("{}/api/v1/admin/orgs/victim-org", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_after.status(), 404);
}
