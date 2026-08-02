//! Ending a session has to reach the sockets that session opened.
//!
//! `card_fcab45f42a02` gave the stateless JWT a revocation channel: a password
//! reset and a `POST /users/logout` bump `users.session_version`, and the shared
//! standing middleware turns away any request carrying an older generation. A
//! WebSocket makes exactly one request, though — the handshake — so a socket
//! already open is past the middleware for good, and the sampled re-check that
//! covers it asked only whether the *account* still stood (card_7898025803a6).
//!
//! Both revocations leave the account standing, on purpose: they are what a user
//! does to end a session, not to end an account. So the one window not bounded
//! by any interval was the one deliberately opened by the user who pressed
//! "log out" on a machine that is not theirs — the stream kept running for as
//! long as the tab stayed open.
//!
//! The job-log half of the same socket pair is asserted in
//! `job_websocket_tests`, where the pipeline fixture lives; the SSH half of the
//! same class is in `rg-ssh/tests/deactivated_ssh_tests.rs`.

use futures::StreamExt;
use tokio_tungstenite::tungstenite::Message;

use crate::common::ws::handshake_request;
use crate::common::{register_full, spawn_test_app_with_overrides, StateOverrides};

const PASSWORD: &str = "Qz7$wRtm";
const NEW_PASSWORD: &str = "Nw9#pLqz";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A notification socket that has greeted its client — so a close afterwards is
/// the re-check and not a handshake that never worked.
async fn open_greeted_socket(base: &str, token: &str) -> Socket {
    let (mut socket, response) = tokio_tungstenite::connect_async(handshake_request(
        base,
        "/api/v1/ws/notifications",
        Some(token),
    ))
    .await
    .expect("a live session must reach the notification socket");
    assert_eq!(response.status(), 101);

    let welcome = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("the socket must greet a live session")
        .expect("the socket closed before the welcome frame")
        .expect("the welcome frame must be readable");
    assert!(
        matches!(&welcome, Message::Text(text) if text.contains("\"connected\"")),
        "unexpected welcome frame: {welcome:?}"
    );
    socket
}

/// Sit through several re-check intervals and require silence. This is the
/// baseline every assertion below leans on: without it, a socket that closes
/// after the revocation is equally good evidence that the re-check hangs up on
/// everyone.
async fn stays_open(socket: &mut Socket) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(4), socket.next())
        .await
        .is_err()
}

/// Wait for the server to end the socket.
async fn closed_by_server(socket: &mut Socket) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while let Some(frame) = socket.next().await {
            match frame {
                Ok(Message::Close(_)) | Err(_) => return true,
                Ok(_) => continue,
            }
        }
        true
    })
    .await
    .unwrap_or(false)
}

async fn logout(base: &str, token: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/logout"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "logout failed");
}

async fn login(base: &str, username: &str, password: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({ "login": username, "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "login failed");
    response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .expect("a login without MFA returns its session token")
        .to_string()
}

/// Plant a reset token straight into the database — the raw value only ever
/// leaves the server by email, which the test harness cannot read.
async fn issue_reset_token(db: &rg_db::DatabaseConnection, user_id: i64, raw: &str) -> String {
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(raw.as_bytes()));
    rg_db::ops::password_reset_token_ops::create(
        db,
        user_id,
        &hash,
        chrono::Utc::now() + chrono::Duration::minutes(15),
    )
    .await
    .expect("create reset token");
    raw.to_string()
}

/// Logging out is the act of a user on a machine they are leaving, and the tab
/// they are leaving behind is exactly the thing it has to reach. The socket
/// opened *after* the logout is asserted in the same run: a re-check that closed
/// every socket would satisfy the first half and make the feature useless.
#[tokio::test]
async fn a_logout_closes_the_notification_socket_it_ended_and_spares_the_next_one() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides {
        ws_session_recheck_secs: Some(1),
        ..Default::default()
    })
    .await;
    let (jwt, _user_id) = register_full(&base, "ws_logout", "ws_logout@example.com").await;

    let mut socket = open_greeted_socket(&base, &jwt).await;
    assert!(
        stays_open(&mut socket).await,
        "the re-check closed a socket whose session is in good standing"
    );

    logout(&base, &jwt).await;

    assert!(
        closed_by_server(&mut socket).await,
        "the notification socket outlived the logout that revoked its session"
    );

    // The generation is a comparison, not a kill switch: a session minted after
    // the bump has to survive the very same re-check.
    let fresh = login(&base, "ws_logout", PASSWORD).await;
    let mut next = open_greeted_socket(&base, &fresh).await;
    assert!(
        stays_open(&mut next).await,
        "the re-check closed a socket opened after the logout, on a session newer than the bump"
    );
}

/// A password reset is the recovery path for a compromised session, so the
/// stream the compromised session is holding open is the point of it.
#[tokio::test]
async fn a_password_reset_closes_the_notification_socket_of_the_session_it_replaced() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        ws_session_recheck_secs: Some(1),
        ..Default::default()
    })
    .await;
    let (jwt, user_id) = register_full(&base, "ws_reset", "ws_reset@example.com").await;

    let mut socket = open_greeted_socket(&base, &jwt).await;
    assert!(
        stays_open(&mut socket).await,
        "the re-check closed a socket whose session is in good standing"
    );

    let raw = issue_reset_token(&db, user_id, "raw-token-ws-reset").await;
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/reset-password"))
        .json(&serde_json::json!({ "token": raw, "new_password": NEW_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "password reset failed");

    assert!(
        closed_by_server(&mut socket).await,
        "the notification socket outlived the password reset that revoked its session"
    );
}
