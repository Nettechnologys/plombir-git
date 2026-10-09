//! The same-origin guard: a browser attaches the session cookie on its own, so
//! a mutation (or a WebSocket upgrade) that carries it must come from the
//! address this instance publishes.
//!
//! `SameSite=Strict` is a browser behaviour, not a server check: it says
//! nothing about a page that is same-site but not same-origin, and nothing in
//! the server proved it ever looked. These tests drive the server — a foreign
//! `Origin` with the cookie is refused, the published origin passes, and a
//! Bearer request (no cookie) is untouched.

use crate::common::{register_full, spawn_test_app};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// The origin the test harness is reached at, spelled the way a browser would
/// put it in `Origin` (no trailing slash).
fn published_origin(base: &str) -> String {
    base.trim_end_matches('/').to_string()
}

#[tokio::test]
async fn a_cross_origin_mutation_with_the_session_cookie_is_refused() {
    let base = spawn_test_app().await;
    let (jwt, _) = register_full(&base, "csrf", "csrf@example.com").await;
    let client = reqwest::Client::new();

    let refused = client
        .post(format!("{base}/api/v1/users/logout"))
        .header("cookie", format!("plombir_git_token={jwt}"))
        .header("origin", "https://evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        403,
        "a foreign Origin must not be allowed to spend the session cookie"
    );
    assert!(
        refused.headers().contains_key("content-security-policy"),
        "the refusal still carries the headers every response carries"
    );

    // The same request from the address the instance is reached at passes.
    let accepted = client
        .post(format!("{base}/api/v1/users/logout"))
        .header("cookie", format!("plombir_git_token={jwt}"))
        .header("origin", published_origin(&base))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 200, "the published origin must pass");
}

#[tokio::test]
async fn a_bearer_request_without_origin_is_untouched() {
    let base = spawn_test_app().await;
    let (jwt, _) = register_full(&base, "csrfbearer", "csrfbearer@example.com").await;
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/logout"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "a browser does not attach tokens on its own, so they are not this guard's business"
    );
}

#[tokio::test]
async fn sec_fetch_site_decides_when_origin_is_absent() {
    let base = spawn_test_app().await;
    let (jwt, _) = register_full(&base, "csrfsecfetch", "csrfsecfetch@example.com").await;
    let client = reqwest::Client::new();

    let cross = client
        .post(format!("{base}/api/v1/users/logout"))
        .header("cookie", format!("plombir_git_token={jwt}"))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), 403, "a cross-site fetch is refused");

    let same = client
        .post(format!("{base}/api/v1/users/logout"))
        .header("cookie", format!("plombir_git_token={jwt}"))
        .header("sec-fetch-site", "same-origin")
        .send()
        .await
        .unwrap();
    assert_eq!(same.status(), 200, "a same-origin fetch passes");
}

#[tokio::test]
async fn a_foreign_origin_cannot_open_the_notification_socket_with_a_cookie() {
    let base = spawn_test_app().await;
    let (jwt, _) = register_full(&base, "csrfws", "csrfws@example.com").await;
    let url = format!(
        "{}/api/v1/ws/notifications",
        base.replacen("http://", "ws://", 1)
    );

    for origin in ["https://evil.example.com", "http://127.0.0.1:1"] {
        let mut request = url
            .clone()
            .into_client_request()
            .expect("websocket request");
        request.headers_mut().insert(
            "cookie",
            format!("plombir_git_token={jwt}")
                .parse()
                .expect("cookie header"),
        );
        request
            .headers_mut()
            .insert("origin", origin.parse().expect("origin header"));

        let error = tokio_tungstenite::connect_async(request)
            .await
            .expect_err("a foreign Origin must not upgrade the socket");
        let status = match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
            other => panic!("expected an HTTP refusal, got {other:?}"),
        };
        assert_eq!(status, 403, "origin {origin} was not refused");
    }
}
