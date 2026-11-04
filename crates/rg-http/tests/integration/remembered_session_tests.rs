//! Browser persistence is chosen at login and renewed only with a current cookie.
use crate::common::{register_full, spawn_test_app_with_db};
use chrono::{Duration, Utc};
use reqwest::header;
use rg_core::auth::jwt::{self, Claims};

const SECRET: &str = "test-secret-key";
fn signed(claims: &Claims) -> String {
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        claims,
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap()
}
fn cookie(token: &str) -> String {
    format!("plombir_git_token={token}")
}
async fn refresh(base: &str, token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/me/session/refresh"))
        .header(header::COOKIE, cookie(token))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn remembered_session_login_cookie_and_logout_policies() {
    let (base, _) = spawn_test_app_with_db().await;
    register_full(&base, "remember", "remember@example.test").await;
    let client = reqwest::Client::new();
    for choice in [None, Some(false), Some(true)] {
        let mut body = serde_json::json!({"username":"remember", "password":"Qz7$wRtm"});
        if let Some(value) = choice {
            body["remember_me"] = value.into();
        }
        let response = client
            .post(format!("{base}/api/v1/users/login"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let set = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        let body: serde_json::Value = response.json().await.unwrap();
        let token = body["token"].as_str().unwrap();
        let claims = jwt::validate_token(token, SECRET).unwrap();
        assert_eq!(claims.remember_me, choice);
        assert!(
            (claims.exp
                - claims.iat
                - if choice == Some(true) {
                    2592000
                } else {
                    604800
                })
            .abs()
                < 2
        );
        assert_eq!(set.contains("Max-Age="), choice != Some(false));
        for attr in ["HttpOnly", "Path=/", "SameSite=Strict"] {
            assert!(set.contains(attr));
        }
        let me: serde_json::Value = client
            .get(format!("{base}/api/v1/users/me"))
            .header(header::COOKIE, cookie(token))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(me["remember_me"], choice == Some(true));
        let out = client
            .post(format!("{base}/api/v1/users/logout"))
            .header(header::COOKIE, cookie(token))
            .send()
            .await
            .unwrap();
        assert_eq!(out.status(), 200);
        let clear = out.headers()[header::SET_COOKIE].to_str().unwrap();
        for attr in ["Max-Age=0", "HttpOnly", "Path=/", "SameSite=Strict"] {
            assert!(clear.contains(attr));
        }
        assert_eq!(refresh(&base, token).await.status(), 401);
    }
}

#[tokio::test]
async fn remembered_session_refresh_preserves_proof_and_has_an_absolute_deadline() {
    let (base, _) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "renew", "renew@example.test").await;
    let mut before = jwt::validate_token(&token, SECRET).unwrap();
    before.remember_me = Some(true);
    before.iat -= Duration::days(2).num_seconds();
    before.exp = Utc::now().timestamp() + Duration::days(28).num_seconds();
    before.sudo_exp = Some(Utc::now().timestamp() - 3600);
    let response = refresh(&base, &signed(&before)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let set = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set.contains("Max-Age=2592000") || set.contains("Max-Age=2591999"));
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body.get("token").is_none(),
        "refresh must not expose the HttpOnly token to scripts"
    );
    let renewed = set.split(';').next().unwrap().split_once('=').unwrap().1;
    let after = jwt::validate_token(renewed, SECRET).unwrap();
    assert_eq!(
        (
            after.sub.clone(),
            after.session_version,
            after.iat,
            after.sudo_exp
        ),
        (
            before.sub.clone(),
            before.session_version,
            before.iat,
            before.sudo_exp
        )
    );
    assert!((after.exp - Utc::now().timestamp() - 2592000).abs() <= 2);
    let noop = refresh(&base, renewed).await;
    assert_eq!(noop.status(), 200);
    assert!(!noop.headers().contains_key(header::SET_COOKIE));
    let body: serde_json::Value = noop.json().await.unwrap();
    assert!(body.get("token").is_none());
    assert_eq!(body["refreshed"], false);
    before.iat = Utc::now().timestamp() - Duration::days(89).num_seconds();
    before.exp = Utc::now().timestamp() + 600;
    let capped = refresh(&base, &signed(&before)).await;
    assert_eq!(capped.status(), 200);
    let capped_cookie = capped.headers()[header::SET_COOKIE].to_str().unwrap();
    let capped_token = capped_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    assert_eq!(
        jwt::validate_token(capped_token, SECRET).unwrap().exp,
        before.iat + Duration::days(90).num_seconds()
    );
    before.iat -= Duration::days(2).num_seconds();
    let refused = refresh(&base, &signed(&before)).await;
    assert_eq!(refused.status(), 401);
    assert!(!refused.headers().contains_key(header::SET_COOKIE));
}

#[tokio::test]
async fn remembered_session_refresh_refuses_other_credentials_and_cross_site_requests() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "renew-refuse", "renew-refuse@example.test").await;
    let mut claims = jwt::validate_token(&token, SECRET).unwrap();
    for choice in [None, Some(false)] {
        claims.remember_me = choice;
        let response = refresh(&base, &signed(&claims)).await;
        assert_eq!(response.status(), 403);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
    }
    claims.remember_me = Some(true);
    let token = signed(&claims);
    let url = format!("{base}/api/v1/users/me/session/refresh");
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .header(header::COOKIE, cookie(&token))
            .header(header::ORIGIN, "https://attacker.test")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let pat = jwt::generate_token_for_pat(
        user_id,
        &claims.username,
        claims.session_version,
        123,
        SECRET,
        7,
    )
    .unwrap();
    assert_eq!(refresh(&base, &pat).await.status(), 403);
    claims.exp = Utc::now().timestamp() - 1; // Within decoder leeway, still cannot renew.
    assert_eq!(refresh(&base, &signed(&claims)).await.status(), 401);
    rg_db::ops::user_ops::invalidate_sessions(&db, user_id)
        .await
        .unwrap();
    let cookie_only = refresh(&base, &token).await;
    assert_eq!(cookie_only.status(), 401);
    assert!(cookie_only.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    let body: serde_json::Value = cookie_only.json().await.unwrap();
    assert!(body["error"].get("reason").is_none());
    assert!(body.get("token").is_none());
    let response = client
        .post(&url)
        .header(header::COOKIE, cookie(&token))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["error"]["reason"], "session_revoked");
}

#[tokio::test]
async fn remembered_mfa_choice_cannot_be_changed_in_the_second_factor_body() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, user_id) = register_full(&base, "remember-mfa", "remember-mfa@example.test").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .unwrap();
    let client = reqwest::Client::new();
    for choice in [false, true] {
        let code = if choice { "123456" } else { "654321" };
        rg_db::ops::mfa_backup_code_ops::set_codes(&db, user_id, &[code.to_owned()])
            .await
            .unwrap();
        let response=client.post(format!("{base}/api/v1/users/login")).json(&serde_json::json!({"username":"remember-mfa","password":"Qz7$wRtm","remember_me":choice})).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let challenge = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .find(|v| v.starts_with("plombir_git_mfa_challenge="))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let response=client.post(format!("{base}/api/v1/users/mfa/verify")).header(header::COOKIE,challenge)
            .json(&serde_json::json!({"username":"remember-mfa","code":code,"backup":true,"remember_me":!choice})).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            jwt::validate_token(body["token"].as_str().unwrap(), SECRET)
                .unwrap()
                .remember_me,
            Some(choice)
        );
    }
}
