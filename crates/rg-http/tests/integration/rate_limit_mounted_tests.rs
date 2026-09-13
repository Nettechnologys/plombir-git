//! Both rate limiters have to be on the router, not just on a router
//! (card_971ab86e0eaf).
//!
//! `rate_limit_middleware` is covered in `rate_limit.rs`, but on a
//! `Router::new()` the test builds its own subject: the behaviour of the layer
//! is proven, the fact that the server carries it is not. Deleting `auth_rl`
//! from `/users/login` — the layer standing between a password and a dictionary
//! — left every test in the suite green, and so did deleting the global limiter
//! from `apply_middleware`.
//!
//! The gap was structural. `apply_middleware` takes the limiter as an argument
//! precisely so the test router can pass `None`: the middleware extracts
//! `ConnectInfo`, and the ordinary harness serves with plain
//! `axum::serve(listener, app)`, which supplies none. So these tests build the
//! *production* router and serve it the way production does, with
//! `into_make_service_with_connect_info`. The budgets are tiny and local to each
//! test, so the shared harness stays limiter-free and no test elsewhere can go
//! flaky under load.

use std::net::SocketAddr;

use crate::common::{build_test_app_state, setup_test_db, wait_for_listener};
use crate::security_headers_tests::assert_security_headers;
use reqwest::header;

const PASSWORD: &str = "Qz7$wRtm";

/// Spawn the production router with the two limiters set to the given budgets.
///
/// `0` disables a limiter while leaving its layer mounted, which is how each
/// test isolates the half it is about: whatever answers 429 can only have come
/// from the other one.
async fn spawn_prod_app_with_rate_limits(global_max: u32, auth_max: u32) -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let state = build_test_app_state(db, repo_root);
    let app = rg_http::create_router_for_test_with_rate_limits(
        state,
        rg_http::rate_limit::RateLimiter::new(global_max, 60),
        rg_http::rate_limit::RateLimiter::new(auth_max, 60),
    );
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

/// One `POST /users/login` with credentials that cannot be right.
async fn failed_login(client: &reqwest::Client, base: &str) -> reqwest::StatusCode {
    client
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({
            "login": "rate_limit_nobody",
            "password": "definitely-not-the-password",
        }))
        .send()
        .await
        .unwrap()
        .status()
}

/// The global per-IP limiter is on the whole router, so a plain `GET /health`
/// is enough to spend its budget.
#[tokio::test]
async fn the_router_carries_the_global_rate_limiter() {
    let base = spawn_prod_app_with_rate_limits(2, 0).await;
    let client = reqwest::Client::new();

    for attempt in 1..=2 {
        let resp = client.get(format!("{base}/health")).send().await.unwrap();
        assert_eq!(
            resp.status(),
            200,
            "request {attempt} of 2 is inside the per-IP budget"
        );
    }

    let resp = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(
        resp.status(),
        429,
        "the third request is over budget — the global rate limiter is not mounted in the \
         production router"
    );
    assert_security_headers(resp.headers(), "global rate-limit 429");

    let api = client
        .post(format!("{base}/api/v1/there-is-no-such-route"))
        .send()
        .await
        .unwrap();
    assert_eq!(api.status(), 429);
    let body: serde_json::Value = api.json().await.expect("REST rate-limit JSON");
    assert_eq!(body["error"]["code"], "RATE_LIMITED");

    let oci = client
        .put(format!("{base}/v2/rate/limited/manifests/latest"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(oci.status(), 429);
    assert!(
        oci.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json")),
        "OCI rate-limit refusal must be JSON"
    );
    let body: serde_json::Value = oci.json().await.expect("OCI rate-limit JSON");
    assert_eq!(body["errors"][0]["code"], "TOOMANYREQUESTS");
    assert!(body["errors"][0]["message"]
        .as_str()
        .is_some_and(|message| !message.is_empty()));

    let git = client
        .post(format!("{base}/rate/limited/git-receive-pack"))
        .send()
        .await
        .unwrap();
    assert_eq!(git.status(), 429);
    assert!(
        git.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/plain")),
        "git rate-limit refusal must stay text/plain"
    );
    let body = git.text().await.expect("git rate-limit text");
    assert!(!body.trim_start().starts_with('{'));
}

/// The stricter limiter is layered onto `/users/register` and `/users/login`
/// only, and both routes draw on the one budget.
///
/// The order — login, register, login — is what makes this test notice the
/// layer going missing from *either* route: with a budget of 2, a route that
/// stops counting leaves the third request inside the budget.
#[tokio::test]
async fn the_credential_endpoints_carry_the_strict_auth_rate_limiter() {
    // Global limiter off: whatever answers 429 below came from the per-route one.
    let base = spawn_prod_app_with_rate_limits(0, 2).await;
    let client = reqwest::Client::new();

    let first = failed_login(&client, &base).await;
    assert_eq!(
        first, 401,
        "the first login is inside the budget and should be answered on its merits"
    );

    let second = client
        .post(format!("{base}/api/v1/users/register"))
        .json(&serde_json::json!({
            "username": "rate_limited",
            "email": "rate_limited@example.com",
            "password": PASSWORD,
        }))
        .send()
        .await
        .unwrap();
    assert!(
        second.status().is_success(),
        "the second credential request is still inside the budget: {}",
        second.status()
    );

    let third = failed_login(&client, &base).await;
    assert_eq!(
        third, 429,
        "the credential endpoints must share one strict budget — the auth rate limiter is \
         missing from /users/login or /users/register"
    );

    // It is a per-route limiter, not a second global one: everything else the
    // server serves is untouched by the spent credential budget.
    let health = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(
        health.status(),
        200,
        "the credential limiter must not gate the rest of the server"
    );
}
