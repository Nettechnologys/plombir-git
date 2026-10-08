//! Letting a person into an organization by the only thing about them their
//! prospective colleagues actually know: their name.
//!
//! The field report behind these tests (card_cb9f71672b11): the owner of an
//! organization on the live instance wanted to give `Zubrenok` access, found a
//! form asking for a numeric "User ID" they had no way to obtain, and created
//! a **team** named `Zubrenok` instead. The audit log shows `org.create →
//! team.create → repo.create` and no `org.add_member` at all — the permission
//! model worked perfectly on a membership that was never created.
//!
//! So the assertions here are about the *entry* to the org, not about what
//! membership grants once it exists: the name is accepted, the name comes
//! back, a name matching nobody is the caller's mistake rather than a server
//! failure, and the person who got in can see what membership was supposed to
//! give them.

use crate::common::{register_full, spawn_test_app};

async fn create_org(base: &str, token: &str, name: &str, visibility: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "visibility": visibility }))
        .send()
        .await
        .expect("create org")
        .status();
    assert_eq!(status, 201, "baseline: the organization exists");
}

/// The scenario from the report, end to end: an owner who knows a username and
/// nothing else gets that person into the organization, sees them named in the
/// member list, and the person can reach the organization's private repository.
#[tokio::test]
async fn an_owner_can_add_a_member_by_username_and_see_them_named() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "zebra-owner", "zebra-owner@example.com").await;
    let (guest_token, guest_id) = register_full(&base, "Zubrenok", "zubrenok@example.com").await;
    let client = reqwest::Client::new();

    create_org(&base, &owner_token, "zebra", "private").await;

    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "ledger",
            "org": "zebra",
            "is_private": true,
        }))
        .send()
        .await
        .expect("create repo");
    assert_eq!(created.status(), 201, "baseline: the org has a repository");

    let hidden = client
        .get(format!("{base}/api/v1/repos/zebra/ledger"))
        .bearer_auth(&guest_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        hidden.status(),
        403,
        "baseline: the guest cannot see the private repo before joining"
    );

    // The whole point: the owner names the human, not a number they cannot
    // look up anywhere on this instance.
    let added = client
        .post(format!("{base}/api/v1/orgs/zebra/members"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username": "Zubrenok", "role": "member"}))
        .send()
        .await
        .expect("request");
    assert_eq!(added.status(), 201, "a username must be enough to join");
    let body: serde_json::Value = added.json().await.expect("json body");
    assert_eq!(body["user_id"], guest_id);
    assert_eq!(
        body["username"], "Zubrenok",
        "the response has to confirm *who* was added, got: {body}"
    );

    let listed = client
        .get(format!("{base}/api/v1/orgs/zebra/members"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request");
    assert_eq!(listed.status(), 200);
    let members: serde_json::Value = listed.json().await.expect("json body");
    let names: Vec<&str> = members
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|m| m["username"].as_str())
        .collect();
    assert!(
        names.contains(&"Zubrenok"),
        "the member list must name its members, got: {members}"
    );

    let visible = client
        .get(format!("{base}/api/v1/repos/zebra/ledger"))
        .bearer_auth(&guest_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        visible.status(),
        200,
        "the point of joining is reaching the org's private repository"
    );
}

/// Teams are the other half of the same surface — and the one the owner in the
/// report actually reached for. A username names an account here too, and the
/// team roster comes back named.
#[tokio::test]
async fn a_team_member_can_be_added_by_username_and_the_roster_is_named() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "tmid-owner", "tmid-owner@example.com").await;
    let (_guest_token, guest_id) =
        register_full(&base, "tmid-guest", "tmid-guest@example.com").await;
    let client = reqwest::Client::new();

    create_org(&base, &owner_token, "tmidcorp", "public").await;

    let team = client
        .post(format!("{base}/api/v1/orgs/tmidcorp/teams"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": "reviewers", "permission": "write"}))
        .send()
        .await
        .expect("create team");
    assert_eq!(team.status(), 201);
    let team: serde_json::Value = team.json().await.expect("json body");
    let team_id = team["id"].as_i64().expect("team id");

    let added = client
        .post(format!(
            "{base}/api/v1/orgs/tmidcorp/teams/{team_id}/members"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username": "tmid-guest", "role": "member"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        added.status(),
        201,
        "a username must be enough to join a team"
    );
    let body: serde_json::Value = added.json().await.expect("json body");
    assert_eq!(body["user_id"], guest_id);
    assert_eq!(body["username"], "tmid-guest");

    let listed = client
        .get(format!(
            "{base}/api/v1/orgs/tmidcorp/teams/{team_id}/members"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request");
    assert_eq!(listed.status(), 200);
    let members: serde_json::Value = listed.json().await.expect("json body");
    let names: Vec<&str> = members
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|m| m["username"].as_str())
        .collect();
    assert!(
        names.contains(&"tmid-guest"),
        "the team roster must name its members, got: {members}"
    );
}

/// card_9e97b992d4a6: an address names whoever registered it first, because
/// nothing here confirms addresses. The squatter below typed the address the
/// owner believes is a colleague's; adding "that address" must not make the
/// squatter a member — of the organization or of a team.
#[tokio::test]
async fn an_email_grants_nothing_to_the_account_that_claimed_it() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "sqat-owner", "sqat-owner@example.com").await;
    let (_squatter_token, squatter_id) =
        register_full(&base, "sqat-mallory", "bob-real@corp.example").await;
    let client = reqwest::Client::new();

    create_org(&base, &owner_token, "sqatcorp", "private").await;
    let team = client
        .post(format!("{base}/api/v1/orgs/sqatcorp/teams"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": "core", "permission": "write"}))
        .send()
        .await
        .expect("create team");
    assert_eq!(team.status(), 201);
    let team: serde_json::Value = team.json().await.expect("json body");
    let team_id = team["id"].as_i64().expect("team id");

    for url in [
        format!("{base}/api/v1/orgs/sqatcorp/members"),
        format!("{base}/api/v1/orgs/sqatcorp/teams/{team_id}/members"),
    ] {
        let added = client
            .post(&url)
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({"email": "bob-real@corp.example", "role": "member"}))
            .send()
            .await
            .expect("request");
        let status = added.status();
        let body = added.text().await.expect("body");
        assert_eq!(status, 400, "{url}: an e-mail must be refused, got: {body}");
        assert!(
            body.contains("addresses are not confirmed"),
            "{url}: the refusal has to say why, got: {body}"
        );

        let listed = client
            .get(&url)
            .bearer_auth(&owner_token)
            .send()
            .await
            .expect("request");
        assert_eq!(listed.status(), 200);
        let members: serde_json::Value = listed.json().await.expect("json body");
        assert!(
            !members
                .as_array()
                .expect("array")
                .iter()
                .any(|m| m["user_id"].as_i64() == Some(squatter_id)),
            "{url}: the account that claimed the address became a member: {members}"
        );
    }
}

/// The numeric id still works — widening what the endpoint accepts must not
/// break the clients that already send one.
#[tokio::test]
async fn the_numeric_user_id_still_works() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "numid-owner", "numid-owner@example.com").await;
    let (_guest_token, guest_id) =
        register_full(&base, "numid-guest", "numid-guest@example.com").await;
    let client = reqwest::Client::new();

    create_org(&base, &owner_token, "numidcorp", "public").await;

    let added = client
        .post(format!("{base}/api/v1/orgs/numidcorp/members"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"user_id": guest_id, "role": "admin"}))
        .send()
        .await
        .expect("request");
    assert_eq!(added.status(), 201, "a numeric user_id is still accepted");
    let body: serde_json::Value = added.json().await.expect("json body");
    assert_eq!(body["user_id"], guest_id);
    assert_eq!(body["role"], "admin");
    assert_eq!(
        body["username"], "numid-guest",
        "even the numeric path now says who it added"
    );
}

/// A body that names nobody, or names somebody who does not exist, is the
/// caller's mistake — and the answer has to repeat what they asked for, so the
/// mistake is visible. A `user_id` matching no account used to reach the
/// `organization_members` foreign key and come back as a 500.
#[tokio::test]
async fn an_unknown_member_is_a_400_that_names_what_was_asked_for() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "badid-owner", "badid-owner@example.com").await;
    let client = reqwest::Client::new();

    create_org(&base, &owner_token, "badidcorp", "public").await;
    let url = format!("{base}/api/v1/orgs/badidcorp/members");

    let resp = client
        .post(&url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username": "no-such-person", "role": "member"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an unknown username is the caller's");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        body.to_string().contains("no-such-person"),
        "the refusal has to name what was asked for, got: {body}"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"user_id": 999_999, "role": "member"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "an id matching no account is a bad request, not a broken insert"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"role": "member"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "a body naming nobody is still a 400");
}
