//! Deactivating an account has to close every door, not just the web login.
//!
//! `is_active = false` is the standard answer to an offboarding or a
//! compromised account, and an administrator who flips it expects the person to
//! be *out*. Before this suite existed the flag only gated `POST /users/login`:
//! the account's Personal Access Token still authenticated the REST API and
//! git-over-HTTP, `docker login` still succeeded against the registry, and the
//! password-reset flow would hand the disabled account a fresh working session
//! with no administrator in the loop.
//!
//! The SSH half of the same class lives in `rg-ssh/tests/deactivated_ssh_tests.rs`.

use base64::Engine as _;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

use crate::common::{register_full, spawn_test_app_with_db};

const INFO_REFS: &str = "info/refs?service=git-upload-pack";
const PASSWORD: &str = "Qz7$wRtm";

async fn deactivate(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, None, Some(false))
        .await
        .expect("deactivate user");
}

async fn create_pat(base: &str, jwt: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({ "name": "cli", "scopes": "user, repo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

async fn create_private_repo(base: &str, jwt: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create private repo failed");
}

/// A PAT is a standing delegation of the account's rights — revoking the
/// account has to revoke the delegation, in both API families that accept one.
#[tokio::test]
async fn deactivating_an_account_revokes_its_personal_access_token() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "deact_pat", "deact_pat@example.com").await;
    create_private_repo(&base, &jwt, "vault").await;
    let pat = create_pat(&base, &jwt).await;
    let client = reqwest::Client::new();

    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("deact_pat:{}", pat))
    );
    let git_url = format!("{}/git/deact_pat/vault/{}", base, INFO_REFS);

    // Baseline: while the account is live the token opens both doors, so a
    // rejection below is the deactivation and not a broken fixture.
    let rest_before = client
        .get(format!("{}/api/v1/users/me", base))
        .bearer_auth(&pat)
        .send()
        .await
        .unwrap();
    assert_eq!(rest_before.status(), 200, "PAT should work while active");
    let git_before = client
        .get(&git_url)
        .header("Authorization", &basic)
        .send()
        .await
        .unwrap();
    assert_eq!(git_before.status(), 200, "PAT should clone while active");

    deactivate(&db, user_id).await;

    let rest_after = client
        .get(format!("{}/api/v1/users/me", base))
        .bearer_auth(&pat)
        .send()
        .await
        .unwrap();
    assert_eq!(
        rest_after.status(),
        401,
        "PAT of a deactivated account still authenticates the REST API"
    );

    let git_after = client
        .get(&git_url)
        .header("Authorization", &basic)
        .send()
        .await
        .unwrap();
    assert_eq!(
        git_after.status(),
        401,
        "PAT of a deactivated account still clones over git-over-HTTP"
    );
}

/// `docker login` runs through the registry's Basic-auth path, which resolved
/// the password without ever looking at the account's standing.
#[tokio::test]
async fn deactivating_an_account_blocks_docker_login() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "deact_oci", "deact_oci@example.com").await;
    create_private_repo(&base, &jwt, "image").await;
    let client = reqwest::Client::new();

    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("deact_oci:{}", PASSWORD))
    );
    let scope = "repository:deact_oci/image:pull,push";

    let request_token = |auth: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let resp = client
                .get(format!("{}/v2/auth/token", base))
                .query(&[("service", "forgekeep-registry"), ("scope", scope)])
                .header(reqwest::header::AUTHORIZATION, auth)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "token request failed");
            let body: serde_json::Value = resp.json().await.unwrap();
            rg_core::auth::oci_token::validate_oci_token(
                body["token"].as_str().unwrap(),
                "test-secret-key",
            )
            .expect("valid OCI token")
        }
    };

    let before = request_token(basic.clone()).await;
    assert_eq!(before.sub, "deact_oci", "baseline: credentials are accepted");
    assert_eq!(
        before.scope.as_deref(),
        Some(scope),
        "baseline: owner may push to their own private image"
    );

    deactivate(&db, user_id).await;

    let after = request_token(basic).await;
    assert_eq!(
        after.sub, "anonymous",
        "docker login with a deactivated account still authenticates"
    );
    assert!(
        after.scope.is_none(),
        "deactivated account still granted a push scope on its private image"
    );
}

/// The reset flow is a way back in that needs no administrator: the mail lands
/// in a mailbox the offboarded user still controls, and the reset ends by
/// minting a session. So it must not start for a disabled account.
#[tokio::test]
async fn a_deactivated_account_gets_no_password_reset_token() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, live_id) = register_full(&base, "deact_fp_live", "deact_fp_live@example.com").await;
    let (_jwt2, dead_id) = register_full(&base, "deact_fp_dead", "deact_fp_dead@example.com").await;
    deactivate(&db, dead_id).await;

    let client = reqwest::Client::new();
    for email in ["deact_fp_live@example.com", "deact_fp_dead@example.com"] {
        let resp = client
            .post(format!("{}/api/v1/users/forgot-password", base))
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await
            .unwrap();
        // Uniform 200 either way — the answer must not enumerate accounts.
        assert_eq!(resp.status(), 200, "forgot-password should always answer 200");
    }

    assert_eq!(
        reset_tokens_for(&db, live_id).await,
        1,
        "baseline: an active account does get a reset token"
    );
    assert_eq!(
        reset_tokens_for(&db, dead_id).await,
        0,
        "forgot-password issued a reset token to a deactivated account"
    );
}

/// An account can be disabled inside the fifteen minutes its reset link is
/// alive for, so the token has to be re-checked when it is spent — otherwise
/// the disabled account walks out with a JWT.
#[tokio::test]
async fn a_reset_token_stops_working_when_the_account_is_disabled() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, live_id) = register_full(&base, "deact_rp_live", "deact_rp_live@example.com").await;
    let (_jwt2, dead_id) = register_full(&base, "deact_rp_dead", "deact_rp_dead@example.com").await;

    let live_token = issue_reset_token(&db, live_id, "raw-token-live").await;
    let dead_token = issue_reset_token(&db, dead_id, "raw-token-dead").await;
    deactivate(&db, dead_id).await;

    let client = reqwest::Client::new();
    let reset = |token: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .post(format!("{}/api/v1/users/reset-password", base))
                .json(&serde_json::json!({ "token": token, "new_password": "Nw9#pLqz" }))
                .send()
                .await
                .unwrap()
        }
    };

    let live_resp = reset(live_token).await;
    assert_eq!(
        live_resp.status(),
        200,
        "baseline: a live account can still spend its reset token"
    );
    let live_body: serde_json::Value = live_resp.json().await.unwrap();
    assert!(
        live_body["token"].as_str().is_some_and(|t| !t.is_empty()),
        "baseline: a successful reset returns a session token"
    );

    let dead_resp = reset(dead_token).await;
    assert_eq!(
        dead_resp.status(),
        400,
        "a deactivated account reset its own password"
    );
    let dead_body: serde_json::Value = dead_resp.json().await.unwrap();
    assert!(
        dead_body.get("token").is_none(),
        "a deactivated account was handed a session token by the reset flow"
    );
}

async fn reset_tokens_for(db: &rg_db::DatabaseConnection, user_id: i64) -> u64 {
    use rg_db::entities::password_reset_token;
    password_reset_token::Entity::find()
        .filter(password_reset_token::Column::UserId.eq(user_id))
        .filter(password_reset_token::Column::Used.eq(false))
        .count(db)
        .await
        .expect("count reset tokens")
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
