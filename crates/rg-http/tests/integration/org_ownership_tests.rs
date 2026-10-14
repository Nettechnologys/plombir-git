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

use crate::common::{register_full, spawn_test_app};

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
