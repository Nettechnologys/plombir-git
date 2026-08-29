//! card_dca5ca190649: one reset link, one successful reset.
//!
//! `POST /users/reset-password` read the token, decided "unspent and unexpired"
//! in application memory, then spent it a full Argon2 pass later with an
//! unconditional `UPDATE ... WHERE id = ?` whose row count nobody read. Requests
//! carrying the same link all passed that check, all wrote
//! `users.password_hash`, all revoked the account's sessions, and all were
//! answered `200` with a session. Only the last write survived — so every loser
//! held a seven-day JWT for a password the account does not have, and believed
//! its reset had landed.
//!
//! The single-use property already had a test, `the_reset_token_of_an_mfa_
//! account_is_single_use`, and it passed throughout: it replays the link
//! *after* the first reset returned, which is the one ordering the old code got
//! right. The hole was only ever reachable concurrently.
//!
//! The assertions below are deliberately symmetric — they never name which
//! request must win, only that exactly one did and that the account agrees with
//! it. An invariant that holds under every interleaving is the property worth
//! testing; pinning a winner would only test the scheduler.

use crate::common::{register_full, spawn_test_app_with_db};

const OLD_PASSWORD: &str = "Qz7$wRtm";

/// Distinct, valid passwords — one per racing request, so the account's stored
/// hash names its winner unambiguously.
const CANDIDATES: [&str; 6] = [
    "Aa1!race01",
    "Bb2@race02",
    "Cc3#race03",
    "Dd4$race04",
    "Ee5%race05",
    "Ff6^race06",
];

/// Plant a reset token straight into the database — the raw value only ever
/// leaves the server by email, which the test harness cannot read.
async fn issue_reset_token(db: &rg_db::DatabaseConnection, user_id: i64, raw: &str) {
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
}

async fn login_status(base: &str, username: &str, password: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({ "login": username, "password": password }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// Status, session token and session cookie of one reset attempt.
struct Attempt {
    password: &'static str,
    status: u16,
    session_token: String,
    has_session_cookie: bool,
}

async fn reset(base: String, token: &'static str, password: &'static str) -> Attempt {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/users/reset-password", base))
        .json(&serde_json::json!({ "token": token, "new_password": password }))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let has_session_cookie = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|cookie| {
            cookie.starts_with("forgekeep_token=") && !cookie.starts_with("forgekeep_token=;")
        });
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    let session_token = body["token"].as_str().unwrap_or_default().to_string();
    Attempt {
        password,
        status,
        session_token,
        has_session_cookie,
    }
}

/// The whole finding: six requests, one link, one reset.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_resets_with_one_link_produce_exactly_one_reset() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, user_id) = register_full(&base, "reset_race", "reset_race@example.com").await;

    const RAW_TOKEN: &str = "raw-token-reset-race";
    issue_reset_token(&db, user_id, RAW_TOKEN).await;

    // Handed to the runtime as tasks so the requests are genuinely in flight
    // together, and every one of them reads the link before any of them has
    // written to it.
    let handles: Vec<_> = CANDIDATES
        .iter()
        .map(|password| {
            let base = base.clone();
            tokio::spawn(reset(base, RAW_TOKEN, password))
        })
        .collect();

    let mut attempts = Vec::with_capacity(CANDIDATES.len());
    for handle in handles {
        attempts.push(handle.await.expect("reset request task panicked"));
    }

    let winners: Vec<&Attempt> = attempts.iter().filter(|a| a.status == 200).collect();
    assert_eq!(
        winners.len(),
        1,
        "one reset link produced {} successful resets: {:?}",
        winners.len(),
        attempts
            .iter()
            .map(|a| (a.password, a.status))
            .collect::<Vec<_>>()
    );
    let winner = winners[0];

    // A loser holds a dead link and must be told so — the same `400` an expired
    // link gets, never a `500` and never a session.
    for attempt in attempts.iter().filter(|a| a.status != 200) {
        assert_eq!(
            attempt.status, 400,
            "a request that lost the link must be answered like any holder of a dead link, \
             got {} for {}",
            attempt.status, attempt.password
        );
        assert!(
            attempt.session_token.is_empty(),
            "a request whose reset did not happen was handed a session token ({})",
            attempt.password
        );
        assert!(
            !attempt.has_session_cookie,
            "a request whose reset did not happen was handed an auth cookie ({})",
            attempt.password
        );
    }

    // The account has to agree with the answer the winner was given.
    assert!(
        !winner.session_token.is_empty(),
        "the winning reset returned no session token"
    );
    assert_eq!(
        login_status(&base, "reset_race", winner.password).await,
        200,
        "the password of the request answered 200 is not the account's password"
    );
    assert_eq!(
        login_status(&base, "reset_race", OLD_PASSWORD).await,
        401,
        "the old password still works after a reset that reported success"
    );
    for attempt in attempts.iter().filter(|a| a.status != 200) {
        assert_eq!(
            login_status(&base, "reset_race", attempt.password).await,
            401,
            "a refused reset changed the password anyway ({})",
            attempt.password
        );
    }

    // And the link is spent exactly once, not merely overwritten by whoever ran
    // last: `reset_password` clears the account's remaining links after a
    // successful reset, so nothing unspent may survive either.
    use rg_db::sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
    let unspent = rg_db::entities::password_reset_token::Entity::find()
        .filter(rg_db::entities::password_reset_token::Column::UserId.eq(user_id))
        .filter(rg_db::entities::password_reset_token::Column::Used.eq(false))
        .count(&db)
        .await
        .expect("count the account's unspent links");
    assert_eq!(unspent, 0, "a spendable link survived a completed reset");
}

/// Inject the retirement marker from the token-spend statement itself. Under
/// the old autocommit sequence that marker landed after the user read, the stale
/// ActiveModel still wrote the password, and the handler returned 200 + JWT.
/// The transactional implementation sees the marker in its conditional user
/// update and rolls the entire statement — marker and token claim included —
/// back to the pre-request state, then answers the same 400 as a dead link.
#[tokio::test]
async fn retirement_inside_the_token_spend_is_400_with_no_session_or_spent_link() {
    use rg_db::sea_orm::{ConnectionTrait, Statement};

    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, user_id) = register_full(
        &base,
        "reset_retirement_http",
        "reset_retirement_http@example.com",
    )
    .await;
    const RAW_TOKEN: &str = "raw-token-reset-retirement-http";
    const REFUSED_PASSWORD: &str = "Aa1!retirement";
    issue_reset_token(&db, user_id, RAW_TOKEN).await;

    db.execute(Statement::from_string(
        rg_db::sea_orm::DatabaseBackend::Sqlite,
        format!(
            "CREATE TRIGGER retire_user_during_reset_token_spend \
             BEFORE UPDATE OF used ON password_reset_tokens \
             WHEN OLD.user_id = {user_id} AND OLD.used = 0 AND NEW.used = 1 \
             BEGIN UPDATE users SET deleted_at = CURRENT_TIMESTAMP WHERE id = {user_id}; END"
        ),
    ))
    .await
    .expect("install deterministic password-reset retirement trigger");

    let attempt = reset(base.clone(), RAW_TOKEN, REFUSED_PASSWORD).await;
    assert_eq!(
        attempt.status, 400,
        "a reset that lost to retirement was not classified as an invalid link"
    );
    assert!(
        attempt.session_token.is_empty(),
        "the refused reset returned a JWT"
    );
    assert!(
        !attempt.has_session_cookie,
        "the refused reset returned an auth cookie"
    );
    assert_eq!(
        login_status(&base, "reset_retirement_http", OLD_PASSWORD).await,
        200,
        "the original password stopped working after a refused reset"
    );
    assert_eq!(
        login_status(&base, "reset_retirement_http", REFUSED_PASSWORD).await,
        401,
        "the refused reset password was stored"
    );

    use sha2::Digest;
    let token_hash = hex::encode(sha2::Sha256::digest(RAW_TOKEN.as_bytes()));
    let token = rg_db::ops::password_reset_token_ops::find_by_hash(&db, &token_hash)
        .await
        .expect("read the reset link after the HTTP refusal")
        .expect("the refused HTTP reset deleted its link");
    assert!(!token.used, "the refused HTTP reset spent its link");
    let user = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .expect("read the account after the HTTP refusal")
        .expect("the trigger deleted the account instead of marking it");
    assert!(
        user.deleted_at.is_none(),
        "the injected marker escaped the rolled-back reset transaction"
    );
}
