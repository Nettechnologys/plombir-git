//! Actual HTTP/M2 policy, including the legacy writer and stored test jobs.
use crate::common::{register_full, spawn_test_app_with_db};
use rg_db::{entities::notification_delivery, notification::Event};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::{json, Value};
const SECRET: &str = "test-secret-key";
const PASSWORD: &str = "Qz7$wRtm";
const TOKEN: &str = "123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi";
const SETTINGS: &str = "/users/me/notification-settings";

async fn send(base: &str, method: &str, path: &str, token: &str, body: Value) -> (u16, Value) {
    let reply = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("{base}/api/v1{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    (
        reply.status().as_u16(),
        reply.json().await.unwrap_or_default(),
    )
}
fn refused(answer: (u16, Value)) {
    assert_eq!(answer.0, 403, "{answer:?}");
    assert_eq!(answer.1["error"]["reason"], "sudo_required", "{answer:?}");
}
fn config() -> Value {
    json!({"telegram":{"action":"set","token":TOKEN,"chat_id":-1001234,"message_thread_id":17}})
}
async fn enrol(base: &str, session: &str) -> Value {
    let (status, setup) = send(base, "POST", "/users/mfa/setup", session, json!({})).await;
    assert_eq!(status, 200, "{setup}");
    let code = totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        totp_rs::Secret::Encoded(setup["secret"].as_str().unwrap().to_owned())
            .to_bytes()
            .unwrap(),
        None,
        String::new(),
    )
    .unwrap()
    .generate_current()
    .unwrap();
    let (status, enabled) = send(
        base,
        "POST",
        "/users/mfa/enable",
        session,
        json!({"password":PASSWORD,"code":code}),
    )
    .await;
    assert_eq!(status, 200, "{enabled}");
    enabled["backup_codes"].clone()
}
async fn elevate(base: &str, session: &str, code: &Value) -> String {
    let (status, reply) = send(
        base,
        "POST",
        "/users/me/sudo",
        session,
        json!({"password":PASSWORD,"backup_code":code}),
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    reply["token"].as_str().unwrap().into()
}
async fn jobs(db: &rg_db::DatabaseConnection, id: i64) -> u64 {
    notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(id))
        .count(db)
        .await
        .unwrap()
}

#[tokio::test]
async fn every_channel_and_telegram_field_requires_current_factor_with_atomic_legacy_defaults() {
    let (base, db) = spawn_test_app_with_db().await;
    let (password_only, id) =
        register_full(&base, "notice_guard", "notice_guard@example.invalid").await;
    let codes = enrol(&base, &password_only).await;
    let before = jobs(&db, id).await;
    refused(
        send(
            &base,
            "PUT",
            SETTINGS,
            &password_only,
            json!({"email":{"mention":false},"locale":"ru"}),
        )
        .await,
    );
    refused(send(&base, "PUT", SETTINGS, &password_only, config()).await);
    assert_eq!(jobs(&db, id).await, before);
    let (_, saved) = send(&base, "GET", SETTINGS, &password_only, json!({})).await;
    assert_eq!(saved["locale"], "en");
    assert_eq!(saved["email"]["mention"], true);
    assert_eq!(saved["revision"], 0);
    // An actual failed SudoPrompt request leaves configuration unchanged too.
    assert_ne!(
        send(
            &base,
            "POST",
            "/users/me/sudo",
            &password_only,
            json!({"password":PASSWORD,"backup_code":"invalid-fixture"})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        send(&base, "GET", SETTINGS, &password_only, json!({}))
            .await
            .1,
        saved
    );
    let fresh = elevate(&base, &password_only, &codes[0]).await;
    let mut all = config();
    all["events"] = Value::Object(
        Event::ALL
            .iter()
            .map(|event| (event.key().into(), json!({"email":true,"telegram":true})))
            .collect(),
    );
    let (status, saved) = send(&base, "PUT", SETTINGS, &fresh, all).await;
    assert_eq!(status, 200, "{saved}");
    assert!(!saved.to_string().contains(TOKEN));
    assert!(!saved.to_string().contains("encrypted"));
    let before = jobs(&db, id).await;
    // Bulk controls must use the same atomic fresh-factor guard as one switch.
    for channel in ["email", "telegram"] {
        let events: serde_json::Map<String, Value> = Event::ALL
            .iter()
            .map(|event| (event.key().into(), json!({channel:false})))
            .collect();
        refused(
            send(
                &base,
                "PUT",
                SETTINGS,
                &password_only,
                json!({"events":events,"locale":"ru"}),
            )
            .await,
        );
        assert_eq!(
            send(&base, "GET", SETTINGS, &password_only, json!({}))
                .await
                .1,
            saved
        );
        assert_eq!(jobs(&db, id).await, before);
    }
    for event in Event::ALL {
        for channel in ["email", "telegram"] {
            refused(
                send(
                    &base,
                    "PUT",
                    SETTINGS,
                    &password_only,
                    json!({"events":{event.key():{channel:false}},"locale":"zh-CN"}),
                )
                .await,
            );
        }
    }
    for event in [
        "review_requested",
        "mention",
        "assigned",
        "ci_failed",
        "participating",
        "ci_triggered",
    ] {
        refused(
            send(
                &base,
                "PUT",
                SETTINGS,
                &password_only,
                json!({"email":{event:false}}),
            )
            .await,
        );
    }
    for change in [
        config()["telegram"].clone(),
        json!({"action":"set","chat_id":-1005678,"message_thread_id":17}),
        json!({"action":"set","chat_id":-1001234,"message_thread_id":18}),
        json!({"action":"set","chat_id":-1001234,"message_thread_id":null}),
        json!({"action":"remove"}),
    ] {
        refused(
            send(
                &base,
                "PUT",
                SETTINGS,
                &password_only,
                json!({"telegram":change}),
            )
            .await,
        );
    }
    assert_eq!(jobs(&db, id).await, before);
    assert_eq!(
        send(&base, "GET", SETTINGS, &password_only, json!({}))
            .await
            .1,
        saved
    );
    let (status, locale) = send(
        &base,
        "PUT",
        SETTINGS,
        &password_only,
        json!({"locale":"ru","telegram":{"action":"keep"}}),
    )
    .await;
    assert_eq!(status, 200, "{locale}");
    assert_eq!(locale["locale"], "ru");
    let (status, reduced) = send(
        &base,
        "PUT",
        SETTINGS,
        &fresh,
        json!({"events":{"new_device":{"telegram":false}}}),
    )
    .await;
    assert_eq!(status, 200, "{reduced}");
    let (status, increased) = send(
        &base,
        "PUT",
        SETTINGS,
        &password_only,
        json!({"events":{"new_device":{"telegram":true}}}),
    )
    .await;
    assert_eq!(status, 200, "{increased}");
}

#[tokio::test]
async fn expired_rotated_revoked_and_pat_proofs_cannot_change_settings() {
    let (base, db) = spawn_test_app_with_db().await;
    let (password, id) = register_full(&base, "notice_proof", "notice_proof@example.invalid").await;
    let codes = enrol(&base, &password).await;
    let first = elevate(&base, &password, &codes[0]).await;
    let mut expired = rg_core::auth::jwt::validate_token(&first, SECRET).unwrap();
    expired.sudo_exp = Some(chrono::Utc::now().timestamp() - 1);
    let expired = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &expired,
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap();
    refused(send(&base, "PUT", SETTINGS, &expired, config()).await);
    let next_codes = enrol(&base, &first).await;
    refused(send(&base, "PUT", SETTINGS, &first, config()).await);
    let second = elevate(&base, &first, &next_codes[0]).await;
    assert_eq!(send(&base, "PUT", SETTINGS, &second, config()).await.0, 200);
    let (status, pat) = send(
        &base,
        "POST",
        "/users/tokens",
        &second,
        json!({"name":"notification-fixture"}),
    )
    .await;
    assert_eq!(status, 201, "{pat}");
    let pat = pat["token"].as_str().unwrap();
    for (method, path, body) in [
        ("GET", SETTINGS, json!({})),
        ("PUT", SETTINGS, json!({"email":{"mention":false}})),
        (
            "POST",
            "/users/me/notification-settings/test",
            json!({"event":"new_device"}),
        ),
    ] {
        let response = send(&base, method, path, pat, body).await;
        assert!(
            response.0 == 401 || response.0 == 403,
            "PAT bypass: {response:?}"
        );
    }
    let before = jobs(&db, id).await;
    rg_db::ops::user_ops::invalidate_sessions(&db, id)
        .await
        .unwrap();
    let response = send(
        &base,
        "PUT",
        SETTINGS,
        &second,
        json!({"email":{"mention":false}}),
    )
    .await;
    assert_eq!(response.0, 401, "{response:?}");
    assert_eq!(response.1["error"]["reason"], "session_revoked");
    assert_eq!(jobs(&db, id).await, before);
}

#[tokio::test]
async fn test_endpoint_uses_saved_config_isolates_identity_and_limits_synthetic_jobs() {
    let (base, db) = spawn_test_app_with_db().await;
    let (fresh, id) = register_full(&base, "notice_test", "notice_test@example.invalid").await;
    let (other, _) = register_full(&base, "notice_other", "notice_other@example.invalid").await;
    // No MFA enrolment required: existing fresh password policy still works.
    let mut patch = config();
    patch["events"] = json!({"new_device":{"telegram":true}});
    assert_eq!(send(&base, "PUT", SETTINGS, &fresh, patch).await.0, 200);
    let user = rg_db::ops::user_ops::find_by_id(&db, id)
        .await
        .unwrap()
        .unwrap();
    let profile = rg_core::notification::settings::get(&db, id)
        .await
        .unwrap()
        .profile;
    let before = jobs(&db, id).await;
    for body in [
        json!({"event":"new_device","chat_id":123}),
        json!({"event":"new_device","token":TOKEN}),
        json!({"event":"unknown"}),
    ] {
        assert!(
            send(
                &base,
                "POST",
                "/users/me/notification-settings/test",
                &fresh,
                body
            )
            .await
            .0 >= 400
        );
    }
    assert_eq!(jobs(&db, id).await, before);
    let (status, test) = send(
        &base,
        "POST",
        "/users/me/notification-settings/test",
        &fresh,
        json!({"event":"new_device"}),
    )
    .await;
    assert_eq!(status, 200, "{test}");
    assert_eq!(test["channels"]["email"]["status"], "unavailable");
    assert_eq!(test["channels"]["telegram"]["status"], "queued");
    let id = test["event_id"].as_str().unwrap();
    let path = format!("/users/me/notification-settings/test?event_id={id}");
    assert_eq!(send(&base, "GET", &path, &fresh, json!({})).await.1, test);
    assert_eq!(send(&base, "GET", &path, &other, json!({})).await.0, 404);
    assert_eq!(
        send(
            &base,
            "POST",
            "/users/me/notification-settings/test",
            &fresh,
            json!({"event":"new_device"})
        )
        .await
        .0,
        429
    );
    assert_eq!(
        rg_db::ops::user_ops::find_by_id(&db, user.id)
            .await
            .unwrap()
            .unwrap(),
        user
    );
    assert_eq!(
        rg_core::notification::settings::get(&db, user.id)
            .await
            .unwrap()
            .profile,
        profile
    );
    let rows = rg_db::ops::notification_delivery_ops::event_rows(&db, user.id, id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.is_test));
    assert!(rows
        .iter()
        .all(|row| !row.metadata.contains(TOKEN) && row.previous_destination_encrypted.is_none()));
    assert!(
        !profile.telegram_confirmed,
        "enqueue alone never confirms delivery"
    );
}
