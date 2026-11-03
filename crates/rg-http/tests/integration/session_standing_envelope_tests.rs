//! `session_standing_middleware` runs *outside* every subtree router, so its
//! refusal has to speak the envelope of whichever subtree the request was aimed
//! at. Before card_9a848c73f48d the gate returned `text/plain` for every
//! refusal, which:
//!
//! * left REST clients under `/api/v1` without the `{error:{code,message}}`
//!   body the OpenAPI document and every frontend branch on;
//! * left OCI clients under `/v2` without the `{errors:[{code,message}]}` body
//!   `docker pull` reads and reports.
//!
//! Two refusal scenarios are exercised — a revoked session (401) and a database
//! outage while resolving the account (503) — and each is fired against
//! `/api/v1`, `/v2` and the git transport root path so a fix that repairs one
//! envelope and breaks another cannot go green.
//!
//! The git-HTTP path stays `text/plain`: git-http clients recognise
//! `WWW-Authenticate: Basic` and prompt for credentials on a 401, matching
//! [`crate::git_http::check_git_access`]'s own denial shape.
//!
//! Anonymous requests still touch no database: presented no session, the
//! middleware never asks the pool and returns whatever the handler does.

use axum::http::StatusCode;
use reqwest::header;

use crate::common::{register_full, spawn_test_app_with_db};

/// A revoked session on `/api/v1/users/me` must arrive as the API's own
/// `{error:{code,message}}` envelope — the same shape the frontend and MCP
/// clients branch on for every other 401.
#[tokio::test]
async fn a_revoked_session_on_api_v1_answers_in_the_api_error_envelope() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "std_v1_rev", "std_v1_rev@example.test").await;

    rg_db::ops::user_ops::invalidate_sessions(&db, user_id)
        .await
        .expect("bump session_version to revoke the JWT");

    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(&jwt)
        .send()
        .await
        .expect("send /api/v1/users/me with a revoked JWT");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("application/json"),
        "REST client was handed {content_type} instead of JSON"
    );
    let body: serde_json::Value = response.json().await.expect("JSON body");
    assert_eq!(
        body["error"]["code"], "UNAUTHORIZED",
        "REST envelope must carry a machine-readable error code, got: {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "REST envelope must carry a non-empty message, got: {body}"
    );
}

/// A revoked session on `/v2/...` must arrive as the OCI spec's
/// `{errors:[{code,message}]}` envelope — `docker pull` reads that body and
/// nothing else, so text/plain leaves the operator with no diagnostic at all.
#[tokio::test]
async fn a_revoked_session_on_v2_answers_in_the_oci_error_envelope() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "std_v2_rev", "std_v2_rev@example.test").await;

    rg_db::ops::user_ops::invalidate_sessions(&db, user_id)
        .await
        .expect("bump session_version to revoke the JWT");

    let response = reqwest::Client::new()
        .get(format!("{base}/v2/std_v2_rev/std_v2_rev/tags/list"))
        .bearer_auth(&jwt)
        .send()
        .await
        .expect("send a /v2 request with a revoked JWT");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("application/json"),
        "OCI client was handed {content_type} instead of JSON"
    );
    let body: serde_json::Value = response.json().await.expect("JSON body");
    assert_eq!(
        body["errors"][0]["code"], "UNAUTHORIZED",
        "OCI envelope must carry a spec-conformant error code, got: {body}"
    );
    assert!(
        body["errors"][0]["message"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "OCI envelope must carry a non-empty message, got: {body}"
    );
}

/// A revoked session on a git-HTTP path stays `text/plain` — git-http clients
/// do not read the JSON envelopes above and rely on `WWW-Authenticate: Basic`
/// to prompt for credentials.
#[tokio::test]
async fn a_revoked_session_on_git_http_answers_with_a_basic_challenge() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "std_git_rev", "std_git_rev@example.test").await;

    rg_db::ops::user_ops::invalidate_sessions(&db, user_id)
        .await
        .expect("bump session_version to revoke the JWT");

    let response = reqwest::Client::new()
        .get(format!(
            "{base}/std_git_rev/std_git_rev/info/refs?service=git-upload-pack"
        ))
        .bearer_auth(&jwt)
        .send()
        .await
        .expect("send a git-HTTP request with a revoked JWT");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        challenge.to_ascii_lowercase().contains("basic"),
        "git transport 401 must carry a Basic challenge, got: {challenge:?}"
    );
    let body = response.text().await.expect("text body");
    // JSON envelopes are refused here on purpose — git-http reads text/plain.
    assert!(
        !body.trim_start().starts_with('{'),
        "git transport must not be handed a JSON API envelope, got: {body}"
    );
}

/// A closed database pool while the gate resolves an account's standing on
/// `/api/v1` must arrive as the API's own envelope with a retryable 503, so
/// clients and load balancers see the same shape they see on every other
/// retryable failure.
#[tokio::test]
async fn a_db_outage_on_api_v1_answers_in_the_api_error_envelope() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, _user_id) = register_full(&base, "std_v1_outage", "std_v1_outage@example.test").await;

    db.close().await.expect("close session-standing test pool");

    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(&jwt)
        .send()
        .await
        .expect("send /api/v1/users/me under a closed pool");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("application/json"),
        "REST client was handed {content_type} instead of JSON"
    );
    let body: serde_json::Value = response.json().await.expect("JSON body");
    assert_eq!(
        body["error"]["code"], "DB_UNAVAILABLE",
        "REST envelope must classify the outage under DB_UNAVAILABLE, got: {body}"
    );
}

/// The same outage on a `/v2` route must arrive in the OCI envelope so
/// `docker push` sees a retryable failure with a body it can parse.
#[tokio::test]
async fn a_db_outage_on_v2_answers_in_the_oci_error_envelope() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, _user_id) = register_full(&base, "std_v2_outage", "std_v2_outage@example.test").await;

    db.close().await.expect("close session-standing test pool");

    let response = reqwest::Client::new()
        .get(format!("{base}/v2/std_v2_outage/std_v2_outage/tags/list"))
        .bearer_auth(&jwt)
        .send()
        .await
        .expect("send a /v2 request under a closed pool");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("application/json"),
        "OCI client was handed {content_type} instead of JSON"
    );
    let body: serde_json::Value = response.json().await.expect("JSON body");
    assert!(
        body["errors"][0]["code"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "OCI envelope must carry a machine-readable error code, got: {body}"
    );
}

/// Anonymous callers still touch no database — `session_gate_cost_tests`
/// pins that invariant by measuring the middleware's cost, so nothing here has
/// to duplicate it. What DOES belong here: a request that carries no
/// session-version must fall through to the handler unchanged, so an unrelated
/// closed-pool 503 from below never surfaces as a session-gate refusal.
#[tokio::test]
async fn an_anonymous_v1_request_falls_through_the_session_gate() {
    // Live pool: `/api/v1/instance` is Public and does not need any credential.
    // With no session presented the gate skips its lookup and the handler
    // answers a 200 — the shape of "the gate did nothing".
    let (base, _db) = spawn_test_app_with_db().await;

    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/instance"))
        .send()
        .await
        .expect("send anonymous /api/v1/instance");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "anonymous public request must not be refused by the session gate"
    );
}

#[tokio::test]
async fn revoked_browser_cookie_recovers_public_pages_but_not_private_access() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "browser-recovery", "recovery@example.test").await;
    rg_db::ops::user_ops::invalidate_sessions(&db, user_id)
        .await
        .unwrap();
    let client = reqwest::Client::new();
    for path in [
        "/api/v1/instance",
        "/api/v1/auth/sso/providers",
        "/health",
        "/",
    ] {
        let anonymous = client.get(format!("{base}{path}")).send().await.unwrap();
        let recovered = client
            .get(format!("{base}{path}"))
            .header(header::COOKIE, format!("plombir_git_token={jwt}"))
            .send()
            .await
            .unwrap();
        assert_eq!(recovered.status(), anonymous.status(), "{path}");
        assert!(
            !recovered.headers().contains_key(header::WWW_AUTHENTICATE),
            "{path}"
        );
        assert!(recovered.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));
    }
    let protected = client
        .get(format!("{base}/api/v1/users/me"))
        .header(header::COOKIE, format!("plombir_git_token={jwt}"))
        .send()
        .await
        .unwrap();
    assert_eq!(protected.status(), StatusCode::UNAUTHORIZED);
    // Explicit stale bearer credentials still fail, even on public routes.
    let explicit = client
        .get(format!("{base}/"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(explicit.status(), StatusCode::UNAUTHORIZED);
    assert!(!explicit.headers().contains_key(header::WWW_AUTHENTICATE));
}

/// Reproduce the stale-tab logout: a gate refusal has not ended the newer
/// cookie session. Only the subsequent cookie-only logout revokes it.
#[tokio::test]
async fn logout_with_a_stale_bearer_must_complete_against_the_current_cookie() {
    let (base, db) = spawn_test_app_with_db().await;
    let (stale, user_id) = register_full(&base, "logout-recovery", "logout@example.test").await;
    rg_db::ops::user_ops::invalidate_sessions(&db, user_id)
        .await
        .unwrap();
    let user = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .unwrap()
        .unwrap();
    let fresh = rg_core::auth::jwt::generate_token(
        user.id,
        &user.username,
        user.session_version,
        "test-secret-key",
        7,
    )
    .unwrap();
    let cookie = format!("plombir_git_token={fresh}");
    let client = reqwest::Client::new();
    let refused = client
        .post(format!("{base}/api/v1/users/logout"))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, &base)
        .bearer_auth(&stale)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);
    assert!(!refused.headers().contains_key(header::SET_COOKIE));
    assert_eq!(
        refused.json::<serde_json::Value>().await.unwrap()["error"]["reason"],
        "session_revoked"
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/users/me"))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    let logged_out = client
        .post(format!("{base}/api/v1/users/logout"))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, &base)
        .send()
        .await
        .unwrap();
    assert_eq!(logged_out.status(), 200);
    assert!(logged_out.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    // Even replaying the old cookie after reload must no longer authenticate.
    let reload = client
        .get(format!("{base}/api/v1/users/me"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(reload.status(), 401);

    let both_stale = client
        .post(format!("{base}/api/v1/users/logout"))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, &base)
        .bearer_auth(&fresh)
        .send()
        .await
        .unwrap();
    assert_eq!(both_stale.status(), 401);
    assert!(both_stale.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
}
