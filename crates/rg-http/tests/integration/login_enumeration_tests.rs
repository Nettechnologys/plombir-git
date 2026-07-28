//! `POST /api/v1/users/login` must not answer "does this account exist?".
//!
//! The response *body* has always been unified to `invalid credentials`, but a
//! login for a real account paid for an Argon2 verification (tens of
//! milliseconds, on purpose) while an unknown username was rejected the moment
//! the database lookup came back empty. That difference is an order of
//! magnitude above network noise, which made a dictionary sweep over usernames
//! a perfectly good enumeration oracle.

use super::common::{register_user, spawn_test_app};

const PASSWORD: &str = "Qz7$wRtm";

/// One failed login, returning the status and the body.
async fn failed_login(
    client: &reqwest::Client,
    base: &str,
    login: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({"login": login, "password": "definitely-not-the-password"}))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let mut body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 401, "expected a rejection for '{login}': {body}");
    // Per-request correlation id — noise for a body comparison.
    if let Some(error) = body.get_mut("error").and_then(|e| e.as_object_mut()) {
        error.remove("request_id");
    }
    (status, body)
}

#[tokio::test]
async fn login_rejects_unknown_and_known_accounts_with_the_same_answer() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    register_user(&base, "enum_known", "enum_known@example.com", PASSWORD).await;

    let (known_status, known_body) = failed_login(&client, &base, "enum_known").await;
    let (unknown_status, unknown_body) = failed_login(&client, &base, "enum_nobody").await;

    assert_eq!(known_status, unknown_status);
    assert_eq!(
        known_body, unknown_body,
        "the rejection body distinguishes an existing account from a missing one"
    );
}
