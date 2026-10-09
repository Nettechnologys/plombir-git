//! A WebSocket handshake carrying its session only as `?token=<jwt>` is
//! anonymous — security audit finding #9.
//!
//! Both sockets used to fall back to the query parameter when neither the
//! cookie nor the `bearer.<jwt>` subprotocol was offered. A URL is the one
//! place a credential should never travel: it is copied into access logs,
//! proxy logs, browser history and `Referer` headers, and this server's own
//! request span carried it into every log line a request wrote. The frontend
//! stopped sending it long ago (`scripts/notification-websocket-contract-check.mjs`
//! forbids it), so the only clients the fallback still served were the ones
//! leaking their session.
//!
//! The refusal is asserted next to the handshake that still works, on the same
//! server with the same JWT, so a red test here is about the query shape and
//! not about a dead fixture.

use crate::common::{register_full, spawn_test_app};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Error};

/// A handshake request for `path` with the JWT in the query string and no
/// header-borne credential at all — the legacy shape.
fn query_token_request(
    base: &str,
    path: &str,
    jwt: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    format!("{}{path}?token={jwt}", base.replacen("http://", "ws://", 1))
        .into_client_request()
        .expect("a routable ws:// url")
}

#[tokio::test]
async fn a_query_token_alone_does_not_open_the_notification_socket() {
    let base = spawn_test_app().await;
    let (jwt, _) = register_full(&base, "wsquery", "wsquery@example.com").await;

    // Baseline: the very same JWT opens the socket as a subprotocol.
    let (mut socket, response) = tokio_tungstenite::connect_async(
        crate::common::ws::handshake_request(&base, "/api/v1/ws/notifications", Some(&jwt)),
    )
    .await
    .expect("the subprotocol handshake must still be accepted");
    assert_eq!(response.status(), 101);
    assert_eq!(
        response.headers()["sec-websocket-protocol"]
            .to_str()
            .expect("selected protocol is text"),
        format!("bearer.{jwt}")
    );
    socket.close(None).await.ok();

    let refusal = tokio_tungstenite::connect_async(query_token_request(
        &base,
        "/api/v1/ws/notifications",
        &jwt,
    ))
    .await;
    match refusal {
        Err(Error::Http(response)) => assert_eq!(
            response.status(),
            401,
            "a `?token=` handshake must be refused as anonymous"
        ),
        other => panic!("a `?token=` handshake was not refused: {other:?}"),
    }
}

#[tokio::test]
async fn a_query_token_alone_does_not_open_the_job_log_socket() {
    let base = spawn_test_app().await;
    let (jwt, _) = register_full(&base, "wsquery_job", "wsquery_job@example.com").await;

    // The job-log route resolves the session before it looks the job up, so no
    // job needs to exist for the refusal to be the one under test: an
    // anonymous caller is turned away with `401` before `job not found` could
    // be answered.
    let refusal =
        tokio_tungstenite::connect_async(query_token_request(&base, "/api/v1/ws/job/1", &jwt))
            .await;
    match refusal {
        Err(Error::Http(response)) => assert_eq!(
            response.status(),
            401,
            "a `?token=` handshake must be refused as anonymous"
        ),
        other => panic!("a `?token=` handshake was not refused: {other:?}"),
    }
}
