//! Organization rights come from membership roles, never from `owner_id`
//! (security audit PR #11).
//!
//! An organization's repositories are stored with `owner_id` set to the
//! organization's owner at creation, and `organizations.owner_id` names the
//! same account. Both columns used to be read as grants, and nothing ever
//! rewrote either: a creator removed from the organization kept reading,
//! writing, deleting and transferring its private repositories, kept seeing
//! them in listings, and could still delete the organization — while a member
//! holding the `owner` role could not delete a repository, because the row
//! did not name them.

use serde_json::Value;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::common::fault::{fail_db_writes, DbWrite};
use crate::common::{register_full, spawn_test_app, spawn_test_app_with_db};

async fn call(
    method: reqwest::Method,
    url: String,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("request");
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_creator_removed_from_the_organization_keeps_nothing_and_the_owner_role_owns() {
    let base = spawn_test_app().await;
    let (creator, creator_id) =
        register_full(&base, "own-creator", "own-creator@example.com").await;
    let (owner, _) = register_full(&base, "own-second", "own-second@example.com").await;
    let (admin, _) = register_full(&base, "own-admin", "own-admin@example.com").await;
    let api = |path: &str| format!("{base}/api/v1{path}");
    use reqwest::Method;

    let (status, body) = call(
        Method::POST,
        api("/orgs"),
        &creator,
        Some(serde_json::json!({"name": "own-org", "visibility": "public"})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    for (user, role) in [("own-second", "owner"), ("own-admin", "admin")] {
        let (status, body) = call(
            Method::POST,
            api("/orgs/own-org/members"),
            &creator,
            Some(serde_json::json!({"username": user, "role": role})),
        )
        .await;
        assert_eq!(status, 201, "adding {user}: {body}");
    }
    for name in ["vault", "spare"] {
        let (status, body) = call(
            Method::POST,
            api("/repos"),
            &creator,
            Some(serde_json::json!({"name": name, "org": "own-org", "is_private": true})),
        )
        .await;
        assert_eq!(status, 201, "creating {name}: {body}");
    }

    let (status, body) = call(
        Method::DELETE,
        api(&format!("/orgs/own-org/members/{creator_id}")),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "removing the creator: {body}");

    // The creator: `owner_id` still names them on the organization and on
    // both repositories, and buys them nothing.
    let (status, _) = call(Method::GET, api("/repos/own-org/vault"), &creator, None).await;
    assert_eq!(
        status, 403,
        "a removed creator still reads a private repository"
    );
    let (status, listing) = call(Method::GET, api("/repos/own-org"), &creator, None).await;
    assert_eq!(status, 200);
    assert!(
        !listing.to_string().contains("vault"),
        "a removed creator still sees private repositories listed: {listing}"
    );
    let (status, found) = call(
        Method::GET,
        api("/search?q=vault&type=repos"),
        &creator,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        !found.to_string().contains("\"vault\""),
        "a removed creator still finds the private repository by search: {found}"
    );
    let (status, _) = call(
        Method::PATCH,
        api("/orgs/own-org"),
        &creator,
        Some(serde_json::json!({"description": "still mine?"})),
    )
    .await;
    assert_eq!(
        status, 403,
        "a removed creator may not administer the organization"
    );
    let (status, _) = call(Method::DELETE, api("/repos/own-org/vault"), &creator, None).await;
    assert!(
        status == 403 || status == 404,
        "a removed creator may not delete a repository, got {status}"
    );
    let (status, _) = call(Method::DELETE, api("/orgs/own-org"), &creator, None).await;
    assert_eq!(
        status, 403,
        "a removed creator may not delete the organization"
    );

    // The admin administers without owning.
    let (status, _) = call(Method::DELETE, api("/repos/own-org/vault"), &admin, None).await;
    assert_eq!(status, 403, "an admin may not dispose of a repository");
    let (status, _) = call(Method::DELETE, api("/orgs/own-org"), &admin, None).await;
    assert_eq!(status, 403, "an admin may not delete the organization");

    // The owner role owns, although no row names its holder.
    let (status, body) = call(Method::DELETE, api("/repos/own-org/vault"), &owner, None).await;
    assert!(
        (200..300).contains(&status),
        "an owner-role member deletes a repository: {status} {body}"
    );
    let (status, body) = call(Method::DELETE, api("/orgs/own-org"), &owner, None).await;
    assert_eq!(
        status, 200,
        "an owner-role member deletes the organization: {body}"
    );
}

/// The `owner` role is the one an organization cannot be left without, and the
/// power it carries is an owner's to hand out or take away.
///
/// An admin cannot give the role or remove someone who holds it. An owner may
/// leave while another remains, and the columns that name the organization's
/// owner — on the organization and on its repository rows, the latter with
/// `ON DELETE CASCADE` — move to the one who stays. The last owner cannot be
/// removed, and cannot be removed by deleting the account either: membership
/// cascades with the account, which used to take the last role with it.
#[tokio::test]
async fn the_last_owner_stays_and_only_owners_hand_out_or_take_away_the_role() {
    let (base, db) = spawn_test_app_with_db().await;
    let api = |path: &str| format!("{base}/api/v1{path}");
    use reqwest::Method;
    let (creator, creator_id) =
        register_full(&base, "life-creator", "life-creator@example.com").await;
    let (second, second_id) = register_full(&base, "life-second", "life-second@example.com").await;
    let (admin, _) = register_full(&base, "life-admin", "life-admin@example.com").await;
    let (_, member_id) = register_full(&base, "life-member", "life-member@example.com").await;
    register_full(&base, "life-newbie", "life-newbie@example.com").await;
    let (instance_admin, instance_admin_id) =
        register_full(&base, "life-root", "life-root@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, instance_admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .unwrap();

    let (status, body) = call(
        Method::POST,
        api("/orgs"),
        &creator,
        Some(serde_json::json!({"name": "life-org", "visibility": "public"})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let org_id = body["id"].as_i64().expect("org id");
    for (user, role) in [
        ("life-second", "owner"),
        ("life-admin", "admin"),
        ("life-member", "member"),
    ] {
        let (status, body) = call(
            Method::POST,
            api("/orgs/life-org/members"),
            &creator,
            Some(serde_json::json!({"username": user, "role": role})),
        )
        .await;
        assert_eq!(status, 201, "adding {user}: {body}");
    }
    let (status, body) = call(
        Method::POST,
        api("/repos"),
        &creator,
        Some(serde_json::json!({"name": "kept", "org": "life-org"})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let repo_id = body["id"].as_i64().expect("repo id");

    // An admin may not hand out the owner role, nor take it away.
    let (status, _) = call(
        Method::POST,
        api("/orgs/life-org/members"),
        &admin,
        Some(serde_json::json!({"username": "life-newbie", "role": "owner"})),
    )
    .await;
    assert_eq!(status, 403, "an admin handed out the owner role");
    let (status, _) = call(
        Method::DELETE,
        api(&format!("/orgs/life-org/members/{second_id}")),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, 403, "an admin removed an owner");
    let (status, _) = call(
        Method::DELETE,
        api(&format!("/orgs/life-org/members/{member_id}")),
        &admin,
        None,
    )
    .await;
    assert_eq!(status, 200, "an admin still removes an ordinary member");

    // The creator leaves; the columns naming the owner move to the owner who
    // stays.
    let (status, body) = call(
        Method::DELETE,
        api(&format!("/orgs/life-org/members/{creator_id}")),
        &creator,
        None,
    )
    .await;
    assert_eq!(status, 200, "an owner leaves while another remains: {body}");
    let org = rg_db::entities::organization::Entity::find_by_id(org_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        org.owner_id, second_id,
        "organizations.owner_id names a member"
    );
    let repo = rg_db::entities::repository::Entity::find_by_id(repo_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.owner_id, second_id,
        "a repository row left naming the departed creator would cascade with their account"
    );

    // The last owner stays, by request and by account deletion alike.
    let (status, body) = call(
        Method::DELETE,
        api(&format!("/orgs/life-org/members/{second_id}")),
        &second,
        None,
    )
    .await;
    assert_eq!(status, 409, "the last owner was removed: {body}");
    // A row from before the role was the rule: `owner_id` names somebody
    // else, so only the role says who must not be cascaded away.
    rg_db::entities::organization::Entity::update_many()
        .col_expr(
            rg_db::entities::organization::Column::OwnerId,
            sea_orm::sea_query::Expr::value(creator_id),
        )
        .filter(rg_db::entities::organization::Column::Id.eq(org_id))
        .exec(&db)
        .await
        .unwrap();
    let (status, body) = call(
        Method::DELETE,
        api(&format!("/admin/users/{second_id}")),
        &instance_admin,
        None,
    )
    .await;
    assert_eq!(
        status, 409,
        "deleting the last owner's account was not refused: {body}"
    );
    assert!(
        body.to_string().contains("last owner") && body.to_string().contains("life-org"),
        "the refusal names the organization and why: {body}"
    );
    assert_eq!(
        rg_db::ops::org_ops::list_orgs_solely_owned_by(&db, second_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// The creator's `owner` membership is what makes an organization
/// manageable, so it is created with the organization or not at all.
#[tokio::test]
async fn an_organization_is_created_with_its_owner_or_not_at_all() {
    let (base, db) = spawn_test_app_with_db().await;
    let (creator, _) = register_full(&base, "atomic-creator", "atomic-creator@example.com").await;

    let fault = fail_db_writes(&db, "organization_members", DbWrite::Insert).await;
    let (status, _) = call(
        reqwest::Method::POST,
        format!("{base}/api/v1/orgs"),
        &creator,
        Some(serde_json::json!({"name": "atomic-org", "visibility": "public"})),
    )
    .await;
    assert!(
        status >= 500,
        "a lost owner membership must fail the creation, got {status}"
    );
    assert!(
        rg_db::ops::org_ops::get_org_by_name(&db, "atomic-org")
            .await
            .unwrap()
            .is_none(),
        "the organization stayed without an owner"
    );

    fault.clear().await;
    let (status, body) = call(
        reqwest::Method::POST,
        format!("{base}/api/v1/orgs"),
        &creator,
        Some(serde_json::json!({"name": "atomic-org", "visibility": "public"})),
    )
    .await;
    assert_eq!(status, 201, "the name is free again: {body}");
}
