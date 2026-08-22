//! card_1fb65c54c73e: switching a provider off must close *every* door.
//!
//! `enabled` used to be a convention each handler re-read by hand.
//! `authorize` and `callback` did; `refresh_token` never did — so an operator
//! could disable a provider and every already-linked account kept renewing its
//! OAuth tokens through it, indefinitely and silently.
//!
//! The refusal is paired with the identical call against the same provider
//! while it is still enabled, so a green test proves the flag and not a broken
//! fixture: the only difference between the two requests is one boolean in one
//! row.

use std::collections::HashMap;

use axum::extract::Form;
use axum::routing::{get, post};
use axum::{Json, Router};
use rg_db::ops::sso_provider_ops::SsoProviderInput;

use crate::common::{register_full, spawn_test_app_with_db, wait_for_listener};

/// The provider's token endpoint, which a refresh grant must reach to succeed.
/// If a disabled provider ever gets this far, the test's 403 assertion is the
/// only thing standing between the operator's decision and a live token.
async fn token(Form(_form): Form<HashMap<String, String>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "access_token": "refreshed-access-token",
        "refresh_token": "refreshed-refresh-token",
        "expires_in": 3600
    }))
}

struct Harness {
    db: sea_orm::DatabaseConnection,
    base: String,
    client: reqwest::Client,
    token: String,
    provider_id: i64,
    idp_server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start() -> Harness {
        // ── A mock OIDC provider that will happily refresh anything ──
        let idp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp_addr = idp_listener.local_addr().unwrap().to_string();
        let idp_base = format!("http://{idp_addr}");
        let discovery_base = idp_base.clone();
        let idp_app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let base = discovery_base.clone();
                    async move {
                        Json(serde_json::json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/authorize"),
                            "token_endpoint": format!("{base}/token"),
                            "userinfo_endpoint": format!("{base}/userinfo"),
                        }))
                    }
                }),
            )
            .route("/token", post(token));
        let idp_server = tokio::spawn(async move {
            axum::serve(idp_listener, idp_app).await.unwrap();
        });
        wait_for_listener(&idp_addr).await;

        let (base, db) = spawn_test_app_with_db().await;
        let provider = rg_db::ops::sso_provider_ops::upsert(
            &db,
            None,
            SsoProviderInput {
                name: "Mock IdP",
                slug: "idp",
                provider_type: "oidc",
                client_id: Some("client-id"),
                discovery_url: Some(&format!("{idp_base}/.well-known/openid-configuration")),
                scopes: Some("openid profile email"),
                enabled: true,
                ..Default::default()
            },
        )
        .await
        .expect("seed SSO provider");

        let (token, user_id) =
            register_full(&base, "sso-refresher", "sso-refresher@example.test").await;

        // The link a completed SSO login would have left behind. Its stored
        // tokens are never read here — the refresh grant is supplied in the
        // request body — but `store_refreshed_oauth_tokens` needs the row.
        rg_db::ops::oauth_account_ops::upsert(
            &db,
            user_id,
            "idp",
            "subject-1",
            "sso-refresher",
            "sso-refresher@example.test",
            None,
            None,
            None,
        )
        .await
        .expect("seed OAuth account link");

        Harness {
            db,
            base,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            token,
            provider_id: provider.id,
            idp_server,
        }
    }

    /// Flip the one boolean this whole test is about, leaving every other
    /// column exactly as it was.
    async fn set_enabled(&self, enabled: bool) {
        let current = rg_db::ops::sso_provider_ops::find_by_id(&self.db, self.provider_id)
            .await
            .expect("read provider")
            .expect("provider must exist");
        rg_db::ops::sso_provider_ops::upsert(
            &self.db,
            Some(self.provider_id),
            SsoProviderInput {
                name: &current.name,
                slug: &current.slug,
                provider_type: &current.provider_type,
                client_id: current.client_id.as_deref(),
                discovery_url: current.discovery_url.as_deref(),
                scopes: current.scopes.as_deref(),
                enabled,
                ..Default::default()
            },
        )
        .await
        .expect("toggle provider");
    }

    async fn refresh(&self) -> reqwest::Response {
        self.client
            .post(format!("{}/api/v1/auth/sso/idp/refresh", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"refresh_token": "stored-refresh-token"}))
            .send()
            .await
            .expect("refresh request")
    }

    async fn links(&self) -> Vec<serde_json::Value> {
        let response = self
            .client
            .get(format!("{}/api/v1/users/me/sso", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("linked identities request");
        assert_eq!(
            response.status(),
            200,
            "listing this account's linked identities must not depend on the provider's flag"
        );
        response.json().await.expect("linked identities body")
    }

    async fn authorize(&self) -> reqwest::Response {
        self.client
            .get(format!("{}/api/v1/auth/sso/idp", self.base))
            .send()
            .await
            .expect("authorize request")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.idp_server.abort();
    }
}

/// The defect: `POST /auth/sso/{slug}/refresh` against a provider the operator
/// switched off used to mint a fresh access token anyway.
#[tokio::test]
async fn a_disabled_provider_refuses_to_refresh_a_token() {
    let app = Harness::start().await;

    // Baseline first, on the same row, so the refusal below cannot be a
    // misconfigured fixture.
    let enabled = app.refresh().await;
    assert_eq!(
        enabled.status(),
        200,
        "an enabled provider must still refresh: {}",
        enabled.text().await.unwrap_or_default()
    );

    app.set_enabled(false).await;

    let disabled = app.refresh().await;
    assert_eq!(
        disabled.status(),
        403,
        "a disabled provider must refuse to refresh, not hand out a live token"
    );
    let body = disabled.text().await.unwrap_or_default();
    assert!(
        body.contains("SSO provider is disabled"),
        "the refusal has to name the reason, got: {body}"
    );
}

/// The other two doors answer the same way, from the same resolver — the point
/// of the fix is that all three now share one answer rather than three copies
/// of it.
#[tokio::test]
async fn a_disabled_provider_refuses_to_start_a_login() {
    let app = Harness::start().await;

    let enabled = app.authorize().await;
    assert!(
        enabled.status().is_redirection(),
        "an enabled provider must still start a login, got {}",
        enabled.status()
    );

    app.set_enabled(false).await;

    assert_eq!(
        app.authorize().await.status(),
        403,
        "a disabled provider must refuse to start a login"
    );
}

/// The deliberate exception: dropping a link must keep working after the
/// operator switches the provider off, or a user is stuck with a binding to a
/// provider that no longer exists for them.
#[tokio::test]
async fn unlinking_still_works_while_the_provider_is_disabled() {
    let app = Harness::start().await;
    app.set_enabled(false).await;

    let response = app
        .client
        .delete(format!("{}/api/v1/auth/sso/idp/unlink", app.base))
        .bearer_auth(&app.token)
        .send()
        .await
        .expect("unlink request");

    assert_eq!(
        response.status(),
        200,
        "unlinking is the one door a disabled provider must not close"
    );
    assert!(
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&app.db, "idp", "subject-1")
            .await
            .expect("read link")
            .is_none(),
        "the OAuth link survived an unlink that reported success"
    );
}

/// card_2cd2d40f27d2: the unlink above was reachable only with `curl`, because
/// nothing listed the links — and a link to a provider the operator has since
/// switched off is exactly the one whose owner most needs to find it. Listing
/// it is therefore held to the same exception the unlink is: the flag changes
/// what the entry *says*, never whether it is shown.
#[tokio::test]
async fn a_disabled_providers_link_is_still_listed_until_it_is_unlinked() {
    let app = Harness::start().await;

    let enabled = app.links().await;
    assert_eq!(
        enabled.len(),
        1,
        "the seeded link must be listed: {enabled:?}"
    );
    assert_eq!(enabled[0]["slug"], "idp");
    assert_eq!(
        enabled[0]["provider_enabled"], true,
        "an enabled provider must be reported as enabled: {enabled:?}"
    );

    app.set_enabled(false).await;

    let disabled = app.links().await;
    assert_eq!(
        disabled.len(),
        1,
        "switching the provider off must not hide the link its owner has to drop: {disabled:?}"
    );
    assert_eq!(
        disabled[0]["provider_enabled"], false,
        "the entry has to say the provider is off, or the page cannot explain it: {disabled:?}"
    );

    let unlinked = app
        .client
        .delete(format!("{}/api/v1/auth/sso/idp/unlink", app.base))
        .bearer_auth(&app.token)
        .send()
        .await
        .expect("unlink request");
    assert_eq!(unlinked.status(), 200);

    assert!(
        app.links().await.is_empty(),
        "the listing still names a link the unlink reported dropping"
    );
}
