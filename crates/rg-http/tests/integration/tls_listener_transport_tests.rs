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
    login_with(
        StateOverrides {
            tls_enabled,
            ..Default::default()
        },
        false,
    )
    .await
}

/// A login against `overrides`, optionally claiming `X-Forwarded-Proto: https`
/// the way a client (or a proxy) can.
async fn login_with(overrides: StateOverrides, forwarded_https: bool) -> reqwest::Response {
    let (base, _db) = spawn_test_app_with_overrides(overrides).await;
    register_full(&base, USER, "tls_listener_user@example.com").await;
    let mut request = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({ "login": USER, "password": "Qz7$wRtm" }));
    if forwarded_https {
        request = request.header("x-forwarded-proto", "https");
    }
    let response = request.send().await.unwrap();
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

/// The published address decides: an instance whose `external_url` is https
/// sits behind a proxy that terminates TLS, and that proxy may forward no
/// `X-Forwarded-Proto` — the session cookie must still be `Secure` and HSTS
/// still sent. The plain listener here is exactly that proxy's backend side.
#[tokio::test]
async fn an_https_external_url_issues_a_secure_session_and_hsts_behind_a_silent_proxy() {
    let response = login_with(
        StateOverrides {
            external_url: Some("https://git.example.com".to_string()),
            ..Default::default()
        },
        false,
    )
    .await;
    let cookie = auth_cookie(&response);
    assert!(
        cookie.contains("; Secure"),
        "an https instance sent its session cookie without Secure: {cookie}"
    );
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::STRICT_TRANSPORT_SECURITY)
            .and_then(|value| value.to_str().ok()),
        Some("max-age=31536000; includeSubDomains"),
        "HSTS follows the published address, and preload stays off by default"
    );
}

/// And the other way round: an instance published over plain http is not
/// talked into `Secure` or HSTS by a forwarded header any client can send.
#[tokio::test]
async fn an_http_external_url_is_not_talked_into_https_by_a_forwarded_header() {
    let response = login_with(
        StateOverrides {
            external_url: Some("http://git.example.com".to_string()),
            ..Default::default()
        },
        true,
    )
    .await;
    let cookie = auth_cookie(&response);
    assert!(
        !cookie.contains("Secure"),
        "an http instance would lose its session to the Secure flag: {cookie}"
    );
    assert!(!response
        .headers()
        .contains_key(reqwest::header::STRICT_TRANSPORT_SECURITY));
}
