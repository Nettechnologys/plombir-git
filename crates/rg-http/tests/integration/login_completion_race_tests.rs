//! Login completion is the lifecycle boundary between a proved credential and
//! anything which says authentication succeeded.
//!
//! SQLite triggers make retirement and physical deletion win *inside* the real
//! conditional user-row update. That is deterministic, unlike a timeout-based
//! race, and it proves the handler orders success audit/login-log publication
//! and JWT/challenge issuance after the finalizer.

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, Statement};

use crate::common::{register_full, source_scan, spawn_test_app_with_db};

const PASSWORD: &str = "Qz7$wRtm";

#[derive(Clone, Copy)]
enum LifecycleLoss {
    Retirement,
    Delete,
}

impl LifecycleLoss {
    fn label(self) -> &'static str {
        match self {
            Self::Retirement => "retirement",
            Self::Delete => "delete",
        }
    }
}

async fn install_lifecycle_loss_on_login_finalizer(
    db: &rg_db::DatabaseConnection,
    user_id: i64,
    trigger_name: &str,
    loss: LifecycleLoss,
) {
    let mutation = match loss {
        LifecycleLoss::Retirement => {
            "UPDATE users SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE id = OLD.id;"
        }
        LifecycleLoss::Delete => "DELETE FROM users WHERE id = OLD.id;",
    };
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER {trigger_name} BEFORE UPDATE OF last_login_at ON users \
             WHEN OLD.id = {user_id} \
             BEGIN {mutation} SELECT RAISE(IGNORE); END"
        ),
    ))
    .await
    .expect("install the competing account lifecycle mutation");
}

async fn audit_count(db: &rg_db::DatabaseConnection) -> u64 {
    rg_db::ops::audit_log_ops::list_paginated(db, 0, 1, None, None, None, None, None)
        .await
        .expect("count audit rows")
        .1
}

async fn successful_login_count(db: &rg_db::DatabaseConnection) -> u64 {
    rg_db::ops::login_log_ops::Entity::find()
        .filter(rg_db::entities::login_log::Column::Success.eq(true))
        .count(db)
        .await
        .expect("count successful login rows")
}

async fn login(base: &str, username: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({
            "login": username,
            "password": PASSWORD,
        }))
        .send()
        .await
        .expect("login request")
}

fn live_cookie(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|cookie| cookie.split(';').next())
        .filter_map(|pair| pair.trim().strip_prefix(&format!("{name}=")))
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

async fn assert_no_published_login(
    response: reqwest::Response,
    context: &str,
) -> serde_json::Value {
    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "{context} was not classified as a rejected authentication"
    );
    assert_eq!(
        live_cookie(&response, "plombir_git_token"),
        None,
        "{context} issued an auth cookie"
    );
    assert_eq!(
        live_cookie(&response, "plombir_git_mfa_challenge"),
        None,
        "{context} issued an MFA challenge"
    );
    response
        .json()
        .await
        .expect("authentication rejection is JSON")
}

async fn setup_mfa(base: &str, token: &str) -> String {
    let client = reqwest::Client::new();
    let setup = client
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(token)
        .send()
        .await
        .expect("set up MFA");
    assert_eq!(setup.status(), reqwest::StatusCode::OK);
    let secret = setup
        .json::<serde_json::Value>()
        .await
        .expect("MFA setup response")["secret"]
        .as_str()
        .expect("MFA setup secret")
        .to_string();
    let enable = client
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "code": current_code(&secret) }))
        .send()
        .await
        .expect("enable MFA");
    assert_eq!(enable.status(), reqwest::StatusCode::OK);
    secret
}

fn current_code(secret: &str) -> String {
    let bytes = totp_rs::Secret::Encoded(secret.to_string())
        .to_bytes()
        .expect("the setup secret is base32");
    totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        None,
        String::new(),
    )
    .expect("build the authenticator side of the handshake")
    .generate_current()
    .expect("read the current TOTP step")
}

#[tokio::test]
async fn password_completion_losing_to_retirement_or_delete_publishes_nothing() {
    let (base, db) = spawn_test_app_with_db().await;

    for (index, loss) in [LifecycleLoss::Retirement, LifecycleLoss::Delete]
        .into_iter()
        .enumerate()
    {
        let username = format!("login_finish_{}_{}", loss.label(), index);
        let (_, user_id) =
            register_full(&base, &username, &format!("{username}@example.invalid")).await;
        let audit_before = audit_count(&db).await;
        let log_before = successful_login_count(&db).await;
        install_lifecycle_loss_on_login_finalizer(
            &db,
            user_id,
            &format!("lose_password_login_{index}"),
            loss,
        )
        .await;

        let body = assert_no_published_login(
            login(&base, &username).await,
            &format!("password login vs {}", loss.label()),
        )
        .await;
        assert!(
            body["token"].as_str().is_none_or(str::is_empty),
            "the rejected password response carried a bearer token: {body}"
        );
        assert_eq!(audit_count(&db).await, audit_before);
        assert_eq!(successful_login_count(&db).await, log_before);
    }
}

#[tokio::test]
async fn mfa_challenge_losing_to_retirement_or_delete_is_not_issued() {
    let (base, db) = spawn_test_app_with_db().await;

    for (index, loss) in [LifecycleLoss::Retirement, LifecycleLoss::Delete]
        .into_iter()
        .enumerate()
    {
        let username = format!("mfa_challenge_{}_{}", loss.label(), index);
        let (token, user_id) =
            register_full(&base, &username, &format!("{username}@example.invalid")).await;
        setup_mfa(&base, &token).await;
        let audit_before = audit_count(&db).await;
        let log_before = successful_login_count(&db).await;
        install_lifecycle_loss_on_login_finalizer(
            &db,
            user_id,
            &format!("lose_mfa_challenge_{index}"),
            loss,
        )
        .await;

        assert_no_published_login(
            login(&base, &username).await,
            &format!("MFA challenge vs {}", loss.label()),
        )
        .await;
        assert_eq!(audit_count(&db).await, audit_before);
        assert_eq!(successful_login_count(&db).await, log_before);
    }
}

#[tokio::test]
async fn mfa_completion_losing_to_retirement_or_delete_issues_no_session() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    for (index, loss) in [LifecycleLoss::Retirement, LifecycleLoss::Delete]
        .into_iter()
        .enumerate()
    {
        let username = format!("mfa_finish_{}_{}", loss.label(), index);
        let (token, user_id) =
            register_full(&base, &username, &format!("{username}@example.invalid")).await;
        let secret = setup_mfa(&base, &token).await;
        let primary = login(&base, &username).await;
        assert_eq!(primary.status(), reqwest::StatusCode::OK);
        let challenge = live_cookie(&primary, "plombir_git_mfa_challenge")
            .expect("the healthy primary factor issued an MFA challenge");
        let audit_before = audit_count(&db).await;
        let log_before = successful_login_count(&db).await;
        install_lifecycle_loss_on_login_finalizer(
            &db,
            user_id,
            &format!("lose_mfa_finish_{index}"),
            loss,
        )
        .await;

        let response = client
            .post(format!("{base}/api/v1/users/mfa/verify"))
            .header(
                reqwest::header::COOKIE,
                format!("plombir_git_mfa_challenge={challenge}"),
            )
            .json(&serde_json::json!({
                "username": username,
                "code": current_code(&secret),
                "backup": false,
            }))
            .send()
            .await
            .expect("finish MFA login");
        assert_no_published_login(response, &format!("MFA completion vs {}", loss.label())).await;
        assert_eq!(audit_count(&db).await, audit_before);
        assert_eq!(successful_login_count(&db).await, log_before);
    }
}

#[tokio::test]
async fn login_finalizer_failure_is_a_server_error_not_a_credential_verdict() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, user_id) = register_full(
        &base,
        "login_finalizer_outage",
        "login_finalizer_outage@example.invalid",
    )
    .await;
    let audit_before = audit_count(&db).await;
    let log_before = successful_login_count(&db).await;
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER fail_login_finalizer BEFORE UPDATE OF last_login_at ON users \
             WHEN OLD.id = {user_id} \
             BEGIN SELECT RAISE(ABORT, 'forced login finalizer failure'); END"
        ),
    ))
    .await
    .expect("install the finalizer failure");

    let response = login(&base, "login_finalizer_outage").await;
    assert!(
        response.status().is_server_error(),
        "a failed finalizer was blamed on the credential: {}",
        response.status()
    );
    assert_ne!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(live_cookie(&response, "plombir_git_token"), None);
    assert_eq!(audit_count(&db).await, audit_before);
    assert_eq!(successful_login_count(&db).await, log_before);
}

fn function_body(path: &str, name: &str) -> String {
    let source = std::fs::read_to_string(source_scan::src_root().join(path))
        .unwrap_or_else(|error| panic!("read {path}: {error}"));
    source_scan::functions(&source)
        .into_iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("{name} is declared in {path}"))
        .body
}

fn code_position(body: &str, needle: &str, context: &str) -> usize {
    source_scan::rust_code_only(body)
        .find(needle)
        .unwrap_or_else(|| panic!("{context} no longer contains `{needle}`"))
}

/// SSO needs a real upstream exchange and passkey completion needs a real
/// authenticator before either can reach its final lines. Their independent
/// protocol suites prove the healthy flows; this guard pins the load-bearing
/// ordering which a fault at the shared DB seam exercises.
#[test]
fn every_http_login_path_finalizes_before_it_publishes_success() {
    let password = function_body("api/users.rs", "login");
    let password_finalize =
        code_position(&password, "finalize_primary_login(", "password/LDAP login");
    assert!(
        password_finalize
            < code_position(&password, "rg_core::audit::record(", "password/LDAP login")
            && password_finalize
                < code_position(&password, "generate_mfa_challenge(", "password/LDAP login")
            && password_finalize
                < code_position(&password, "build_auth_cookie(", "password/LDAP login"),
        "password/LDAP login published audit, challenge, or session before lifecycle finalization"
    );

    let mfa_finalizer = function_body("api/mfa.rs", "finalize_mfa_login");
    assert!(
        code_position(&mfa_finalizer, "record_successful_login(", "MFA finalizer",)
            < code_position(&mfa_finalizer, "log_attempt(", "MFA finalizer"),
        "MFA success was logged before the lifecycle finalizer"
    );
    let mfa = function_body("api/mfa.rs", "verify_mfa");
    assert!(
        code_position(&mfa, "finalize_mfa_login(", "MFA verify")
            < code_position(&mfa, "generate_token(", "MFA verify"),
        "MFA verification minted a session before lifecycle finalization"
    );

    let sso = function_body("api/sso.rs", "callback");
    let sso_finalize = code_position(&sso, "finalize_primary_login(", "SSO callback");
    assert!(
        sso_finalize < code_position(&sso, "log_attempt(", "SSO callback")
            && sso_finalize < code_position(&sso, "generate_mfa_challenge(", "SSO callback")
            && sso_finalize < code_position(&sso, "generate_token(", "SSO callback"),
        "SSO callback published success, challenge, or session before lifecycle finalization"
    );
}
