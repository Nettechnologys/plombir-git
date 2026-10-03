//! End-to-end OIDC discovery, PKCE and callback-cookie regression coverage.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::common::{build_test_app_state_with, setup_test_db, StateOverrides};
use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rg_db::sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use sha2::{Digest, Sha256};

#[derive(Clone)]
struct MockOidcState {
    base_url: String,
    token_calls: Arc<AtomicUsize>,
    last_verifier: Arc<Mutex<Option<String>>>,
}

async fn discovery(State(state): State<MockOidcState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": state.base_url,
        "authorization_endpoint": format!("{}/authorize", state.base_url),
        "token_endpoint": format!("{}/token", state.base_url),
        "userinfo_endpoint": format!("{}/userinfo", state.base_url),
    }))
}

async fn token(
    State(state): State<MockOidcState>,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    *state.last_verifier.lock().unwrap() = form.get("code_verifier").cloned();
    if form.get("code").map(String::as_str) != Some("valid-code")
        || form.get("grant_type").map(String::as_str) != Some("authorization_code")
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_request"})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "access_token": "mock-access-token",
            "refresh_token": "mock-refresh-token",
            "expires_in": 3600
        })),
    )
}

async fn userinfo(headers: HeaderMap) -> (StatusCode, Json<serde_json::Value>) {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bearer mock-access-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid_token"})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "sub": "subject-1",
            "preferred_username": "oidc-user",
            "email": "oidc-user@example.com",
            "name": "OIDC User"
        })),
    )
}

/// Userinfo for an IdP that answers a *different person* on every call while
/// withholding the email — the shape behind `card_0a08de4d6707`. The third call
/// does return an address, and says out loud that it is unconfirmed.
async fn userinfo_without_a_usable_email(
    State(calls): State<Arc<AtomicUsize>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let nth = calls.fetch_add(1, Ordering::SeqCst) + 1;
    let mut claims = serde_json::json!({
        "sub": format!("subject-{nth}"),
        "preferred_username": format!("nomail-{nth}"),
        "name": "No Mail",
    });
    if nth == 3 {
        claims["email"] = serde_json::json!("claimed@example.com");
        claims["email_verified"] = serde_json::json!(false);
    }
    (StatusCode::OK, Json(claims))
}

fn cookie_pair(headers: &HeaderMap, name: &str) -> String {
    let prefix = format!("{name}=");
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing {name} cookie"))
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn signed_cookie_value(cookie: &str) -> String {
    cookie
        .split_once('=')
        .unwrap()
        .1
        .rsplit_once(':')
        .unwrap()
        .0
        .to_string()
}

async fn scalar(db: &DatabaseConnection, sql: String) -> i64 {
    db.query_one(Statement::from_string(db.get_database_backend(), sql))
        .await
        .expect("query scalar")
        .expect("scalar query returned one row")
        .try_get::<i64>("", "n")
        .expect("scalar column")
}

fn oidc_test_state(
    db: DatabaseConnection,
    repo_root: std::path::PathBuf,
    idp_origin: &str,
) -> rg_http::AppState {
    build_test_app_state_with(
        db,
        repo_root,
        StateOverrides {
            oidc_transport_policy: Some(
                rg_core::auth::sso::OidcTransportPolicy::parse(&[idp_origin.to_string()])
                    .expect("test IdP origin is exact"),
            ),
            ..Default::default()
        },
    )
}

/// An SSO profile that identifies nobody must not be allowed to identify
/// *somebody*.
///
/// Before the identity gate, a provider that withheld the email left
/// `email = ""` on the profile, and `""` reached `find_by_email` as an ordinary
/// key: the first such login provisioned an account holding it, and every later
/// one matched that row and signed into a stranger's account. Two different
/// subjects go through the real callback here, and neither may end up anywhere
/// near the other.
#[tokio::test]
async fn sso_logins_without_a_usable_email_are_refused_instead_of_merged() {
    let userinfo_calls = Arc::new(AtomicUsize::new(0));
    let oidc_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let oidc_addr = oidc_listener.local_addr().unwrap().to_string();
    let oidc_base = format!("http://{oidc_addr}");
    let oidc_state = MockOidcState {
        base_url: oidc_base.clone(),
        token_calls: Arc::new(AtomicUsize::new(0)),
        last_verifier: Arc::new(Mutex::new(None)),
    };
    let oidc_app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .with_state(oidc_state)
        .merge(
            Router::new()
                .route("/userinfo", get(userinfo_without_a_usable_email))
                .with_state(userinfo_calls.clone()),
        );
    let oidc_server = tokio::spawn(async move {
        axum::serve(oidc_listener, oidc_app).await.unwrap();
    });
    crate::common::wait_for_listener(&oidc_addr).await;

    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    rg_db::ops::sso_provider_ops::create(
        &db,
        rg_db::ops::sso_provider_ops::SsoProviderInput {
            name: "Mock OIDC",
            slug: "oidc-nomail",
            provider_type: "oidc",
            client_id: Some("client-id"),
            discovery_url: Some(&format!("{oidc_base}/.well-known/openid-configuration")),
            scopes: Some("openid profile"),
            enabled: true,
            auto_provision: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let app = rg_http::create_router_for_test(oidc_test_state(db.clone(), repo_root, &oidc_base));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let app_server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // One full authorize → callback round trip, exactly as a browser drives it.
    let sign_in = |client: reqwest::Client, base: String| async move {
        let authorize = client
            .get(format!("{base}/api/v1/auth/sso/oidc-nomail"))
            .send()
            .await
            .unwrap();
        let state_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_state");
        let verifier_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_code_verifier");
        let state = signed_cookie_value(&state_cookie);
        let callback = client
            .get(format!(
                "{base}/api/v1/auth/sso/oidc-nomail/callback?code=valid-code&state={state}"
            ))
            .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
            .send()
            .await
            .unwrap();
        (callback.status(), callback.text().await.unwrap())
    };

    for expected_subject in 1..=2 {
        let (status, body) = sign_in(client.clone(), base.clone()).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a login the provider would not name must not succeed"
        );
        assert!(
            body.contains("email"),
            "the refusal has to say which field is missing, got: {body}"
        );
        assert!(
            rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
                &db,
                "oidc-nomail",
                &format!("subject-{expected_subject}"),
            )
            .await
            .unwrap()
            .is_none(),
            "a refused login must leave no OAuth link behind"
        );
        assert!(
            rg_db::ops::user_ops::find_by_username(&db, &format!("nomail-{expected_subject}"))
                .await
                .unwrap()
                .is_none(),
            "a refused login must provision no account"
        );
    }

    // Nothing was provisioned under the empty address either — the row that
    // used to become everyone's account.
    assert!(rg_db::ops::user_ops::find_by_email(&db, "")
        .await
        .unwrap()
        .is_none());

    // Third call: the provider does hand over an address, and reports it as
    // unconfirmed. That address is the key an existing local account would be
    // merged on, so an unconfirmed one does not get to nominate the owner.
    let (status, body) = sign_in(client.clone(), base.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.contains("unverified"),
        "the refusal has to name the reason, got: {body}"
    );
    assert!(
        rg_db::ops::user_ops::find_by_email(&db, "claimed@example.com")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(userinfo_calls.load(Ordering::SeqCst), 3);

    app_server.abort();
    oidc_server.abort();
}

#[tokio::test]
async fn oidc_callback_uses_discovery_and_pkce_and_rejects_missing_verifier() {
    let token_calls = Arc::new(AtomicUsize::new(0));
    let last_verifier = Arc::new(Mutex::new(None));
    let oidc_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let oidc_addr = oidc_listener.local_addr().unwrap().to_string();
    let oidc_base = format!("http://{oidc_addr}");
    let oidc_state = MockOidcState {
        base_url: oidc_base.clone(),
        token_calls: token_calls.clone(),
        last_verifier: last_verifier.clone(),
    };
    let oidc_app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .route("/userinfo", get(userinfo))
        .with_state(oidc_state);
    let oidc_server = tokio::spawn(async move {
        axum::serve(oidc_listener, oidc_app).await.unwrap();
    });
    crate::common::wait_for_listener(&oidc_addr).await;

    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    rg_db::ops::sso_provider_ops::create(
        &db,
        rg_db::ops::sso_provider_ops::SsoProviderInput {
            name: "Mock OIDC",
            slug: "oidc-test",
            provider_type: "oidc",
            client_id: Some("client-id"),
            discovery_url: Some(&format!("{oidc_base}/.well-known/openid-configuration")),
            scopes: Some("openid profile email"),
            enabled: true,
            auto_provision: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let app = rg_http::create_router_for_test(oidc_test_state(db.clone(), repo_root, &oidc_base));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let app_server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let authorize = client
        .get(format!("{base}/api/v1/auth/sso/oidc-test"))
        .send()
        .await
        .unwrap();
    assert!(authorize.status().is_redirection());
    let state_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_state");
    let verifier_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_code_verifier");
    let state = signed_cookie_value(&state_cookie);
    let verifier = signed_cookie_value(&verifier_cookie);
    assert!((43..=128).contains(&verifier.len()));

    let location = authorize
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    let auth_url = reqwest::Url::parse(location).unwrap();
    assert_eq!(auth_url.path(), "/authorize");
    let query = auth_url.query_pairs().collect::<HashMap<_, _>>();
    assert_eq!(query.get("state").map(|v| v.as_ref()), Some(state.as_str()));
    assert_eq!(
        query.get("code_challenge_method").map(|v| v.as_ref()),
        Some("S256")
    );
    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(
        query.get("code_challenge").map(|v| v.as_ref()),
        Some(expected_challenge.as_str())
    );

    let callback = client
        .get(format!(
            "{base}/api/v1/auth/sso/oidc-test/callback?code=valid-code&state={state}"
        ))
        .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
        .send()
        .await
        .unwrap();
    assert!(callback.status().is_redirection());
    assert_eq!(callback.headers()[header::LOCATION], "/dashboard");
    let set_cookies = callback
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(set_cookies
        .iter()
        .any(|cookie| cookie.starts_with("plombir_git_token=")));
    assert!(set_cookies.iter().any(
        |cookie| cookie.starts_with("plombir_git_sso_state=;") && cookie.contains("Max-Age=0")
    ));
    assert!(set_cookies.iter().any(|cookie| {
        cookie.starts_with("plombir_git_sso_code_verifier=;") && cookie.contains("Max-Age=0")
    }));
    assert_eq!(token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        last_verifier.lock().unwrap().as_deref(),
        Some(verifier.as_str())
    );

    let user = rg_db::ops::user_ops::find_by_username(&db, "oidc-user")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.email, "oidc-user@example.com");
    assert!(
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "oidc-test", "subject-1")
            .await
            .unwrap()
            .is_some()
    );

    let authorize_without_verifier = client
        .get(format!("{base}/api/v1/auth/sso/oidc-test"))
        .send()
        .await
        .unwrap();
    let state_cookie = cookie_pair(
        authorize_without_verifier.headers(),
        "plombir_git_sso_state",
    );
    let state = signed_cookie_value(&state_cookie);
    let missing_verifier = client
        .get(format!(
            "{base}/api/v1/auth/sso/oidc-test/callback?code=valid-code&state={state}"
        ))
        .header(header::COOKIE, state_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_verifier.status(), StatusCode::FORBIDDEN);
    assert_eq!(token_calls.load(Ordering::SeqCst), 1);

    let authorize_mismatch = client
        .get(format!("{base}/api/v1/auth/sso/oidc-test"))
        .send()
        .await
        .unwrap();
    let state_cookie = cookie_pair(authorize_mismatch.headers(), "plombir_git_sso_state");
    let verifier_cookie = cookie_pair(
        authorize_mismatch.headers(),
        "plombir_git_sso_code_verifier",
    );
    let mismatch = client
        .get(format!(
            "{base}/api/v1/auth/sso/oidc-test/callback?code=valid-code&state=wrong-state"
        ))
        .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(mismatch.status(), StatusCode::FORBIDDEN);
    assert_eq!(token_calls.load(Ordering::SeqCst), 1);

    // The provider and identity are healthy, but account retirement wins inside
    // the lifecycle finalizer after both have been proved. The callback must
    // stop before its success login-log row and auth cookie.
    let authorize_lifecycle = client
        .get(format!("{base}/api/v1/auth/sso/oidc-test"))
        .send()
        .await
        .unwrap();
    let state_cookie = cookie_pair(authorize_lifecycle.headers(), "plombir_git_sso_state");
    let verifier_cookie = cookie_pair(
        authorize_lifecycle.headers(),
        "plombir_git_sso_code_verifier",
    );
    let state = signed_cookie_value(&state_cookie);
    let successful_logins_before = scalar(
        &db,
        "SELECT COUNT(*) AS n FROM login_logs WHERE success = 1".to_string(),
    )
    .await;
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER retire_user_inside_sso_finalizer \
             BEFORE UPDATE OF last_login_at ON users WHEN OLD.id = {} \
             BEGIN \
                 UPDATE users SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
                 WHERE id = OLD.id; \
                 SELECT RAISE(IGNORE); \
             END",
            user.id
        ),
    ))
    .await
    .expect("install the competing account retirement");
    let lifecycle_loss = client
        .get(format!(
            "{base}/api/v1/auth/sso/oidc-test/callback?code=valid-code&state={state}"
        ))
        .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        lifecycle_loss.status(),
        StatusCode::UNAUTHORIZED,
        "SSO callback accepted an account whose retirement won after identity proof"
    );
    assert!(
        lifecycle_loss
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .all(|cookie| !cookie.starts_with("plombir_git_token=")
                || cookie.starts_with("plombir_git_token=;")),
        "the losing SSO callback issued an auth cookie"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM login_logs WHERE success = 1".to_string(),
        )
        .await,
        successful_logins_before,
        "the losing SSO callback published a success login-log row"
    );
    assert_eq!(token_calls.load(Ordering::SeqCst), 2);

    app_server.abort();
    oidc_server.abort();
}

/// Linking and unlinking an external identity are journal events
/// (card_79c61ed60181).
///
/// A linked provider is a way into the account that needs no password of ours,
/// so it appears in the journal for the same reason a passkey does; dropping
/// somebody's link is a step of a takeover, not housekeeping. Neither wrote a
/// row, and `api::sso` held no journal call at all. The row it writes also
/// carries the provider's `access_token` and `refresh_token` encrypted —
/// somebody else's credentials, kept by this server — which is why the leak
/// assertion at the bottom belongs to the same test.
#[tokio::test]
async fn linking_and_unlinking_an_external_identity_are_journalled_without_its_tokens() {
    let token_calls = Arc::new(AtomicUsize::new(0));
    let last_verifier = Arc::new(Mutex::new(None));
    let oidc_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let oidc_addr = oidc_listener.local_addr().unwrap().to_string();
    let oidc_base = format!("http://{oidc_addr}");
    let oidc_state = MockOidcState {
        base_url: oidc_base.clone(),
        token_calls: token_calls.clone(),
        last_verifier,
    };
    let oidc_app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .route("/userinfo", get(userinfo))
        .with_state(oidc_state);
    let oidc_server = tokio::spawn(async move {
        axum::serve(oidc_listener, oidc_app).await.unwrap();
    });
    crate::common::wait_for_listener(&oidc_addr).await;

    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    rg_db::ops::sso_provider_ops::create(
        &db,
        rg_db::ops::sso_provider_ops::SsoProviderInput {
            name: "Mock OIDC",
            slug: "oidc-journal",
            provider_type: "oidc",
            client_id: Some("client-id"),
            discovery_url: Some(&format!("{oidc_base}/.well-known/openid-configuration")),
            scopes: Some("openid profile email"),
            enabled: true,
            auto_provision: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let app = rg_http::create_router_for_test(oidc_test_state(db.clone(), repo_root, &oidc_base));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let app_server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // One full sign-in through the provider, returning this instance's session
    // token for the account behind it.
    let sign_in = || async {
        let authorize = client
            .get(format!("{base}/api/v1/auth/sso/oidc-journal"))
            .send()
            .await
            .unwrap();
        assert!(authorize.status().is_redirection());
        let state_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_state");
        let verifier_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_code_verifier");
        let state = signed_cookie_value(&state_cookie);

        let callback = client
            .get(format!(
                "{base}/api/v1/auth/sso/oidc-journal/callback?code=valid-code&state={state}"
            ))
            .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
            .send()
            .await
            .unwrap();
        assert!(
            callback.status().is_redirection(),
            "the SSO callback failed: {}",
            callback.status()
        );
        let session = cookie_pair(callback.headers(), "plombir_git_token");
        session.split_once('=').unwrap().1.to_string()
    };

    let session = sign_in().await;

    // A second sign-in through the same link. It must not read as a second
    // link: a row per login would bury the one event an incident review is
    // looking for.
    let _ = sign_in().await;

    let user = rg_db::ops::user_ops::find_by_username(&db, "oidc-user")
        .await
        .unwrap()
        .unwrap();
    rg_db::ops::user_ops::update_by_id(&db, user.id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("the provisioned account exists");

    // Read while the link is alive: after the unlink below there is nothing
    // left to look at.
    let link =
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "oidc-journal", "subject-1")
            .await
            .unwrap()
            .expect("the sign-in linked the identity");
    assert_eq!(link.provider_user_id, "subject-1");

    // The link is an identity, not a credential store. This sign-in handed the
    // instance a live access token and a refresh token for somebody's account
    // at the provider; the row it wrote must have nowhere to keep them
    // (card_51dd82b6dc82). Asserted against the schema the real migrations
    // produced, not against a hand-written fixture, so a migration that brings
    // the columns back is caught here even if nothing writes them yet.
    let credential_columns = db
        .query_one(rg_db::sea_orm::Statement::from_string(
            rg_db::sea_orm::DatabaseBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM pragma_table_info('oauth_accounts') \
             WHERE name IN ('access_token', 'refresh_token', 'token_expires_at')"
                .to_string(),
        ))
        .await
        .expect("read the oauth_accounts schema")
        .expect("pragma_table_info always answers");
    assert_eq!(
        credential_columns.try_get::<i64>("", "n").unwrap(),
        0,
        "`oauth_accounts` has a column to hold the provider's credentials again; a database dump \
         plus the instance key would hand over live access to these external accounts, and no \
         feature on this instance reads them"
    );

    let unlink = client
        .delete(format!("{base}/api/v1/auth/sso/oidc-journal/unlink"))
        .bearer_auth(&session)
        .send()
        .await
        .unwrap();
    assert_eq!(
        unlink.status(),
        StatusCode::OK,
        "unlinking failed: {}",
        unlink.text().await.unwrap_or_default()
    );

    let journal = client
        .get(format!("{base}/api/v1/admin/audit/logs"))
        .query(&[("per_page", "100")])
        .bearer_auth(&session)
        .send()
        .await
        .unwrap();
    let status = journal.status();
    let body = journal.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "reading the journal failed: {body}");
    let rows = serde_json::from_str::<serde_json::Value>(&body).expect("the journal is JSON")
        ["logs"]
        .as_array()
        .expect("`logs` array")
        .clone();

    let entries = |action: &str| -> Vec<serde_json::Value> {
        rows.iter()
            .filter(|row| row["action"] == action)
            .cloned()
            .collect()
    };
    let details = |row: &serde_json::Value| -> serde_json::Value {
        serde_json::from_str(
            row["details"]
                .as_str()
                .unwrap_or_else(|| panic!("no details on {row}")),
        )
        .expect("details are JSON")
    };

    let links = entries("user.link_oauth_account");
    assert_eq!(
        links.len(),
        1,
        "two sign-ins through one link are one link event, not two: {:?}",
        rows.iter()
            .map(|row| row["action"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        links[0]["username"], "oidc-user",
        "the actor column names the account the identity was attached to: {}",
        links[0]
    );
    let linked = details(&links[0]);
    assert_eq!(linked["provider"], "oidc-journal");
    assert_eq!(linked["provider_username"], "oidc-user");
    assert_eq!(
        linked["provider_user_id"], "subject-1",
        "the entry has to name the identity on the far side — after the link is \
         gone it is the only thing that identifies it"
    );

    let unlinks = entries("user.unlink_oauth_account");
    assert_eq!(
        unlinks.len(),
        1,
        "the unlink must be journalled exactly once"
    );
    assert_eq!(unlinks[0]["username"], "oidc-user");
    let dropped = details(&unlinks[0]);
    assert_eq!(dropped["provider"], "oidc-journal");
    assert_eq!(
        dropped["provider_user_id"], "subject-1",
        "read off the row before it went"
    );

    // Over the whole journal: a row carrying the provider's access token leaks
    // it to every operator who can read the admin API.
    let whole = serde_json::to_string(&rows).expect("the journal serializes");
    for (what, secret) in [
        ("the provider's access token", "mock-access-token"),
        ("the provider's refresh token", "mock-refresh-token"),
    ] {
        assert!(
            !whole.contains(secret),
            "{what} reached `audit_log`; the journal is read by operators and served over the \
             admin API, so a credential in it is a second credential store"
        );
    }

    // Recreate the link without going through the HTTP journal so the next
    // callback takes the existing-identity branch. The trigger then commits an
    // unlink inside that branch's real UPDATE: after its lookup, before the
    // write can land. The old ActiveModel path surfaced RecordNotUpdated as
    // 500; retrying through the create branch would be worse because it would
    // undo the newer explicit unlink.
    let relinked = rg_db::ops::oauth_account_ops::link(
        &db,
        user.id,
        "oidc-journal",
        "subject-1",
        "oidc-user",
        "oidc-user@example.com",
    )
    .await
    .expect("recreate the link targeted by the callback race")
    .expect("the recreated link remains present");
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER unlink_oauth_account_before_callback_touch \
             BEFORE UPDATE OF updated_at ON oauth_accounts WHEN OLD.id = {} \
             BEGIN DELETE FROM oauth_accounts WHERE id = OLD.id; END",
            relinked.id
        ),
    ))
    .await
    .expect("install the competing OAuth unlink");

    let link_audits_before = scalar(
        &db,
        format!(
            "SELECT COUNT(*) AS n FROM audit_log \
             WHERE user_id = {} AND action = 'user.link_oauth_account'",
            user.id
        ),
    )
    .await;
    let successful_logins_before = scalar(
        &db,
        format!(
            "SELECT COUNT(*) AS n FROM login_logs \
             WHERE user_id = {} AND auth_provider = 'oidc-journal' AND success = 1",
            user.id
        ),
    )
    .await;

    let authorize = client
        .get(format!("{base}/api/v1/auth/sso/oidc-journal"))
        .send()
        .await
        .unwrap();
    let state_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_state");
    let verifier_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_code_verifier");
    let state = signed_cookie_value(&state_cookie);
    let raced_callback = client
        .get(format!(
            "{base}/api/v1/auth/sso/oidc-journal/callback?code=valid-code&state={state}"
        ))
        .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
        .send()
        .await
        .unwrap();
    let status = raced_callback.status();
    let body = raced_callback.text().await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body.contains("identity link changed; restart SSO"),
        "the conflict must tell the person how to recover, got: {body}"
    );
    assert!(
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "oidc-journal", "subject-1",)
            .await
            .unwrap()
            .is_none(),
        "the losing callback must not recreate a link removed by the newer unlink"
    );
    assert_eq!(
        scalar(
            &db,
            format!(
                "SELECT COUNT(*) AS n FROM audit_log \
                 WHERE user_id = {} AND action = 'user.link_oauth_account'",
                user.id
            ),
        )
        .await,
        link_audits_before,
        "a callback that lost its identity link must not journal a link"
    );
    assert_eq!(
        scalar(
            &db,
            format!(
                "SELECT COUNT(*) AS n FROM login_logs \
                 WHERE user_id = {} AND auth_provider = 'oidc-journal' AND success = 1",
                user.id
            ),
        )
        .await,
        successful_logins_before,
        "a callback that returns 409 must not journal a successful login"
    );

    assert_eq!(token_calls.load(Ordering::SeqCst), 3);

    app_server.abort();
    oidc_server.abort();
}
