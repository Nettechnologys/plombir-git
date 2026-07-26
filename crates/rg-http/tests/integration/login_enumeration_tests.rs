//! `POST /api/v1/users/login` must not answer "does this account exist?".
//!
//! The response *body* has always been unified to `invalid credentials`, but a
//! login for a real account paid for an Argon2 verification (tens of
//! milliseconds, on purpose) while an unknown username was rejected the moment
//! the database lookup came back empty. That difference is an order of
//! magnitude above network noise, which made a dictionary sweep over usernames
//! a perfectly good enumeration oracle.

use super::common::{register_user, spawn_test_app};
use std::time::{Duration, Instant};

const PASSWORD: &str = "Qz7$wRtm";

/// One failed login, returning the status, the body, and how long it took.
async fn failed_login(
    client: &reqwest::Client,
    base: &str,
    login: &str,
) -> (reqwest::StatusCode, serde_json::Value, Duration) {
    let started = Instant::now();
    let resp = client
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({"login": login, "password": "definitely-not-the-password"}))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let elapsed = started.elapsed();
    let mut body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 401, "expected a rejection for '{login}': {body}");
    // Per-request correlation id — noise for a body comparison.
    if let Some(error) = body.get_mut("error").and_then(|e| e.as_object_mut()) {
        error.remove("request_id");
    }
    (status, body, elapsed)
}

#[tokio::test]
async fn login_rejects_unknown_and_known_accounts_with_the_same_answer() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    register_user(&base, "enum_known", "enum_known@example.com", PASSWORD).await;

    let (known_status, known_body, _) = failed_login(&client, &base, "enum_known").await;
    let (unknown_status, unknown_body, _) = failed_login(&client, &base, "enum_nobody").await;

    assert_eq!(known_status, unknown_status);
    assert_eq!(
        known_body, unknown_body,
        "the rejection body distinguishes an existing account from a missing one"
    );
}

/// The timing half of the same property.
///
/// Every sample uses a *fresh* account: the fifth failure in a row locks one
/// out, and a locked account is rejected by a different (fast) path with a
/// different message, which would measure something else entirely.
#[tokio::test]
async fn login_spends_the_same_work_on_unknown_and_known_accounts() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    const SAMPLES: usize = 3;
    for i in 0..SAMPLES {
        register_user(
            &base,
            &format!("enum_timing_{i}"),
            &format!("enum_timing_{i}@example.com"),
            PASSWORD,
        )
        .await;
    }

    // The floor of several samples, not the mean: the true cost is the minimum,
    // everything above it is scheduler noise from the parallel test runner.
    let mut known = Duration::MAX;
    let mut unknown = Duration::MAX;
    for i in 0..SAMPLES {
        let (_, _, took) = failed_login(&client, &base, &format!("enum_timing_{i}")).await;
        known = known.min(took);
        let (_, _, took) = failed_login(&client, &base, &format!("enum_ghost_{i}")).await;
        unknown = unknown.min(took);
    }

    // A wide band on purpose — this asserts "one full Argon2 either way", not a
    // benchmark. The bug it guards against skipped the hash entirely, so the
    // unknown-account branch used to land near zero.
    let (known, unknown) = (known.as_secs_f64(), unknown.as_secs_f64());
    assert!(
        unknown > known * 0.5,
        "an unknown username was rejected in {unknown:.4}s against {known:.4}s for a real \
         account — the response time still says whether the account exists"
    );
}
