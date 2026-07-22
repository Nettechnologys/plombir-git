mod common;

use common::{register_full, spawn_test_app_with_db};

#[tokio::test]
async fn admin_settings_list_requires_auth() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/admin/settings", base))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn admin_settings_requires_admin() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (user_token, _user_id) =
        register_full(&base, "settings_user", "settings_user@example.com").await;
    let (admin_token, admin_id) =
        register_full(&base, "settings_admin", "settings_admin@example.com").await;

    let normal_resp = client
        .get(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(normal_resp.status(), 403);

    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let admin_resp = client
        .get(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_resp.status(), 200);
    let body: serde_json::Value = admin_resp.json().await.unwrap();
    assert!(body.get("maintenance_mode").is_some());
    assert!(body.get("banner_type").is_some());
}

#[tokio::test]
async fn admin_settings_update_and_restore() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(
        &base,
        "settings_admin_update",
        "settings_admin_update@example.com",
    )
    .await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let baseline_resp = client
        .get(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(baseline_resp.status(), 200);
    let baseline: serde_json::Value = baseline_resp.json().await.unwrap();
    let old_maintenance = baseline["maintenance_mode"].as_bool().unwrap_or(false);
    let old_banner_message = baseline
        .get("banner_message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let old_banner_type = baseline["banner_type"].as_str().unwrap_or("").to_string();

    let update_resp = client
        .patch(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "maintenance_mode": !old_maintenance,
            "banner_message": "Maintenance scheduled",
            "banner_type": "warning",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(update_resp.status(), 200);
    let updated: serde_json::Value = update_resp.json().await.unwrap();
    assert_eq!(updated["maintenance_mode"], !old_maintenance);
    assert_eq!(updated["banner_message"], "Maintenance scheduled");
    assert_eq!(updated["banner_type"], "warning");

    let get_resp = client
        .get(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let got: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(got["maintenance_mode"], !old_maintenance);
    assert_eq!(got["banner_message"], "Maintenance scheduled");
    assert_eq!(got["banner_type"], "warning");

    // restore
    let restore_resp = client
        .patch(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "maintenance_mode": old_maintenance,
            "banner_message": old_banner_message,
            "banner_type": old_banner_type,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(restore_resp.status(), 200);
    let restored: serde_json::Value = restore_resp.json().await.unwrap();
    assert_eq!(restored["maintenance_mode"], old_maintenance);
    if old_banner_message.is_empty() {
        assert!(
            restored.get("banner_message").is_none()
                || restored["banner_message"].is_null()
                || restored["banner_message"] == ""
        );
    } else {
        assert_eq!(restored["banner_message"], old_banner_message);
    }
}

#[tokio::test]
async fn admin_settings_non_admin_post() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (user_token, _user_id) = register_full(
        &base,
        "settings_post_user",
        "settings_post_user@example.com",
    )
    .await;
    let (admin_token, _admin_id) = register_full(
        &base,
        "settings_post_admin",
        "settings_post_admin@example.com",
    )
    .await;

    rg_db::ops::user_ops::update_by_id(&db, _admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let blocked_resp = client
        .patch(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&user_token)
        .json(&serde_json::json!({
            "maintenance_mode": true,
            "banner_type": "error",
            "banner_message": "Should not work",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_resp.status(), 403);

    // admin path should work
    let ok_resp = client
        .patch(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "maintenance_mode": false,
            "banner_message": "",
            "banner_type": "info",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_resp.status(), 200);
}
