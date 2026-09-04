//! Regression coverage for card_0cf83ac01b31: `PATCH /admin/sso/providers/{id}`
//! could change a provider's `slug` while `oauth_accounts.provider` — which
//! stores exactly that slug and has no foreign key behind it — kept the old
//! value.
//!
//! Nothing failed when that happened, which is the whole problem. The next
//! sign-in through the provider looked the link up under the new slug, found
//! nothing, and fell through to the merge-by-email branch or to provisioning:
//! an administrator editing a *name* decided which account a person lands in,
//! and a second link appeared beside the first.
//!
//! The second half is the guard next door. "A provider that identities still
//! point at may not be deleted" (card_30187038af39) counts `oauth_accounts` by
//! the provider's current slug, so a rename that left the rows behind also
//! blinded the guard: it counted zero and let the provider go, stranding the
//! rows for good.
//!
//! The last test holds the other side of the line. Rows already sitting on the
//! target slug are somebody else's identity space — `(provider,
//! provider_user_id)` is UNIQUE and would either refuse the merge or, for a
//! different subject, quietly put two people under one provider name — so that
//! rename is refused whole rather than "repaired".

use crate::common::{register_full, spawn_test_app_with_db};

/// Register an account and make it an instance admin.
async fn admin(base: &str, db: &sea_orm::DatabaseConnection, name: &str) -> (String, i64) {
    let (token, id) = register_full(base, name, &format!("{name}@example.test")).await;
    rg_db::ops::user_ops::update_by_id(db, id, None, None, Some(true), None)
        .await
        .expect("promote the account")
        .expect("registered user must exist");
    (token, id)
}

fn provider_body(name: &str, slug: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "slug": slug,
        "provider_type": "oidc",
        "enabled": true,
        "client_id": "client-id",
        "discovery_url": "https://idp.example.test/.well-known/openid-configuration",
    })
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

async fn patch_provider(
    base: &str,
    token: &str,
    id: i64,
    body: serde_json::Value,
) -> (u16, String) {
    let response = reqwest::Client::new()
        .patch(format!("{base}/api/v1/admin/sso/providers/{id}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("patch request");
    let status = response.status().as_u16();
    (status, response.text().await.unwrap_or_default())
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

/// The link must be the *same row*, found under the new slug. A second link
/// beside the first is the failure this test exists for: it is what a
/// merge-by-email or an auto-provision leaves behind.
#[tokio::test]
async fn renaming_a_provider_moves_the_identities_that_named_its_slug() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = admin(&base, &db, "sso-rename-admin").await;

    let provider_id = create_provider(&base, &token, provider_body("Corporate", "corp-idp")).await;
    let link = rg_db::ops::oauth_account_ops::link(
        &db,
        admin_id,
        "corp-idp",
        "external-user-1",
        "sso-rename-admin",
        "sso-rename-admin@example.test",
    )
    .await
    .expect("link the identity")
    .expect("the link must be present");

    let (status, body) = patch_provider(
        &base,
        &token,
        provider_id,
        provider_body("Corporate", "corp-sso"),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let moved = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
        &db,
        "corp-sso",
        "external-user-1",
    )
    .await
    .expect("look the identity up under the new slug")
    .expect(
        "the next sign-in resolves the link by the provider's current slug, so it must be there",
    );
    assert_eq!(
        moved.id, link.id,
        "the identity must be the same row carried over, not a second link"
    );
    assert_eq!(
        moved.user_id, admin_id,
        "the link must still open the account it was written for"
    );
    assert!(
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "corp-idp", "external-user-1")
            .await
            .expect("look the identity up under the old slug")
            .is_none(),
        "nothing may be left on the slug the provider no longer answers to"
    );
    assert_eq!(
        rg_db::ops::oauth_account_ops::find_by_user_id(&db, admin_id)
            .await
            .expect("list this account's links")
            .len(),
        1,
        "a rename must not leave the account holding two links to one provider"
    );

    // The second half: the delete guard counts `oauth_accounts` by the
    // provider's current slug. A rename that stranded the rows would make it
    // count zero and let the provider go.
    let (status, body) = delete_provider(&base, &token, provider_id).await;
    assert_eq!(
        status, 409,
        "the renamed provider is still held by an identity: {body}"
    );
    assert!(
        body.contains("linked identities"),
        "the conflict must name what holds the provider, got: {body}"
    );
}

/// The rename is refused whole when the target slug still names identities the
/// provider did not write. The refusal is a `409` for the same reason the
/// delete guard's is: the request is correct and unchangeable, and it is the
/// rows that exist right now that stand in its way.
#[tokio::test]
async fn renaming_onto_a_slug_that_still_names_identities_is_a_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = admin(&base, &db, "sso-rename-clash").await;
    let (_, stranded_id) = register_full(&base, "stranded", "stranded@example.test").await;

    let provider_id = create_provider(&base, &token, provider_body("Corporate", "corp-idp")).await;
    rg_db::ops::oauth_account_ops::link(
        &db,
        admin_id,
        "corp-idp",
        "external-user-1",
        "sso-rename-clash",
        "sso-rename-clash@example.test",
    )
    .await
    .expect("link the identity that would move")
    .expect("the link must be present");
    // Left behind on `corp-sso` by an earlier rename, before the carry existed.
    rg_db::ops::oauth_account_ops::link(
        &db,
        stranded_id,
        "corp-sso",
        "external-user-2",
        "stranded",
        "stranded@example.test",
    )
    .await
    .expect("strand an identity on the target slug")
    .expect("the link must be present");

    let (status, body) = patch_provider(
        &base,
        &token,
        provider_id,
        provider_body("Corporate", "corp-sso"),
    )
    .await;
    assert_eq!(
        status, 409,
        "identities already on the target slug are state, not a malformed request: {body}"
    );
    assert!(
        body.contains("corp-sso"),
        "the conflict must name the slug that is held, got: {body}"
    );

    assert_eq!(
        rg_db::ops::sso_provider_ops::find_by_id(&db, provider_id)
            .await
            .expect("read the provider back")
            .expect("a refused rename leaves the provider in place")
            .slug,
        "corp-idp",
        "a refused rename must not write the new slug"
    );
    assert!(
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "corp-idp", "external-user-1")
            .await
            .expect("look the mover up")
            .is_some(),
        "the link that would have moved must still be on the old slug"
    );
    assert_eq!(
        rg_db::ops::oauth_account_ops::count_by_provider(&db, "corp-sso")
            .await
            .expect("count the target slug"),
        1,
        "the stranded identity must still be the only row on the target slug"
    );
}
