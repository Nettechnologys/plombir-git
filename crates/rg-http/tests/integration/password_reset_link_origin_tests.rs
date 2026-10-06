//! A password-reset link may only name the instance's configured address
//! (card_e67aeb8c09ca).
//!
//! `POST /users/forgot-password` is anonymous and the link it mails goes into a
//! mailbox the requester does not control. Built from the request `Host`, it
//! let anyone ask for a victim's reset with `Host: attacker.example` and have
//! the real token delivered as a link to their own host — one click from the
//! victim hands it over. With mail configured and no `external_url` the reset
//! is now refused before a token exists; with `external_url` it goes ahead.

use crate::common::{register_full, spawn_test_app_with_overrides, StateOverrides};
use sea_orm::{ConnectionTrait, Statement};

const EMAIL: &str = "reset_victim@example.com";

/// SMTP that nothing listens on: the delivery is detached and its failure is
/// logged, so what these tests observe is the decision taken before it.
fn unreachable_smtp() -> rg_core::email::SmtpConfig {
    rg_core::email::SmtpConfig::new("127.0.0.1", 9, "mailer", "secret", "noreply@example.com")
}

async fn reset_tokens(db: &rg_db::DatabaseConnection) -> i64 {
    db.query_one(Statement::from_string(
        db.get_database_backend(),
        "SELECT COUNT(*) AS n FROM password_reset_tokens".to_string(),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "n")
    .unwrap()
}

async fn request_reset(overrides: StateOverrides) -> (reqwest::StatusCode, i64) {
    let (base, db) = spawn_test_app_with_overrides(overrides).await;
    register_full(&base, "reset_victim", EMAIL).await;
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/forgot-password"))
        .header(reqwest::header::HOST, "attacker.example")
        .json(&serde_json::json!({ "email": EMAIL }))
        .send()
        .await
        .unwrap()
        .status();
    (status, reset_tokens(&db).await)
}

#[tokio::test]
async fn mail_without_external_url_issues_no_token_for_a_host_the_requester_chose() {
    let (status, tokens) = request_reset(StateOverrides {
        smtp_config: Some(unreachable_smtp()),
        ..Default::default()
    })
    .await;
    // The trace first: a refusal that came after the token was minted would
    // already have handed the mail task a link to attacker.example.
    assert_eq!(
        tokens, 0,
        "no reset token may exist for a Host-derived link"
    );
    assert_eq!(
        status, 500,
        "a configuration the server cannot honour is its own fault"
    );
}

#[tokio::test]
async fn a_configured_external_url_lets_the_reset_go_ahead() {
    let (status, tokens) = request_reset(StateOverrides {
        smtp_config: Some(unreachable_smtp()),
        external_url: Some("https://git.example.test".to_string()),
        ..Default::default()
    })
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        tokens, 1,
        "the reset must be issued once the link has an honest base"
    );
}
