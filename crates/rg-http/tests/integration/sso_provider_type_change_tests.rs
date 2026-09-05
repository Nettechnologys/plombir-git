//! Regression coverage for card_7fa843b39846: `provider_type` decides *where* a
//! provider's identities are stored, and the same `PATCH` that edits a provider
//! can rewrite it.
//!
//! The two families do not share a table. A directory account carries the
//! provider's row id in `users.ldap_provider_id`; a federated one carries its
//! slug in `oauth_accounts.provider`. So the type is not a label on the row —
//! it is the answer to "which column points here", and a guard that reads
//! today's answer about yesterday's rows counts the wrong table.
//!
//! Two halves are covered, because a fix to either one alone still leaves the
//! hole open:
//!
//! * the delete guard must count **both** tables, whatever the type says — an
//!   instance can already be carrying a row whose type was flipped before the
//!   refusal below existed, and that row's `DELETE` must still be refused;
//! * the flip itself must be refused while identities reach the provider
//!   through the side it is leaving, because there is nothing to carry: a
//!   directory binding has no `provider_user_id` to become an OAuth link.
//!
//! The last test holds the other side of the line. `oauth2` and `oidc` store
//! their links in the same column, so moving between them strands nobody and
//! must stay an ordinary edit. A guard written as "the type changed" rather
//! than "the storage changed" would pass the first three tests and fail this
//! one.

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

/// Bind an account to a provider the way an LDAP sign-in does, by row id.
async fn bind_to_directory(db: &sea_orm::DatabaseConnection, user_id: i64, provider_id: i64) {
    let member = rg_db::entities::user::Entity::find_by_id(user_id)
        .one(db)
        .await
        .expect("read the member")
        .expect("the registered member must exist");
    let mut member: rg_db::entities::user::ActiveModel = member.into();
    member.ldap_provider_id = Set(Some(provider_id));
    member.update(db).await.expect("bind the member");
}

/// The delete guard, asked about a row whose type does not name the table its
/// links are in.
///
/// This is the state an instance reaches on its own: before the refusal below
/// existed, a `PATCH` could move a directory provider to `oidc` and leave the
/// bound accounts behind. The guard is the last thing standing between that row
/// and a `users.ldap_provider_id` pointing at nothing, so it must count both
/// tables and never ask the type which one to look in.
#[tokio::test]
async fn deleting_a_provider_counts_the_directory_half_its_type_no_longer_names() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = admin(&base, &db, "sso-type-del").await;
    let (_, member_id) = register_full(&base, "typed-member", "typed-member@example.test").await;

    // The provider calls itself federated; the account is bound to it the
    // directory way. Written straight to the database, because the route that
    // used to produce this pairing now refuses to.
    let provider_id = create_provider(
        &base,
        &token,
        serde_json::json!({
            "name": "Corporate",
            "slug": "corporate",
            "provider_type": "oidc",
            "enabled": false,
        }),
    )
    .await;
    bind_to_directory(&db, member_id, provider_id).await;
    assert_eq!(
        rg_db::ops::oauth_account_ops::count_by_provider(&db, "corporate")
            .await
            .expect("count the federated half"),
        0,
        "the half the provider's own type names must be empty, or this test proves nothing"
    );

    let (status, body) = delete_provider(&base, &token, provider_id).await;
    assert_eq!(
        status, 409,
        "the accounts bound to this provider hold it, whatever its type column says: {body}"
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
}

/// Moving a directory off `ldap` abandons every account bound to it: the login
/// path for a federated provider never reads `users.ldap_provider_id`.
#[tokio::test]
async fn moving_a_directory_provider_to_oidc_with_bound_accounts_is_a_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = admin(&base, &db, "sso-type-ldap").await;
    let (_, member_id) = register_full(&base, "dir-member", "dir-member@example.test").await;

    let provider_id = create_provider(
        &base,
        &token,
        serde_json::json!({
            "name": "Directory",
            "slug": "directory",
            "provider_type": "ldap",
            "enabled": false,
        }),
    )
    .await;
    bind_to_directory(&db, member_id, provider_id).await;

    let (status, body) = patch_provider(
        &base,
        &token,
        provider_id,
        serde_json::json!({
            "name": "Directory",
            "slug": "directory",
            "provider_type": "oidc",
            "enabled": false,
        }),
    )
    .await;
    assert_eq!(
        status, 409,
        "accounts bound to the directory stand in the way of the move, and the identical PATCH \
         succeeds once they are unbound: {body}"
    );
    assert!(
        body.contains("strand"),
        "the conflict must say what the change would do to them, got: {body}"
    );
    assert_eq!(
        rg_db::ops::sso_provider_ops::find_by_id(&db, provider_id)
            .await
            .expect("read the provider back")
            .expect("a refused write leaves the provider in place")
            .provider_type,
        "ldap",
        "a refused type change must not write the new type"
    );

    // And the guard behind it still holds: the row is unchanged, so the
    // accounts still hold it against a delete.
    let (status, body) = delete_provider(&base, &token, provider_id).await;
    assert_eq!(status, 409, "{body}");
}

/// The mirror image: a federated provider's links name its slug, and an LDAP
/// sign-in never looks at `oauth_accounts`.
#[tokio::test]
async fn moving_a_federated_provider_to_ldap_with_linked_identities_is_a_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = admin(&base, &db, "sso-type-oidc").await;

    let provider_id = create_provider(
        &base,
        &token,
        serde_json::json!({
            "name": "Corporate",
            "slug": "corporate",
            "provider_type": "oidc",
            "enabled": false,
        }),
    )
    .await;
    rg_db::ops::oauth_account_ops::link(
        &db,
        admin_id,
        "corporate",
        "external-user-1",
        "sso-type-oidc",
        "sso-type-oidc@example.test",
    )
    .await
    .expect("link the identity")
    .expect("the link must be present");

    let (status, body) = patch_provider(
        &base,
        &token,
        provider_id,
        serde_json::json!({
            "name": "Corporate",
            "slug": "corporate",
            "provider_type": "ldap",
            "enabled": false,
        }),
    )
    .await;
    assert_eq!(
        status, 409,
        "the linked identity stands in the way of the move: {body}"
    );
    assert_eq!(
        rg_db::ops::oauth_account_ops::count_by_provider(&db, "corporate")
            .await
            .expect("count the links"),
        1,
        "a refused type change must leave the links where they are"
    );
}

/// The line this fix must not cross: `oauth2` and `oidc` are the same storage,
/// so an edit between them carries nothing and stays a `200`.
#[tokio::test]
async fn moving_between_two_federated_types_stays_allowed() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = admin(&base, &db, "sso-type-federated").await;

    let provider_id = create_provider(
        &base,
        &token,
        serde_json::json!({
            "name": "Workforce",
            "slug": "workforce",
            "provider_type": "oauth2",
            "enabled": false,
        }),
    )
    .await;
    rg_db::ops::oauth_account_ops::link(
        &db,
        admin_id,
        "workforce",
        "external-user-1",
        "sso-type-federated",
        "sso-type-federated@example.test",
    )
    .await
    .expect("link the identity")
    .expect("the link must be present");

    let (status, body) = patch_provider(
        &base,
        &token,
        provider_id,
        serde_json::json!({
            "name": "Workforce",
            "slug": "workforce",
            "provider_type": "oidc",
            "enabled": false,
            "discovery_url": "https://idp.example.test/.well-known/openid-configuration",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "both types link through `oauth_accounts.provider`, so this edit strands nobody: {body}"
    );
    assert!(
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
            &db,
            "workforce",
            "external-user-1"
        )
        .await
        .expect("look the identity up")
        .is_some(),
        "the link must still be found where it was written"
    );
}
