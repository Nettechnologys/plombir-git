mod common;

use common::{register_full, spawn_test_app_with_db};

#[tokio::test]
async fn runner_register_requires_admin() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let unauth_resp = client
        .post(format!("{}/api/v1/runners/register", base))
        .json(&serde_json::json!({"name": "unauth-runner"}))
        .send()
        .await
        .unwrap();
    assert!(unauth_resp.status() == 401 || unauth_resp.status() == 403);

    let (user_token, _user_id) =
        register_full(&base, "runner_user", "runner_user@example.com").await;
    let user_resp = client
        .post(format!("{}/api/v1/runners/register", base))
        .bearer_auth(&user_token)
        .json(&serde_json::json!({"name": "user-runner"}))
        .send()
        .await
        .unwrap();
    assert!(user_resp.status() == 401 || user_resp.status() == 403);

    let (admin_token, admin_id) =
        register_full(&base, "runner_admin", "runner_admin@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let admin_resp = client
        .post(format!("{}/api/v1/runners/register", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"name": "admin-runner"}))
        .send()
        .await
        .unwrap();
    assert_eq!(admin_resp.status(), 201);

    let body: serde_json::Value = admin_resp.json().await.unwrap();
    assert!(body["id"].as_i64().is_some());
    assert!(body["token"].as_str().is_some());
}

#[tokio::test]
async fn runner_register_accepts_admin_httponly_cookie() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "runner_cookie", "runner_cookie@example.com").await;

    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let resp = client
        .post(format!("{}/api/v1/runners/register", base))
        .header(
            reqwest::header::COOKIE,
            format!("ironforge_token={}", admin_token),
        )
        .json(&serde_json::json!({"name": "cookie-runner"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
}
