//! Completed login events through real routes. All messages remain DB rows.
use crate::common::{register_full, spawn_test_app_with_db};
use rg_db::entities::notification_delivery;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::{json, Value};
const PW: &str = "Qz7$wRtm";
const COOKIE: &str = rg_core::notification::browser::COOKIE_NAME;

pub(super) async fn count(db: &rg_db::DatabaseConnection, user_id: i64) -> u64 {
    notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(user_id))
        .filter(notification_delivery::Column::EventKey.eq("new_device"))
        .count(db)
        .await
        .unwrap()
}
fn cookie(response: &reqwest::Response, name: &str) -> String {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .find(|h| h.starts_with(&format!("{name}=")))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}
async fn post(base: &str, path: &str, body: Value, cookies: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1{path}"))
        .header(reqwest::header::COOKIE, cookies)
        .json(&body)
        .send()
        .await
        .unwrap()
}
async fn enrol(base: &str, session: &str) -> Value {
    let client = reqwest::Client::new();
    let setup: Value = client
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(session)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        totp_rs::Secret::Encoded(setup["secret"].as_str().unwrap().into())
            .to_bytes()
            .unwrap(),
        None,
        String::new(),
    )
    .unwrap()
    .generate_current()
    .unwrap();
    let enabled = client
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(session)
        .json(&json!({"password":PW,"code":code}))
        .send()
        .await
        .unwrap();
    assert_eq!(enabled.status(), 200);
    enabled.json::<Value>().await.unwrap()["backup_codes"].clone()
}

#[tokio::test]
async fn witness_less_login_always_alerts_known_cookie_does_not_and_api_refresh_are_not_logins() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, id) = register_full(&base, "browser_http", "browser_http@example.invalid").await;
    assert_eq!(
        count(&db, id).await,
        1,
        "registration issued a complete session"
    );
    let response = post(
        &base,
        "/users/login",
        json!({"login":"browser_http","password":"wrong"}),
        "",
    )
    .await;
    assert_eq!(response.status(), 401);
    assert_eq!(count(&db, id).await, 1);
    let login = json!({"login":"browser_http","password":PW});
    let response = post(&base, "/users/login", login.clone(), "").await;
    assert_eq!(response.status(), 200);
    let witness = cookie(&response, COOKIE);
    assert_eq!(count(&db, id).await, 2);
    let response = post(&base, "/users/login", login.clone(), &witness).await;
    assert_eq!(response.status(), 200);
    assert!(
        !response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .any(|h| h.to_str().unwrap().starts_with(COOKIE)),
        "known evidence is not renewed"
    );
    assert_eq!(count(&db, id).await, 2);
    assert_eq!(
        post(&base, "/users/login", login.clone(), "")
            .await
            .status(),
        200
    );
    assert_eq!(count(&db, id).await, 3, "no daily witness-less collapse");
    assert_eq!(
        post(&base, "/users/login", login, &format!("{witness}tampered"))
            .await
            .status(),
        200
    );
    assert_eq!(count(&db, id).await, 4);
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{base}/api/v1/users/me"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let refreshed = client
        .post(format!("{base}/api/v1/users/me/session/refresh"))
        .header(reqwest::header::ORIGIN, &base)
        .header(
            reqwest::header::COOKIE,
            format!(
                "plombir_git_token={}",
                rg_core::auth::jwt::with_browser_session(&token, Some(true), "test-secret-key")
                    .unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(refreshed.status(), 200);
    let pat: Value = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&token)
        .json(&json!({"name":"fixture","scopes":"repo"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let _ = client
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(pat["token"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(
        count(&db, id).await,
        4,
        "ordinary API/PAT/refresh must not register devices"
    );
}

#[tokio::test]
async fn mfa_first_factor_and_failed_code_do_not_alert_but_completed_proof_does() {
    let (base, db) = spawn_test_app_with_db().await;
    let (session, id) = register_full(&base, "browser_mfa", "browser_mfa@example.invalid").await;
    let codes = enrol(&base, &session).await;
    let initial = count(&db, id).await;
    let response = post(
        &base,
        "/users/login",
        json!({"login":"browser_mfa","password":PW}),
        "",
    )
    .await;
    assert_eq!(response.status(), 200);
    let cookies = format!(
        "{}; {}",
        cookie(&response, "plombir_git_mfa_challenge"),
        cookie(&response, COOKIE)
    );
    assert!(response.json::<Value>().await.unwrap()["mfa_required"]
        .as_bool()
        .unwrap());
    assert_eq!(count(&db, id).await, initial);
    assert_eq!(
        post(
            &base,
            "/users/mfa/verify",
            json!({"username":"browser_mfa","code":"wrong"}),
            &cookies
        )
        .await
        .status(),
        401
    );
    assert_eq!(count(&db, id).await, initial);
    let good = post(
        &base,
        "/users/mfa/verify",
        json!({"username":"browser_mfa","code":codes[0],"backup":true}),
        &cookies,
    )
    .await;
    assert_eq!(good.status(), 200, "{}", good.text().await.unwrap());
    assert_eq!(count(&db, id).await, initial + 1);
    // A signed SSO reauthentication challenge is a step-up, not a login.
    let challenge = rg_core::auth::jwt::generate_mfa_reauthentication_challenge(
        id,
        "browser_mfa",
        "fixture-sso",
        "test-secret-key",
        None,
    )
    .unwrap();
    let response = post(
        &base,
        "/users/mfa/verify",
        json!({"username":"browser_mfa","code":codes[1],"backup":true}),
        &format!("plombir_git_mfa_challenge={challenge}"),
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(count(&db, id).await, initial + 1);
}

#[tokio::test]
async fn reset_and_initial_password_alert_only_when_their_login_is_complete() {
    use sha2::{Digest, Sha256};
    for mfa in [false, true] {
        for initial in [false, true] {
            let (base, db) = spawn_test_app_with_db().await;
            let (session, id) =
                register_full(&base, "browser_reset", "browser_reset@example.invalid").await;
            let codes = if mfa {
                enrol(&base, &session).await
            } else {
                Value::Null
            };
            let before = count(&db, id).await;
            let response = if initial {
                rg_db::ops::user_ops::set_password_by_administrator(
                    &db,
                    id,
                    &rg_core::auth::password::hash_password(PW).await.unwrap(),
                )
                .await
                .unwrap();
                post(
                    &base,
                    "/users/password/initial",
                    json!({"login":"browser_reset","password":PW,"new_password":"N3w$ecret!"}),
                    "",
                )
                .await
            } else {
                let raw = "fixture-reset-link";
                rg_db::ops::password_reset_token_ops::create(
                    &db,
                    id,
                    &hex::encode(Sha256::digest(raw.as_bytes())),
                    chrono::Utc::now() + chrono::Duration::minutes(15),
                )
                .await
                .unwrap();
                post(
                    &base,
                    "/users/reset-password",
                    json!({"token":raw,"new_password":"N3w$ecret!"}),
                    "",
                )
                .await
            };
            assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
            if mfa {
                assert_eq!(count(&db, id).await, before);
                let cookies = format!(
                    "{}; {}",
                    cookie(&response, "plombir_git_mfa_challenge"),
                    cookie(&response, COOKIE)
                );
                assert_eq!(
                    post(
                        &base,
                        "/users/mfa/verify",
                        json!({"username":"browser_reset","code":codes[0],"backup":true}),
                        &cookies
                    )
                    .await
                    .status(),
                    200
                );
            }
            assert_eq!(
                count(&db, id).await,
                before + 1,
                "mfa={mfa},initial={initial}"
            );
        }
    }
}
