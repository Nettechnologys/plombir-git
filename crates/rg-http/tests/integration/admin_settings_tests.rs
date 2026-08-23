use crate::common::{register_full, spawn_test_app_over_db, spawn_test_app_with_db};

/// Promote a freshly registered user to instance admin and hand back its token.
async fn admin_token(base: &str, db: &rg_db::DatabaseConnection, name: &str) -> String {
    let (token, id) = register_full(base, name, &format!("{name}@example.com")).await;
    rg_db::ops::user_ops::update_by_id(db, id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");
    token
}

#[tokio::test]
async fn admin_settings_list_requires_auth() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/admin/settings", base))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_settings_requires_admin() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, _admin_id) =
        register_full(&base, "settings_admin", "settings_admin@example.com").await;
    let (user_token, _user_id) =
        register_full(&base, "settings_user", "settings_user@example.com").await;

    let normal_resp = client
        .get(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(normal_resp.status(), 403);

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
        .unwrap()
        .expect("registered user must exist");

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

/// The prod half of card_08bab0b46e40: settings used to live only in a
/// process-global `RwLock`, so `PATCH /admin/settings` wrote nothing durable and
/// a restart silently reverted every switch — worst of all maintenance mode,
/// which gets turned on precisely when a restart is imminent.
#[tokio::test]
async fn admin_settings_survive_a_restart() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let token = admin_token(&base, &db, "settings_restart_admin").await;

    let update_resp = client
        .patch(format!("{}/api/v1/admin/settings", base))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "maintenance_mode": true,
            "banner_message": "Upgrading the storage backend",
            "banner_type": "warning",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(update_resp.status(), 200);

    // Straight at the database, so the assertion does not depend on any cache
    // the server may or may not be holding.
    let row = rg_db::ops::instance_settings_ops::find(&db)
        .await
        .unwrap()
        .expect("PATCH /admin/settings must write the instance_settings row");
    assert!(row.maintenance_mode);
    assert_eq!(
        row.banner_message.as_deref(),
        Some("Upgrading the storage backend")
    );
    assert_eq!(row.banner_type, "warning");

    // A second server over the same database: fresh state, cold caches. The
    // admin registered above is reused rather than a new one registered here —
    // both instances share the database and the test JWT secret, and the
    // restarted one comes up in maintenance mode, which (correctly) refuses the
    // POST that registration is.
    let restarted = spawn_test_app_over_db(db.clone()).await;
    let get_resp = client
        .get(format!("{}/api/v1/admin/settings", restarted))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let got: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(
        got["maintenance_mode"], true,
        "maintenance mode did not survive the restart"
    );
    assert_eq!(got["banner_message"], "Upgrading the storage backend");
    assert_eq!(got["banner_type"], "warning");
}

/// The test-isolation half of the same card: two servers in one process each
/// have their own database, and settings written to one must be invisible to
/// the other. While the settings lived in a `static`, this failed — and so did
/// every pair of tests in this file that touched them concurrently.
#[tokio::test]
async fn settings_written_by_one_instance_are_invisible_to_another() {
    let client = reqwest::Client::new();

    let (base_a, db_a) = spawn_test_app_with_db().await;
    let (base_b, db_b) = spawn_test_app_with_db().await;
    let token_a = admin_token(&base_a, &db_a, "settings_isolation_a").await;
    let token_b = admin_token(&base_b, &db_b, "settings_isolation_b").await;

    // Read B's settings first, so the shared-global version of this bug would
    // also have to survive B having already cached a value.
    let before: serde_json::Value = client
        .get(format!("{}/api/v1/admin/settings", base_b))
        .bearer_auth(&token_b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before["maintenance_mode"], false);

    let resp = client
        .patch(format!("{}/api/v1/admin/settings", base_a))
        .bearer_auth(&token_a)
        .json(&serde_json::json!({
            "maintenance_mode": true,
            "banner_message": "Only instance A said this",
            "banner_type": "error",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let after: serde_json::Value = client
        .get(format!("{}/api/v1/admin/settings", base_b))
        .bearer_auth(&token_b)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        after["maintenance_mode"], false,
        "instance B saw a setting only instance A was told about"
    );
    assert!(
        after["banner_message"].is_null(),
        "instance B picked up instance A's banner: {}",
        after["banner_message"]
    );

    // And A really did keep what it was told.
    assert!(
        rg_db::ops::instance_settings_ops::find(&db_a)
            .await
            .unwrap()
            .expect("instance A must have a settings row")
            .maintenance_mode
    );
    assert!(
        rg_db::ops::instance_settings_ops::find(&db_b)
            .await
            .unwrap()
            .is_none(),
        "instance B was never configured, so it must have no settings row"
    );
}

#[tokio::test]
async fn admin_settings_non_admin_post() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, _admin_id) = register_full(
        &base,
        "settings_post_admin",
        "settings_post_admin@example.com",
    )
    .await;
    let (user_token, _user_id) = register_full(
        &base,
        "settings_post_user",
        "settings_post_user@example.com",
    )
    .await;

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
