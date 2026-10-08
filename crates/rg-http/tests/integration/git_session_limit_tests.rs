//! card_444288887c81: anonymous Smart-HTTP fetches draw on the same bounded
//! pool of git sessions as everything else.
//!
//! The pool is process-wide (`rg_core::git_sessions::global`), so these tests
//! fill it — or one source's share of it — by hand and then ask over HTTP.
//! nextest runs every test in a process of its own, which is what keeps the
//! held places from leaking into a neighbour.

use std::net::SocketAddr;

use rg_core::git_sessions::{global, GitSessionPermit, SessionSource};

use crate::common::{
    build_test_app_state, create_repo, register_user, setup_test_db, wait_for_listener,
};

async fn spawn_with_connect_info() -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root));
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
    format!("http://{addr}")
}

/// An anonymous v2 fetch of a public repository: a flush, nothing wanted.
async fn anonymous_fetch(base: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/{path}/git-upload-pack"))
        .header("Git-Protocol", "version=2")
        .header("Content-Type", "application/x-git-upload-pack-request")
        .body("0000")
        .send()
        .await
        .unwrap()
}

/// Take every place `source` may have, and hand them back on drop.
fn hold_all(source: Option<SessionSource>) -> Vec<GitSessionPermit> {
    let mut held = Vec::new();
    while let Ok(permit) = global().try_acquire(source) {
        held.push(permit);
    }
    assert!(!held.is_empty());
    held
}

async fn assert_busy(response: reqwest::Response) {
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("5"),
        "a busy server says when to come back"
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("try again"), "{body}");
}

#[tokio::test]
async fn a_fetch_beyond_the_server_ceiling_is_refused_before_git_starts() {
    let base = spawn_with_connect_info().await;
    let owner = "sessions_full";
    let token = register_user(&base, owner, "sessions_full@example.com", "Qz7$wRtm").await;
    create_repo(&base, &token, "public").await;
    let path = format!("{owner}/public");

    assert!(anonymous_fetch(&base, &path).await.status().is_success());

    let held = hold_all(None);
    assert_busy(anonymous_fetch(&base, &path).await).await;

    drop(held);
    assert!(
        anonymous_fetch(&base, &path).await.status().is_success(),
        "the places come back once the sessions holding them end"
    );
}

/// One anonymous address cannot take the whole pool: once it holds its share,
/// it is refused while an authenticated client is still served.
#[tokio::test]
async fn one_anonymous_address_is_held_to_its_share() {
    let base = spawn_with_connect_info().await;
    let owner = "sessions_share";
    let token = register_user(&base, owner, "sessions_share@example.com", "Qz7$wRtm").await;
    create_repo(&base, &token, "public").await;
    let path = format!("{owner}/public");

    let loopback = SessionSource::of(None, Some("127.0.0.1".parse().unwrap()));
    let _held = hold_all(loopback);
    assert_busy(anonymous_fetch(&base, &path).await).await;

    let authenticated = reqwest::Client::new()
        .post(format!("{base}/{path}/git-upload-pack"))
        .bearer_auth(&token)
        .header("Git-Protocol", "version=2")
        .header("Content-Type", "application/x-git-upload-pack-request")
        .body("0000")
        .send()
        .await
        .unwrap();
    assert!(
        authenticated.status().is_success(),
        "another source is still served: {}",
        authenticated.status()
    );
}
