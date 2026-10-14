//! A request that rides on the session cookie must come from a page this
//! instance publishes (card_71613e98a0c5, PR #18 item 5).
//!
//! `SameSite=Strict` is the browser's promise, not the server's check, and a
//! sibling subdomain is the same site. The guard refuses a cookie-carrying
//! mutation or WebSocket handshake whose `Origin` is not this instance's and
//! whose `Sec-Fetch-Site` does not say `same-origin` — and lets through every
//! request a browser did not start on someone's behalf.

use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::common::{register_full, spawn_test_app_with_db, spawn_test_app_with_overrides};

const EVIL: &str = "https://evil.example";

/// `POST /api/v1/repos` as the session cookie, with `headers` on top.
async fn create_with_cookie(
    base: &str,
    token: &str,
    name: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let mut request = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .header("cookie", format!("plombir_git_token={token}"))
        .json(&serde_json::json!({ "name": name }));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

fn refused_by_the_guard(answer: &(u16, String)) -> bool {
    answer.0 == 403
        && answer
            .1
            .contains("comes from a page this instance does not publish")
}

#[tokio::test]
async fn a_cookie_mutation_from_another_origin_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "origin-owner", "origin-owner@example.com").await;

    for headers in [
        vec![("origin", EVIL)],
        vec![("origin", EVIL), ("sec-fetch-site", "cross-site")],
        // A sibling subdomain is the same *site*: SameSite lets the cookie go.
        vec![("origin", EVIL), ("sec-fetch-site", "same-site")],
        vec![("sec-fetch-site", "cross-site")],
        vec![("origin", "null")],
        // Same port, another host: not this instance.
        vec![("origin", "http://localhost:1")],
    ] {
        let answer = create_with_cookie(&base, &token, "forged", &headers).await;
        assert!(
            refused_by_the_guard(&answer),
            "{headers:?} was let through: {answer:?}"
        );
    }

    // Each of these is let through — and makes the repository, which is what
    // proves it was let through rather than refused for another reason.
    let own = base.clone();
    let https_own = base.replacen("http://", "https://", 1);
    for (name, headers) in [
        ("from-own-origin", vec![("origin", own.as_str())]),
        (
            "from-own-origin-same-origin",
            vec![("origin", own.as_str()), ("sec-fetch-site", "same-origin")],
        ),
        // A development proxy that rewrites `Host` (vite `changeOrigin`): the
        // backend sees another host, the browser says same-origin.
        (
            "through-a-proxy",
            vec![
                ("origin", "http://localhost:5173"),
                ("sec-fetch-site", "same-origin"),
            ],
        ),
        ("typed-address", vec![("sec-fetch-site", "none")]),
        // The scheme behind a TLS proxy that forwards no `X-Forwarded-Proto`
        // is a guess; the host and port are not.
        ("tls-proxy-guess", vec![("origin", https_own.as_str())]),
        // git, docker, curl: neither header.
        ("no-browser", vec![]),
    ] {
        let (status, body) = create_with_cookie(&base, &token, name, &headers).await;
        assert_eq!(status, 201, "{name}: {body}");
    }

    // No cookie, no forgery: a Bearer client is not the guard's business.
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .header("origin", EVIL)
        .header("sec-fetch-site", "cross-site")
        .json(&serde_json::json!({ "name": "bearer-from-anywhere" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());

    // A read changes nothing and is not refused.
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/user"))
        .header("cookie", format!("plombir_git_token={token}"))
        .header("origin", EVIL)
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_ne!(response.status(), 403);
}

/// The WebSocket handshake hands the socket the cookie's authority for as long
/// as it stays open, so a cross-origin page may not open it.
#[tokio::test]
async fn a_cookie_websocket_from_another_origin_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "origin-ws", "origin-ws@example.com").await;
    let handshake = |origin: &str| {
        let url = format!(
            "{}/api/v1/ws/notifications",
            base.replacen("http://", "ws://", 1)
        );
        let mut request = url.into_client_request().unwrap();
        let headers = request.headers_mut();
        headers.insert(
            "cookie",
            format!("plombir_git_token={token}").parse().unwrap(),
        );
        headers.insert("origin", origin.parse().unwrap());
        request
    };

    match tokio_tungstenite::connect_async(handshake(EVIL)).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 403);
            let body =
                String::from_utf8_lossy(response.body().as_deref().unwrap_or(&[])).into_owned();
            assert!(
                body.contains("comes from a page this instance does not publish"),
                "{body}"
            );
        }
        other => panic!("a cross-origin handshake was not refused: {other:?}"),
    }
    let (_socket, response) = tokio_tungstenite::connect_async(handshake(&base))
        .await
        .expect("the instance's own page opens the socket");
    assert_eq!(response.status().as_u16(), 101);
}

/// With `external_url` published, that is the instance's origin — not
/// whatever `Host` a request happens to arrive with.
#[tokio::test]
async fn with_a_published_address_the_origin_is_that_address() {
    let (base, _db) = spawn_test_app_with_overrides(crate::common::StateOverrides {
        external_url: Some("https://git.example.test".to_string()),
        ..Default::default()
    })
    .await;
    let (token, _) = register_full(&base, "origin-pub", "origin-pub@example.com").await;

    let (status, body) = create_with_cookie(
        &base,
        &token,
        "published",
        &[("origin", "https://git.example.test:443")],
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let answer = create_with_cookie(&base, &token, "by-host", &[("origin", base.as_str())]).await;
    assert!(
        refused_by_the_guard(&answer),
        "an Origin that only matches the Host was let through: {answer:?}"
    );
}
