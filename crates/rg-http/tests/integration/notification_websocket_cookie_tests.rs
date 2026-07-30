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
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

/// Open `/ws/notifications` the way a browser does — a `Cookie` header, no
/// `Authorization`, no subprotocol — and read the first frame back.
///
/// Returns the handshake status, whichever subprotocol the server selected, and
/// the first frame's text. The socket is opened even for a caller with no
/// session: this endpoint reports the refusal in a frame rather than failing the
/// upgrade, so the frame is where both answers live.
async fn open_notification_socket(
    base: &str,
    cookie: Option<&str>,
) -> (u16, Option<String>, String) {
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

    let (mut socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("the notification socket accepts the upgrade");
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
    (status, selected_protocol, frame)
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
        open_notification_socket(&base, Some(&format!("forgekeep_token={jwt}"))).await;
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

    let (status, _, refusal) = open_notification_socket(&base, None).await;
    assert_eq!(status, 101);
    assert!(
        refusal.contains("authentication required"),
        "a handshake with no session must be refused — the greeting above proves \
         the fixture works, got {refusal}"
    );
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

    let (_, _, refusal) =
        open_notification_socket(&base, Some(&format!("forgekeep_session={jwt}"))).await;
    assert!(
        refusal.contains("authentication required"),
        "a valid JWT under the wrong cookie name was accepted as a session, got {refusal}"
    );
}
