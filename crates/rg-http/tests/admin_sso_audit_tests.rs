mod common;

use chrono::Utc;
use common::{register_full, spawn_test_app_with_db};
use sea_orm::{ActiveValue, Set};

#[tokio::test]
async fn admin_sso_list_requires_auth() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/admin/sso/providers", base))
        .send()
        .await
        .unwrap();

    assert!(resp.status() == 401 || resp.status() == 403);
}

#[tokio::test]
async fn admin_sso_requires_admin() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (user_token, _user_id) = register_full(&base, "sso_user", "sso_user@example.com").await;
    let (admin_token, admin_id) = register_full(&base, "sso_admin", "sso_admin@example.com").await;

    let nonadmin_resp = client
        .get(format!("{}/api/v1/admin/sso/providers", base))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(nonadmin_resp.status(), 403);

    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let admin_resp = client
        .get(format!("{}/api/v1/admin/sso/providers", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_resp.status(), 200);
    let list: serde_json::Value = admin_resp.json().await.unwrap();
    assert!(list.is_array());
}

#[tokio::test]
async fn admin_sso_accepts_httponly_cookie_without_bearer() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "sso_cookie", "sso_cookie@example.com").await;

    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let resp = client
        .get(format!("{}/api/v1/admin/sso/providers", base))
        .header(
            reqwest::header::COOKIE,
            format!("ironforge_token={}", admin_token),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn admin_sso_create_get_update_delete() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(&base, "sso_crud", "sso_crud@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let create_resp = client
        .post(format!("{}/api/v1/admin/sso/providers", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "name": "Gitea Login",
            "slug": "gitea-login-1",
            "provider_type": "oauth2",
            "enabled": true,
            "client_id": "client-id",
            "client_secret": "secret",
            "discovery_url": "https://example.com/.well-known/openid-configuration",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 201);
    let created: serde_json::Value = create_resp.json().await.unwrap();
    assert_eq!(created["slug"], "gitea-login-1");
    let provider_id = created["id"].as_i64().unwrap();

    let unsupported_test = client
        .post(format!(
            "{}/api/v1/admin/sso/providers/{}/test",
            base, provider_id
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(unsupported_test.status(), 400);

    let list_resp = client
        .get(format!("{}/api/v1/admin/sso/providers", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(list_resp.status(), 200);
    let list: serde_json::Value = list_resp.json().await.unwrap();
    let list_items = list.as_array().unwrap();
    assert!(list_items.iter().any(|item| item["id"] == provider_id));

    let get_resp = client
        .get(format!(
            "{}/api/v1/admin/sso/providers/{}",
            base, provider_id
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let got: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(got["id"], provider_id);

    let update_resp = client
        .patch(format!(
            "{}/api/v1/admin/sso/providers/{}",
            base, provider_id
        ))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "name": "Gitea Login Updated",
            "slug": "gitea-login-1",
            "provider_type": "oauth2",
            "enabled": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(update_resp.status(), 200);
    let updated: serde_json::Value = update_resp.json().await.unwrap();
    assert_eq!(updated["name"], "Gitea Login Updated");
    assert_eq!(updated["enabled"], false);

    let linked_account = rg_db::ops::oauth_account_ops::upsert(
        &db,
        admin_id,
        "gitea-login-1",
        "provider-user-1",
        "sso_crud",
        "sso_crud@example.com",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        rg_db::ops::oauth_account_ops::count_by_provider(&db, "gitea-login-1")
            .await
            .unwrap(),
        1
    );

    let linked_delete = client
        .delete(format!(
            "{}/api/v1/admin/sso/providers/{}",
            base, provider_id
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(linked_delete.status(), 400);
    rg_db::ops::oauth_account_ops::delete_by_id(&db, linked_account.id, admin_id)
        .await
        .unwrap();

    let del_resp = client
        .delete(format!(
            "{}/api/v1/admin/sso/providers/{}",
            base, provider_id
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    let delete_status = del_resp.status();
    let delete_body = del_resp.text().await.unwrap();
    assert_eq!(delete_status, 200, "{delete_body}");
    let body: serde_json::Value = serde_json::from_str(&delete_body).unwrap();
    assert_eq!(body["deleted"], true);

    let get_after = client
        .get(format!(
            "{}/api/v1/admin/sso/providers/{}",
            base, provider_id
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_after.status(), 404);
}

#[tokio::test]
async fn enabled_ldap_provider_requires_safe_complete_configuration() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "ldap_admin", "ldap_admin@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let incomplete = client
        .post(format!("{}/api/v1/admin/sso/providers", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "name": "Directory",
            "slug": "directory",
            "provider_type": "ldap",
            "enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(incomplete.status(), 400);

    let invalid_filter = client
        .post(format!("{}/api/v1/admin/sso/providers", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "name": "Directory",
            "slug": "directory",
            "provider_type": "ldap",
            "enabled": true,
            "ldap_host": "ldap://127.0.0.1",
            "ldap_port": 1,
            "ldap_bind_dn": "cn=service,dc=example,dc=com",
            "ldap_bind_password": "bind-secret",
            "ldap_base_dn": "dc=example,dc=com",
            "ldap_user_filter": "(objectClass=person)"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_filter.status(), 400);

    let valid = client
        .post(format!("{}/api/v1/admin/sso/providers", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "name": "Directory",
            "slug": "directory",
            "provider_type": "ldap",
            "enabled": true,
            "ldap_host": "ldap://127.0.0.1",
            "ldap_port": 1,
            "ldap_bind_dn": "cn=service,dc=example,dc=com",
            "ldap_bind_password": "bind-secret",
            "ldap_base_dn": "dc=example,dc=com",
            "ldap_user_filter": "(uid={username})"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(valid.status(), 201);
    let response: serde_json::Value = valid.json().await.unwrap();
    assert!(response.get("ldap_bind_password").is_none());
    let stored = rg_db::ops::sso_provider_ops::find_by_slug(&db, "directory")
        .await
        .unwrap()
        .unwrap();
    let encrypted = stored.ldap_bind_password_enc.unwrap();
    assert_ne!(encrypted, "bind-secret");
    let key = rg_core::auth::encryption::derive_key("test-secret-key");
    assert_eq!(
        rg_core::auth::encryption::decrypt(&encrypted, &key).unwrap(),
        "bind-secret"
    );

    let unauthenticated_test = client
        .post(format!(
            "{}/api/v1/admin/sso/providers/{}/test",
            base, stored.id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated_test.status(), 403);

    let failed_test = client
        .post(format!(
            "{}/api/v1/admin/sso/providers/{}/test",
            base, stored.id
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(failed_test.status(), 400);
    let failed_body = failed_test.text().await.unwrap();
    assert!(failed_body.contains("LDAP connection test failed"));
    assert!(!failed_body.contains("127.0.0.1"));
    assert!(!failed_body.contains("bind-secret"));

    rg_db::ops::user_ops::create_ldap_user(
        &db,
        stored.id,
        "directory_user",
        "directory_user@example.com",
        Some("Directory User"),
        "uid=directory_user,dc=example,dc=com",
        Some("directory_user"),
    )
    .await
    .unwrap();
    let delete_linked = client
        .delete(format!("{}/api/v1/admin/sso/providers/{}", base, stored.id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(delete_linked.status(), 400);
}

#[tokio::test]
async fn admin_audit_list_requires_auth() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/admin/audit/logs", base))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_audit_list_and_get_log() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(&base, "auditor", "auditor@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let inserted = rg_db::ops::audit_log_ops::insert(
        &db,
        rg_db::entities::audit_log::ActiveModel {
            id: ActiveValue::NotSet,
            user_id: Set(Some(admin_id)),
            username: Set(Some("auditor".to_string())),
            action: Set("admin.sso.create".to_string()),
            resource_type: Set(Some("sso_provider".to_string())),
            resource_id: Set(Some(99)),
            resource_name: Set(Some("gitea-login-1".to_string())),
            ip_address: Set(Some("127.0.0.1".to_string())),
            user_agent: Set(Some("rg-http-tests".to_string())),
            details: Set(Some("{}".to_string())),
            created_at: Set(Utc::now()),
        },
    )
    .await
    .unwrap();

    let list_resp = client
        .get(format!(
            "{}/api/v1/admin/audit/logs?page=0&page_size=10",
            base
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(list_resp.status(), 200);
    let list: serde_json::Value = list_resp.json().await.unwrap();
    assert!(list["total"].as_u64().unwrap_or(0) >= 1);
    let logs = list["logs"].as_array().expect("logs array");
    assert!(logs.iter().any(|item| item["action"] == "admin.sso.create"));

    let get_resp = client
        .get(format!("{}/api/v1/admin/audit/logs/{}", base, inserted.id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let got: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(got["id"], inserted.id);
    assert_eq!(got["action"], "admin.sso.create");
}

#[tokio::test]
async fn admin_audit_get_not_found() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "auditor_nf", "auditor_nf@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();

    let resp = client
        .get(format!("{}/api/v1/admin/audit/logs/987654", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn admin_login_attempts_are_protected_paginated_and_filterable() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let unauthenticated = client
        .get(format!("{}/api/v1/admin/login-attempts", base))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), 401);

    let (admin_token, admin_id) =
        register_full(&base, "login_auditor", "login_auditor@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap();
    rg_db::ops::login_log_ops::log_attempt(
        &db,
        Some(admin_id),
        "login_auditor",
        "password",
        Some("192.0.2.1"),
        Some("test-agent"),
        false,
        Some("invalid_credentials"),
    )
    .await
    .unwrap();
    rg_db::ops::login_log_ops::log_attempt(
        &db,
        Some(admin_id),
        "login_auditor",
        "password",
        Some("192.0.2.1"),
        Some("test-agent"),
        true,
        None,
    )
    .await
    .unwrap();
    let bounded = rg_db::ops::login_log_ops::log_attempt(
        &db,
        Some(admin_id),
        &"u".repeat(300),
        &"provider".repeat(10),
        Some(&"1".repeat(80)),
        Some(&"agent".repeat(200)),
        false,
        Some(&"reason".repeat(100)),
    )
    .await
    .unwrap();
    assert_eq!(bounded.username.chars().count(), 255);
    assert_eq!(bounded.auth_provider.chars().count(), 20);
    assert_eq!(bounded.ip_address.as_deref().unwrap().chars().count(), 45);
    assert_eq!(bounded.user_agent.as_deref().unwrap().chars().count(), 512);
    assert_eq!(
        bounded.failure_reason.as_deref().unwrap().chars().count(),
        255
    );

    let response = client
        .get(format!(
            "{}/api/v1/admin/login-attempts?page=1&per_page=1&username=login_auditor&auth_provider=password&success=false",
            base
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["page"], 1);
    assert_eq!(body["per_page"], 1);
    assert_eq!(body["total"], 1);
    assert_eq!(body["attempts"].as_array().unwrap().len(), 1);
    let attempt = &body["attempts"][0];
    assert_eq!(attempt["username"], "login_auditor");
    assert_eq!(attempt["success"], false);
    assert_eq!(attempt["failure_reason"], "invalid_credentials");
    assert_eq!(attempt["ip_address"], "192.0.2.1");
    assert_eq!(attempt["user_agent"], "test-agent");

    let invalid_time = client
        .get(format!("{}/api/v1/admin/login-attempts", base))
        .bearer_auth(&admin_token)
        .query(&[("start_time", "not-a-time")])
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_time.status(), 400);

    let reversed_time = client
        .get(format!("{}/api/v1/admin/login-attempts", base))
        .bearer_auth(&admin_token)
        .query(&[
            ("start_time", "2026-07-12T12:00:00Z"),
            ("end_time", "2026-07-12T11:00:00Z"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(reversed_time.status(), 400);

    let oversized_provider = client
        .get(format!("{}/api/v1/admin/login-attempts", base))
        .bearer_auth(&admin_token)
        .query(&[("auth_provider", "x".repeat(21))])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized_provider.status(), 400);
}
