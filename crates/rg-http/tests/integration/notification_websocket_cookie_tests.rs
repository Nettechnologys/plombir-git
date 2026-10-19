//! The notification socket authenticates by cookie and nothing else.
//!
//! `scripts/notification-websocket-contract-check.mjs` forbids the frontend from
//! carrying the token in the URL, in a `?token=` parameter, or in a
//! `Sec-WebSocket-Protocol` subprotocol — and a browser cannot put a header on a
//! WebSocket upgrade at all. So on `/ws/notifications` the HttpOnly session
//! cookie is not the *preferred* shape, it is the only one a browser has.
//!
//! Nothing in Rust covered that path before this file: `job_websocket_tests`
//! authenticates through the `bearer.<jwt>` subprotocol, and the contract check
//! only reads `web/src/lib/api/websockets.ts` — it never reaches the server. That
//! is what let `ws.rs` keep a second cookie reader with the name written out as a
//! literal, where renaming `AUTH_COOKIE_NAME` would have silently unauthenticated
//! every browser tab (card_24a8ef566056).

use crate::common::{register_full, spawn_test_app};
use futures::StreamExt;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Error, Message};

/// Open `/ws/notifications` the way a browser does — a `Cookie` header, no
/// `Authorization`, no subprotocol — and read the first frame back.
///
/// Returns the handshake status, whichever subprotocol the server selected, and
/// the first frame's text. A refused handshake stays an `Error::Http`, so callers
/// can prove that authentication failed before the protocol switched.
async fn open_notification_socket(
    base: &str,
    cookie: Option<&str>,
) -> Result<(u16, Option<String>, String), Error> {
    let url = format!(
        "{}/api/v1/ws/notifications",
        base.replacen("http://", "ws://", 1)
    );
    let mut request = url.into_client_request().expect("websocket request");
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert("cookie", cookie.parse().expect("cookie header value"));
    }

    let (mut socket, response) = tokio_tungstenite::connect_async(request).await?;
    let status = response.status().as_u16();
    let selected_protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .map(|value| {
            value
                .to_str()
                .expect("selected protocol is text")
                .to_string()
        });

    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("the socket must send a first frame")
        .expect("the socket closed before its first frame")
        .expect("the first frame must be readable");
    let Message::Text(frame) = frame else {
        panic!("expected a text frame, got {frame:?}");
    };
    let frame = frame.to_string();

    if socket.close(None).await.is_err() {
        // The server may have closed first — the frame above is the assertion.
    }
    Ok((status, selected_protocol, frame))
}

/// A browser holding only the session cookie must be greeted as the account that
/// owns it; an anonymous handshake must be refused.
///
/// The refusal is the baseline that makes the greeting mean something: without
/// it, a socket that greeted everybody would read as a passing test. Both halves
/// go through the same handler on the same server, so the only difference
/// between them is the cookie.
#[tokio::test]
async fn a_cookie_session_may_open_the_notification_socket_and_an_anonymous_caller_may_not() {
    let base = spawn_test_app().await;
    let (jwt, user_id) = register_full(&base, "wscookie", "wscookie@example.com").await;

    let (status, selected_protocol, welcome) =
        open_notification_socket(&base, Some(&format!("plombir_git_token={jwt}")))
            .await
            .expect("a valid cookie must open the notification socket");
    assert_eq!(status, 101, "the cookie handshake was not upgraded");
    assert_eq!(
        selected_protocol, None,
        "the client offered no subprotocol, so the server must select none"
    );
    assert!(
        welcome.contains("\"type\":\"connected\""),
        "a cookie handshake must be greeted, got {welcome}"
    );
    assert!(
        welcome.contains(&format!("\"user_id\":{user_id}")),
        "the greeting must name the account that owns the cookie, got {welcome}"
    );

    let refusal = open_notification_socket(&base, None).await;
    match refusal {
        Err(Error::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected an unauthorized WebSocket handshake, got {other:?}"),
    }
}

/// A cookie under any other name is not a session.
///
/// This is the shape the bug would have taken: after a rename, the browser keeps
/// sending a cookie and the server keeps reading one, but they no longer agree on
/// the name — so the tab authenticates as nobody while everything still looks
/// connected.
#[tokio::test]
async fn a_cookie_under_another_name_does_not_authenticate_the_notification_socket() {
    let base = spawn_test_app().await;
    let (jwt, _) = register_full(&base, "wsothername", "wsothername@example.com").await;

    let refusal =
        open_notification_socket(&base, Some(&format!("plombir_git_session={jwt}"))).await;
    match refusal {
        Err(Error::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("a valid JWT under the wrong cookie name was accepted: {other:?}"),
    }
}

/// card_6abf2e3c4287, the watch half: a new issue wrote an inbox row for every
/// watcher of the repository and pushed nothing, so a watcher's open tab heard
/// of it only on the next periodic re-read. The socket is the cookie one, the
/// way the navigation bar opens it, and it is open before the issue exists.
#[tokio::test]
async fn a_watcher_hears_about_a_new_issue_on_their_open_notification_socket() {
    let base = spawn_test_app().await;
    let (owner, _) = register_full(&base, "wswatch-owner", "wswatch-owner@example.com").await;
    let (watcher, watcher_id) =
        register_full(&base, "wswatch-reader", "wswatch-reader@example.com").await;
    crate::common::create_repo(&base, &owner, "watched").await;
    let client = reqwest::Client::new();
    let watched = client
        .put(format!("{base}/api/v1/repos/wswatch-owner/watched/watch"))
        .bearer_auth(&watcher)
        .json(&serde_json::json!({ "state": "watching" }))
        .send()
        .await
        .unwrap();
    assert_eq!(watched.status(), 200);

    let mut request = format!(
        "{}/api/v1/ws/notifications",
        base.replacen("http://", "ws://", 1)
    )
    .into_client_request()
    .unwrap();
    request.headers_mut().insert(
        "cookie",
        format!("plombir_git_token={watcher}").parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("the watcher opens their notification socket");
    async fn next_frame<S>(socket: &mut S) -> serde_json::Value
    where
        S: futures::Stream<Item = Result<Message, Error>> + Unpin,
    {
        match tokio::time::timeout(std::time::Duration::from_secs(10), socket.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str(&text).unwrap(),
            other => panic!("no notification frame on the watcher's socket: {other:?}"),
        }
    }
    assert_eq!(next_frame(&mut socket).await["type"], "connected");

    let opened = client
        .post(format!("{base}/api/v1/repos/wswatch-owner/watched/issues"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({ "title": "Watched thread" }))
        .send()
        .await
        .unwrap();
    assert_eq!(opened.status(), 201);

    let event = next_frame(&mut socket).await;
    assert_eq!(event["event_type"], "issue", "{event}");
    assert_eq!(event["data"]["user_id"], watcher_id, "{event}");
    assert_eq!(
        event["data"]["payload"]["title"], "Issue #1 opened",
        "{event}"
    );
}
