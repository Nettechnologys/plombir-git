//! An instance that serves `[tls]` itself treats its requests as HTTPS
//! (card_c94e2be7c148).
//!
//! The `Secure` cookie flag and HSTS used to be decided by `X-Forwarded-Proto`
//! alone — a header only a proxy adds. With no proxy in front, a session cookie
//! issued over TLS went out without `Secure`, and HSTS was never sent: the
//! HTTP/1.1 request URI carries no scheme to read it from. The harness serves
//! plain HTTP either way; `tls_enabled` is the listener's flag the server reads.

use crate::common::{register_full, spawn_test_app_with_overrides, StateOverrides};

const USER: &str = "tls_listener_user";

async fn login(tls_enabled: bool) -> reqwest::Response {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides {
        tls_enabled,
        ..Default::default()
    })
    .await;
    register_full(&base, USER, "tls_listener_user@example.com").await;
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({ "login": USER, "password": "Qz7$wRtm" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response
}

fn auth_cookie(response: &reqwest::Response) -> String {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|cookie| !cookie.contains("Max-Age=0"))
        .unwrap_or_else(|| panic!("login set no session cookie: {:?}", response.headers()))
        .to_string()
}

#[tokio::test]
async fn a_tls_listener_issues_a_secure_session_and_hsts_without_a_proxy() {
    let response = login(true).await;
    let cookie = auth_cookie(&response);
    assert!(
        cookie.contains("; Secure"),
        "a session issued over TLS must not be sendable over plain HTTP: {cookie}"
    );
    assert!(
        response
            .headers()
            .contains_key(reqwest::header::STRICT_TRANSPORT_SECURITY),
        "a TLS listener must pin the browser to HTTPS"
    );
}

#[tokio::test]
async fn a_plain_http_listener_sets_neither_secure_nor_hsts() {
    let response = login(false).await;
    let cookie = auth_cookie(&response);
    assert!(
        !cookie.contains("Secure"),
        "a Secure cookie is never sent back over plain HTTP, so login would not stick: {cookie}"
    );
    assert!(!response
        .headers()
        .contains_key(reqwest::header::STRICT_TRANSPORT_SECURITY));
}
