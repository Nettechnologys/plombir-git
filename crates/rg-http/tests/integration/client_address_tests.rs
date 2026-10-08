//! Which address a request is from — one answer, the server's.
//!
//! The audit log and the login log used to record whatever the client wrote
//! into `X-Forwarded-For` (card_5d48237b16b0); the limiter that did consult
//! `trusted_proxies` took the client-controlled left entry of the chain
//! (card_c2f0454ceb89); and the password limiter's per-source share had no
//! address it could trust on any HTTP door, so one client flooding
//! `/v2/auth/token` could take every place in it (card_a0f0cc7aed3a).
//!
//! These tests serve the router the production way — with `ConnectInfo` — so
//! the TCP peer exists, and steer the forwarding headers.

use std::net::SocketAddr;

use base64::Engine as _;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::common::{
    build_test_app_state_with, register_user, setup_test_db, wait_for_listener, StateOverrides,
};

/// Serve the test router with `ConnectInfo`, trusting `trusted` as proxies.
async fn spawn_with_peer(trusted: Vec<std::net::IpAddr>) -> (String, rg_db::DatabaseConnection) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let state = build_test_app_state_with(
        db.clone(),
        repo_root,
        StateOverrides {
            client_ip: Some(rg_http::client_ip::ClientIpResolver::new(trusted)),
            ..Default::default()
        },
    );
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    (format!("http://{addr}"), db)
}

async fn failed_login(base: &str, login: &str, forwarded_for: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .header("x-forwarded-for", forwarded_for)
        .header("x-real-ip", "198.51.100.200")
        .header(rg_http::client_ip::CLIENT_IP_HEADER, "198.51.100.201")
        .json(&serde_json::json!({"login": login, "password": "not-the-password"}))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
}

async fn last_login_ip(db: &rg_db::DatabaseConnection, username: &str) -> Option<String> {
    rg_db::ops::login_log_ops::Entity::find()
        .filter(rg_db::entities::login_log::Column::Username.eq(username))
        .order_by_desc(rg_db::entities::login_log::Column::Id)
        .one(db)
        .await
        .unwrap()
        .expect("a login_log row")
        .ip_address
}

/// card_5d48237b16b0: a client that is not a trusted proxy is recorded at its
/// socket address, whatever it writes into the forwarding headers.
#[tokio::test]
async fn the_login_log_records_the_socket_address_of_an_untrusted_client() {
    let (base, db) = spawn_with_peer(Vec::new()).await;
    register_user(
        &base,
        "addr_untrusted",
        "addr_untrusted@example.com",
        "Qz7$wRtm",
    )
    .await;

    failed_login(&base, "addr_untrusted", "1.2.3.4").await;

    assert_eq!(
        last_login_ip(&db, "addr_untrusted").await.as_deref(),
        Some("127.0.0.1")
    );
}

/// card_c2f0454ceb89 / card_5d48237b16b0: behind a trusted, appending proxy
/// the client is the right-most entry the proxy wrote, not the left-most one
/// the client did.
#[tokio::test]
async fn behind_a_trusted_proxy_the_rightmost_untrusted_hop_is_recorded() {
    let (base, db) = spawn_with_peer(vec!["127.0.0.1".parse().unwrap()]).await;
    register_user(
        &base,
        "addr_proxied",
        "addr_proxied@example.com",
        "Qz7$wRtm",
    )
    .await;

    failed_login(&base, "addr_proxied", "1.2.3.4, 203.0.113.7").await;

    assert_eq!(
        last_login_ip(&db, "addr_proxied").await.as_deref(),
        Some("203.0.113.7")
    );
}

fn basic(username: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
    )
}

async fn token_status(base: &str, from: &str, auth: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(format!("{base}/v2/auth/token"))
        .query(&[("service", "plombir-git-registry")])
        .header("x-forwarded-for", from)
        .header(reqwest::header::AUTHORIZATION, auth)
        .send()
        .await
        .unwrap()
        .status()
}

/// card_a0f0cc7aed3a: one source flooding `/v2/auth/token` with passwords is
/// held to its share of the password limiter, while another source asking at
/// the same moment is still checked.
///
/// The burst is smaller than the limiter's whole capacity on any host (at
/// least 34 places), so without a per-source share none of it would be shed.
#[tokio::test]
async fn one_source_cannot_take_the_password_limiter_from_another() {
    let (base, _db) = spawn_with_peer(vec!["127.0.0.1".parse().unwrap()]).await;
    register_user(&base, "addr_share", "addr_share@example.com", "Qz7$wRtm").await;
    let wrong = basic("addr_share", "not-the-password");

    let flood = (0..12).map(|_| token_status(&base, "203.0.113.50", &wrong));
    let (flooded, other) = tokio::join!(
        futures::future::join_all(flood),
        token_status(&base, "198.51.100.60", &wrong)
    );

    let shed = flooded
        .iter()
        .filter(|status| **status == reqwest::StatusCode::SERVICE_UNAVAILABLE)
        .count();
    assert!(
        shed > 0,
        "twelve checks in flight from one source must exceed its share: {flooded:?}"
    );
    assert!(
        flooded
            .iter()
            .all(|status| *status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                || *status == reqwest::StatusCode::UNAUTHORIZED),
        "{flooded:?}"
    );
    assert_eq!(
        other,
        reqwest::StatusCode::UNAUTHORIZED,
        "another source is still checked while the first is at its share"
    );
}
