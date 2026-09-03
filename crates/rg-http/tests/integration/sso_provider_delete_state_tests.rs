//! Regression coverage for card_30187038af39: deleting an SSO provider that
//! identities still point at answered `400` instead of `409`.
//!
//! The request is correct in every part the caller controls — the id is
//! well-formed, the body is empty, the rights are there — and it is refused by
//! rows that exist right now. Unlink the accounts and the *identical* `DELETE`
//! succeeds, which is the definition of a `409`. A `400` tells an admin console
//! the request is unfixable and sends the operator looking for a mistake in a
//! request that has none.
//!
//! Both counting branches of the guard are covered, because they read different
//! tables: an OAuth/OIDC provider is found through `oauth_accounts.provider`
//! (the slug), an LDAP one through `users.ldap_provider_id` (the row id). A fix
//! applied to only one branch passes half of this file.
//!
//! The last test holds the other side of the line. The neighbouring refusal in
//! the same module — connection-testing a provider that is not LDAP — is about
//! the *request* (this route does not apply to this kind of provider), and
//! stays a `400`. A fix that swept the whole SSO admin surface into `Conflict`
//! would pass the first two tests and fail this one.

use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

/// Register an account and make it an instance admin.
async fn admin(base: &str, db: &sea_orm::DatabaseConnection, name: &str) -> (String, i64) {
    let (token, id) = register_full(base, name, &format!("{name}@example.test")).await;
    rg_db::ops::user_ops::update_by_id(db, id, None, None, Some(true), None)
        .await
        .expect("promote the account")
        .expect("registered user must exist");
    (token, id)
}

async fn create_provider(base: &str, token: &str, body: serde_json::Value) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/admin/sso/providers"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("create request");
    let status = response.status();
    let created: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(
        status, 201,
        "the fixture provider must be created: {created}"
    );
    created["id"].as_i64().expect("the new provider's id")
}

async fn delete_provider(base: &str, token: &str, id: i64) -> (u16, String) {
    let response = reqwest::Client::new()
        .delete(format!("{base}/api/v1/admin/sso/providers/{id}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("delete request");
    let status = response.status().as_u16();
    (status, response.text().await.unwrap_or_default())
}

/// The OAuth/OIDC branch of the guard: `oauth_accounts` still name the slug.
#[tokio::test]
async fn deleting_a_provider_with_a_linked_oauth_identity_is_a_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = admin(&base, &db, "sso-del-oauth").await;

    let provider_id = create_provider(
        &base,
        &token,
        serde_json::json!({
            "name": "Corporate OIDC",
            "slug": "corporate-oidc",
            "provider_type": "oidc",
            "enabled": true,
            "client_id": "client-id",
            "discovery_url": "https://idp.example.test/.well-known/openid-configuration",
        }),
    )
    .await;

    let link = rg_db::ops::oauth_account_ops::link(
        &db,
        admin_id,
        "corporate-oidc",
        "external-user-1",
        "sso-del-oauth",
        "sso-del-oauth@example.test",
    )
    .await
    .expect("link the identity")
    .expect("the link must be present");

    let (status, body) = delete_provider(&base, &token, provider_id).await;
    assert_eq!(
        status, 409,
        "a provider held by existing identities is state, not a malformed request: {body}"
    );
    assert!(
        body.contains("linked identities"),
        "the conflict must name what holds the provider, got: {body}"
    );
    assert!(
        rg_db::ops::sso_provider_ops::find_by_id(&db, provider_id)
            .await
            .expect("read the provider back")
            .is_some(),
        "a refused delete must leave the provider in place"
    );

    // The same request, once nothing links to the provider any more. This is
    // the half that makes the code a `409` rather than a `400`: the client did
    // not have to change anything about it.
    assert!(
        rg_db::ops::oauth_account_ops::delete_by_id(&db, link.id, admin_id)
            .await
            .expect("unlink the identity"),
        "the link this test created must be the row that got deleted"
    );
    let (status, body) = delete_provider(&base, &token, provider_id).await;
    assert_eq!(status, 200, "{body}");
}

/// The LDAP branch of the guard: accounts carry the provider's row id, not its
/// slug, so this half is counted through a different table entirely.
#[tokio::test]
async fn deleting_a_provider_with_a_linked_ldap_account_is_a_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = admin(&base, &db, "sso-del-ldap").await;
    let (_, member_id) = register_full(&base, "ldap-member", "ldap-member@example.test").await;

    let provider_id = create_provider(
        &base,
        &token,
        serde_json::json!({
            // Disabled, so the fixture does not have to carry a complete and
            // reachable directory configuration — the delete guard counts rows
            // and never looks at the flag.
            "name": "Directory",
            "slug": "directory",
            "provider_type": "ldap",
            "enabled": false,
        }),
    )
    .await;

    let member = rg_db::entities::user::Entity::find_by_id(member_id)
        .one(&db)
        .await
        .expect("read the member")
        .expect("the registered member must exist");
    let mut member: rg_db::entities::user::ActiveModel = member.into();
    member.ldap_provider_id = Set(Some(provider_id));
    member.update(&db).await.expect("bind the member to LDAP");
    assert_eq!(
        rg_db::ops::user_ops::count_by_ldap_provider(&db, provider_id)
            .await
            .expect("count the directory accounts"),
        1
    );

    let (status, body) = delete_provider(&base, &token, provider_id).await;
    assert_eq!(
        status, 409,
        "a directory that still owns accounts is state, not a malformed request: {body}"
    );
    assert!(
        body.contains("linked identities"),
        "the conflict must name what holds the provider, got: {body}"
    );
}

/// The line this fix must not cross: connection-testing a provider that is not
/// LDAP is a statement about the *request*, and keeps its `400`.
#[tokio::test]
async fn testing_a_non_ldap_provider_is_still_a_bad_request() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = admin(&base, &db, "sso-test-line").await;

    let provider_id = create_provider(
        &base,
        &token,
        serde_json::json!({
            "name": "Corporate OIDC",
            "slug": "corporate-oidc",
            "provider_type": "oidc",
            "enabled": true,
            "client_id": "client-id",
            "discovery_url": "https://idp.example.test/.well-known/openid-configuration",
        }),
    )
    .await;

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/admin/sso/providers/{provider_id}/test"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("test request");
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    assert_eq!(
        status, 400,
        "this route does not apply to a non-LDAP provider — that is the request, not the state: \
         {body}"
    );
}
