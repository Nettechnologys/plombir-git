//! Every value the SSO provider form accepts fits its `sso_providers` column on
//! every backend (card_b363c2b4d07d).
//!
//! The client secret and the LDAP bind password are sealed before they are
//! stored, and AES-GCM plus base64 grows them by about a third; the columns were
//! `VARCHAR(1024)` on MySQL and PostgreSQL, and nothing bounded the input. The
//! plain columns have declared widths as well, so a long name or bind DN used
//! to pass validation and fail the insert as a 500. Both now answer 400 before
//! anything is sealed or written, and the largest accepted secret reads back.

use crate::common::{
    register_full, spawn_test_app_over_db, spawn_test_app_with_db, TEST_ENCRYPTION_KEY,
};
use rg_http::api::admin::MAX_SSO_SECRET_BYTES;

fn open(sealed: &str) -> String {
    let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
    rg_core::auth::encryption::decrypt(sealed, &key).expect("open stored SSO secret")
}

async fn stored(db: &rg_db::DatabaseConnection, id: i64) -> rg_db::entities::sso_provider::Model {
    rg_db::ops::sso_provider_ops::find_by_id(db, id)
        .await
        .expect("read SSO provider")
        .expect("the SSO provider exists")
}

async fn exercise(base: &str, db: &rg_db::DatabaseConnection) {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("ssolimit{suffix}");
    let (token, admin_id) =
        register_full(base, &username, &format!("{username}@example.invalid")).await;
    rg_db::ops::user_ops::update_by_id(db, admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");
    let client = reqwest::Client::new();
    let providers = format!("{base}/api/v1/admin/sso/providers");
    let slug = format!("limit-{suffix}");

    let maximum = "c".repeat(MAX_SSO_SECRET_BYTES);
    let sealed_length = rg_core::auth::encryption::encrypt(
        &maximum,
        &rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY),
    )
    .unwrap()
    .len();
    assert!(
        sealed_length > 1024,
        "the maximum no longer outgrows VARCHAR(1024)"
    );

    // A draft LDAP provider takes both secrets without a reachable host.
    let created = client
        .post(&providers)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "Limit",
            "slug": slug,
            "provider_type": "ldap",
            "enabled": false,
            "client_secret": maximum,
            "ldap_bind_password": maximum,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "{}", created.text().await.unwrap());
    let id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let row = stored(db, id).await;
    assert_eq!(open(row.client_secret_enc.as_deref().unwrap()), maximum);
    assert_eq!(
        open(row.ldap_bind_password_enc.as_deref().unwrap()),
        maximum
    );

    let replacement = "r".repeat(MAX_SSO_SECRET_BYTES);
    let updated = client
        .patch(format!("{providers}/{id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "Limit",
            "slug": slug,
            "provider_type": "ldap",
            "enabled": false,
            "client_secret": replacement,
            "ldap_bind_password": replacement,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(updated.status(), 200, "{}", updated.text().await.unwrap());
    let row = stored(db, id).await;
    assert_eq!(open(row.client_secret_enc.as_deref().unwrap()), replacement);
    assert_eq!(
        open(row.ldap_bind_password_enc.as_deref().unwrap()),
        replacement
    );

    let oversized = "o".repeat(MAX_SSO_SECRET_BYTES + 1);
    for field in ["client_secret", "ldap_bind_password"] {
        let mut body = serde_json::json!({
            "name": "Limit",
            "slug": format!("over-{suffix}"),
            "provider_type": "ldap",
            "enabled": false,
        });
        body[field] = serde_json::json!(oversized);
        let rejected = client
            .post(&providers)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), 400, "create with an oversized {field}");

        body["slug"] = serde_json::json!(slug);
        let rejected = client
            .patch(format!("{providers}/{id}"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), 400, "update with an oversized {field}");
    }
    let row = stored(db, id).await;
    assert_eq!(open(row.client_secret_enc.as_deref().unwrap()), replacement);
    assert!(
        rg_db::ops::sso_provider_ops::find_by_slug(db, &format!("over-{suffix}"))
            .await
            .unwrap()
            .is_none(),
        "a refused create stored a provider"
    );

    // The plain columns: at their width the value is stored, one past is 400.
    for (field, width) in [("name", 50), ("ldap_bind_dn", 512), ("scopes", 255)] {
        let mut body = serde_json::json!({
            "name": "Limit",
            "slug": slug,
            "provider_type": "ldap",
            "enabled": false,
        });
        body[field] = serde_json::json!("é".repeat(width));
        let accepted = client
            .patch(format!("{providers}/{id}"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), 200, "{field} at its column width");
        body[field] = serde_json::json!("é".repeat(width + 1));
        let rejected = client
            .patch(format!("{providers}/{id}"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), 400, "{field} past its column width");
    }
}

#[tokio::test]
async fn sso_secret_and_column_limits_are_checked_before_storage() {
    let (base, db) = spawn_test_app_with_db().await;
    exercise(&base, &db).await;
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
async fn server_database_sso_secret_and_column_limits_are_checked_before_storage() {
    let _shared = crate::common::SHARED_SERVER_DB.lock().await;
    let database_url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    assert!(database_url.starts_with("postgres://") || database_url.starts_with("mysql://"));
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to disposable server database");
    rg_db::run_migrations(&db)
        .await
        .expect("migrate disposable server database");
    let base = spawn_test_app_over_db(db.clone()).await;
    exercise(&base, &db).await;
}
