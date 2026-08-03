use crate::common::{register_full, spawn_test_app_with_db, TEST_ENCRYPTION_KEY};
use chrono::Utc;
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

    assert_eq!(resp.status(), 401);
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
        .unwrap()
        .expect("registered user must exist");

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
        .unwrap()
        .expect("registered user must exist");

    let resp = client
        .get(format!("{}/api/v1/admin/sso/providers", base))
        .header(
            reqwest::header::COOKIE,
            format!("forgekeep_token={}", admin_token),
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
        .unwrap()
        .expect("registered user must exist");

    let create_resp = client
        .post(format!("{}/api/v1/admin/sso/providers", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "name": "Gitea Login",
            "slug": "gitea-login-1",
            // `oidc`, not `oauth2`: plain OAuth2 has no discovery step, so a
            // slug outside the built-in endpoint table could never authorize
            // anybody — which the admin API now refuses instead of storing.
            "provider_type": "oidc",
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
            "provider_type": "oidc",
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
    assert!(
        rg_db::ops::oauth_account_ops::delete_by_id(&db, linked_account.id, admin_id)
            .await
            .unwrap(),
        "the link this test just created must be the row that got deleted"
    );

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
        .unwrap()
        .expect("registered user must exist");

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
    let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
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
    assert_eq!(unauthenticated_test.status(), 401);

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

/// card_d77d2b7e02e6: the completeness question used to be asked of LDAP only.
///
/// An enabled `oauth2` provider with no `client_id` was a `201`, showed up on
/// the login page, and sent the first login off with `client_id=""` — so our
/// unfinished configuration arrived as the IdP's refusal.
///
/// The instance ships `github` / `gitlab` / `google` seeded and disabled, with
/// no `client_id` — so the shortest path to the bug is the one an operator
/// takes on day one: press "Enable" on the GitHub provider and get a `200`
/// back. That is what the first half of this test presses. The counter-example
/// in each pair matters as much as the refusal: a *draft* must stay saveable,
/// or the form becomes impossible to fill in one field at a time.
#[tokio::test]
async fn enabled_oauth2_provider_requires_a_client_id_and_reachable_endpoints() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "oauth_admin", "oauth_admin@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");

    let seeded = rg_db::ops::sso_provider_ops::find_by_slug(&db, "github")
        .await
        .unwrap()
        .expect("the instance ships a disabled GitHub provider");
    assert!(
        seeded.client_id.is_none() && !seeded.enabled,
        "the fixture this test needs is the shipped row: disabled, no client id"
    );

    let create = |payload: serde_json::Value| {
        client
            .post(format!("{}/api/v1/admin/sso/providers", base))
            .bearer_auth(&admin_token)
            .json(&payload)
            .send()
    };
    let patch_github = |payload: serde_json::Value| {
        client
            .patch(format!(
                "{}/api/v1/admin/sso/providers/{}",
                base, seeded.id
            ))
            .bearer_auth(&admin_token)
            .json(&payload)
            .send()
    };

    // Switching the shipped provider on, exactly as the admin UI's "Enable"
    // button does — with nothing to identify us to GitHub.
    let enabled_as_shipped = patch_github(serde_json::json!({
        "name": "GitHub",
        "slug": "github",
        "provider_type": "oauth2",
        "enabled": true
    }))
    .await
    .unwrap();
    assert_eq!(enabled_as_shipped.status(), 400);
    let message = enabled_as_shipped.text().await.unwrap();
    assert!(
        message.contains("client ID"),
        "the refusal must name the field that is missing, got {message}"
    );

    // A blank string is the same emptiness with a value in it.
    let blank_client_id = patch_github(serde_json::json!({
        "name": "GitHub",
        "slug": "github",
        "provider_type": "oauth2",
        "enabled": true,
        "client_id": "   "
    }))
    .await
    .unwrap();
    assert_eq!(blank_client_id.status(), 400);

    // A refused write leaves the row alone — including the flag it was asked
    // to flip.
    let untouched = rg_db::ops::sso_provider_ops::find_by_id(&db, seeded.id)
        .await
        .unwrap()
        .expect("the refused update must not have removed the provider");
    assert!(!untouched.enabled, "a refused enable must not have enabled it");
    assert!(untouched.client_id.is_none());

    // The same body switched off is a draft, and drafts are the normal way to
    // fill this form in one field at a time.
    let draft = patch_github(serde_json::json!({
        "name": "GitHub",
        "slug": "github",
        "provider_type": "oauth2",
        "enabled": false,
        "scopes": "read:user user:email"
    }))
    .await
    .unwrap();
    assert_eq!(draft.status(), 200);

    // Complete, and on a slug the built-in endpoint table knows: accepted with
    // no discovery URL at all.
    let complete = patch_github(serde_json::json!({
        "name": "GitHub",
        "slug": "github",
        "provider_type": "oauth2",
        "enabled": true,
        "client_id": "client-id",
        "scopes": "read:user user:email"
    }))
    .await
    .unwrap();
    assert_eq!(complete.status(), 200);

    // Enabled OIDC without a discovery URL and on a slug the built-in table
    // does not know: `resolve_oidc_endpoints` would bail at the first login.
    let oidc_without_discovery = create(serde_json::json!({
        "name": "Keycloak",
        "slug": "keycloak",
        "provider_type": "oidc",
        "enabled": true,
        "client_id": "client-id"
    }))
    .await
    .unwrap();
    assert_eq!(oidc_without_discovery.status(), 400);

    // Same provider, discovery URL supplied — accepted.
    let oidc_with_discovery = create(serde_json::json!({
        "name": "Keycloak",
        "slug": "keycloak",
        "provider_type": "oidc",
        "enabled": true,
        "client_id": "client-id",
        "discovery_url": "https://idp.example.com/.well-known/openid-configuration"
    }))
    .await
    .unwrap();
    assert_eq!(oidc_with_discovery.status(), 201);

    // Plain OAuth2 has no discovery step, so an unknown slug has no
    // authorization endpoint at all — `oauth2_authorize_url` gives up on it,
    // and the discovery URL supplied here is never read.
    let oauth2_unknown_slug = create(serde_json::json!({
        "name": "Gitea",
        "slug": "gitea",
        "provider_type": "oauth2",
        "enabled": true,
        "client_id": "client-id",
        "discovery_url": "https://gitea.example.com/.well-known/openid-configuration"
    }))
    .await
    .unwrap();
    assert_eq!(oauth2_unknown_slug.status(), 400);

    // A typo in the type used to be stored verbatim and then behave like
    // `oauth2` — including the LDAP checks never running against it.
    let unknown_type = create(serde_json::json!({
        "name": "Directory",
        "slug": "ldpa",
        "provider_type": "ldpa",
        "enabled": true,
        "client_id": "client-id"
    }))
    .await
    .unwrap();
    assert_eq!(unknown_type.status(), 400);
}

/// The other half of card_d77d2b7e02e6: a row that got past the admin API —
/// enabled before the check existed, or written straight into the database —
/// must fail on our side rather than at the IdP.
///
/// The redirect is the request: a `302` to the provider's authorize endpoint
/// carrying `client_id=` is exactly the outcome where the operator reads our
/// empty field as the provider's refusal.
#[tokio::test]
async fn a_provider_stored_without_a_client_id_fails_before_reaching_the_idp() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = rg_db::ops::sso_provider_ops::find_by_slug(&db, "github")
        .await
        .unwrap()
        .expect("the instance ships a disabled GitHub provider");
    rg_db::ops::sso_provider_ops::upsert(
        &db,
        Some(seeded.id),
        rg_db::ops::sso_provider_ops::SsoProviderInput {
            name: &seeded.name,
            slug: &seeded.slug,
            provider_type: &seeded.provider_type,
            client_id: None,
            scopes: seeded.scopes.as_deref(),
            enabled: true,
            ..Default::default()
        },
    )
    .await
    .expect("enable the provider behind the admin API's back");

    // No redirect following: a regression must show up as the 302 it is, not
    // as a request that leaves this machine for github.com.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client
        .get(format!("{}/api/v1/auth/sso/github", base))
        .send()
        .await
        .expect("authorize request");

    assert_eq!(
        response.status(),
        500,
        "a provider we cannot identify ourselves with is our failure, not the IdP's"
    );
    let location = response.headers().get(reqwest::header::LOCATION);
    assert!(
        location.is_none(),
        "the login must not be sent to the provider with an empty client_id, got {location:?}"
    );
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

    // The shared `InstanceAdmin` extractor owns this distinction for every
    // admin route: no session is 401, while a signed-in non-admin is 403.
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_audit_list_and_get_log() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(&base, "auditor", "auditor@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");

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
        .unwrap()
        .expect("registered user must exist");

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
    // See `admin_audit_list_requires_auth`: one shared gate, with 401 for an
    // absent session and 403 only after an authenticated non-admin verdict.
    assert_eq!(unauthenticated.status(), 401);

    let (admin_token, admin_id) =
        register_full(&base, "login_auditor", "login_auditor@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");
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
