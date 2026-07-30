//! card_934b6037bcda: `POST /repos/{owner}/{name}/transfer` handed a
//! repository to a stranger and asked no one on the receiving side.
//!
//! `RepoOwner` gates the *source* — that half was never in doubt — while
//! `new_owner` arrived in the request body, where no path extractor reaches it.
//! `transfer_repo` checked exactly one thing (`repo.owner_id != user_id`) and
//! then resolved `new_owner` to any existing user or organization and rewrote
//! the row. Verified against `HEAD` before the fix: a repository moved under
//! another account with a `200`, and `GET /repos/{victim}/{name}` answered
//! `200` from the victim's namespace. Anything could be signed with someone
//! else's name that way.
//!
//! The destination is now gated by `NamespaceCreate`, the body-reading sibling
//! of the import gate: a transfer may land only where the caller could have
//! created the repository themselves — their own account, or an organization
//! they belong to.

use crate::common::{register_full, spawn_test_app};

async fn transfer(base: &str, token: &str, owner: &str, name: &str, new_owner: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{name}/transfer"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "new_owner": new_owner }))
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

async fn create_repo(base: &str, token: &str, name: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

async fn create_repo_in_org(base: &str, token: &str, name: &str, org: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "org": org }))
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

async fn repo_visible_at(base: &str, token: &str, owner: &str, name: &str) -> bool {
    reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{name}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .status()
        .is_success()
}

async fn create_org(base: &str, token: &str, name: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "visibility": "public" }))
        .send()
        .await
        .expect("create org")
        .status();
    assert_eq!(status, 201, "baseline: the organization exists");
}

async fn add_org_member(base: &str, token: &str, org: &str, user_id: i64) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs/{org}/members"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "user_id": user_id, "role": "member" }))
        .send()
        .await
        .expect("add org member")
        .status();
    assert_eq!(status, 201, "baseline: the member is in the org");
}

/// The reported hole: a repository pushed under an unrelated account.
#[tokio::test]
async fn a_repository_cannot_be_transferred_onto_a_stranger() {
    let base = spawn_test_app().await;
    let (mine_token, _) = register_full(&base, "tp-mine", "tp-mine@example.com").await;
    let (victim_token, _) = register_full(&base, "tp-victim", "tp-victim@example.com").await;

    assert_eq!(
        create_repo(&base, &mine_token, "junk").await,
        201,
        "baseline: there is a repository to give away"
    );

    assert_eq!(
        transfer(&base, &mine_token, "tp-mine", "junk", "tp-victim").await,
        403,
        "a repository landed in a stranger's namespace without them agreeing to it"
    );
    assert!(
        !repo_visible_at(&base, &victim_token, "tp-victim", "junk").await,
        "the denied transfer moved the repository anyway"
    );
    assert!(
        repo_visible_at(&base, &mine_token, "tp-mine", "junk").await,
        "the denied transfer left the repository unreachable at its own owner"
    );
}

/// An organization is a namespace like any other: membership decides, and a
/// non-member is refused even though the organization plainly exists.
#[tokio::test]
async fn a_repository_cannot_be_transferred_into_an_organization_one_is_not_in() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "tp2-owner", "tp2-owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "tp2-out", "tp2-out@example.com").await;

    create_org(&base, &owner_token, "tp2corp").await;
    assert_eq!(
        create_repo(&base, &outsider_token, "dumped").await,
        201,
        "baseline: the outsider has a repository of their own"
    );

    assert_eq!(
        transfer(&base, &outsider_token, "tp2-out", "dumped", "tp2corp").await,
        403,
        "a non-member dumped a repository into someone else's organization"
    );
    assert!(
        !repo_visible_at(&base, &owner_token, "tp2corp", "dumped").await,
        "the denied transfer moved the repository into the organization anyway"
    );
}

/// A destination that is neither a user nor an organization is a denial, not a
/// `404`: the gate is not an account-existence oracle for a caller with no
/// business in that namespace either way.
#[tokio::test]
async fn an_unknown_destination_is_denied_rather_than_reported_missing() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "tp3-user", "tp3-user@example.com").await;

    assert_eq!(create_repo(&base, &token, "solo").await, 201);
    assert_eq!(
        transfer(&base, &token, "tp3-user", "solo", "nobody-by-that-name").await,
        403,
    );
}

/// The baseline, in the same test as the denials: the transfers that *are*
/// legitimate still work, and the repository really moves.
#[tokio::test]
async fn an_org_member_transfers_into_the_organization() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "tp4-owner", "tp4-owner@example.com").await;
    let (member_token, member_id) = register_full(&base, "tp4-mem", "tp4-mem@example.com").await;

    create_org(&base, &owner_token, "tp4corp").await;
    add_org_member(&base, &owner_token, "tp4corp", member_id).await;

    assert_eq!(
        create_repo(&base, &member_token, "widgets").await,
        201,
        "baseline: the member has a repository to move"
    );
    assert_eq!(
        transfer(&base, &member_token, "tp4-mem", "widgets", "tp4corp").await,
        200,
        "a member may transfer into an organization they belong to"
    );
    assert!(
        repo_visible_at(&base, &member_token, "tp4corp", "widgets").await,
        "the accepted transfer did not actually move the repository"
    );
}

/// The way back out, which is the transfer that used to be impossible.
///
/// An organization's repository is stored under `org.owner_id`, and the
/// destination-collision check asked `find_by_owner_and_name(new_owner_id,
/// name)` — for the organization's owner, the same account. So the check found
/// the repository *itself* and answered `400 already exists at destination`: a
/// repository could enter an organization and never leave (card_92019cc97dcd).
///
/// Only the organization's owner can do this — a transfer into an organization
/// rewrites `owner_id` to the organization's owner, so the member who moved it
/// in is no longer its owner. That is the pre-existing ownership rule, asserted
/// here as the baseline it is rather than changed.
#[tokio::test]
async fn an_org_repository_transfers_back_out_to_its_owners_account() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "tp6-owner", "tp6-owner@example.com").await;

    create_org(&base, &owner_token, "tp6corp").await;
    assert_eq!(
        create_repo_in_org(&base, &owner_token, "gadgets", "tp6corp").await,
        201,
        "baseline: the organization has a repository to hand back"
    );
    assert!(
        repo_visible_at(&base, &owner_token, "tp6corp", "gadgets").await,
        "baseline: it answers at the organization's path"
    );

    assert_eq!(
        transfer(&base, &owner_token, "tp6corp", "gadgets", "tp6-owner").await,
        200,
        "the organization's owner cannot take their own repository back out"
    );
    assert!(
        repo_visible_at(&base, &owner_token, "tp6-owner", "gadgets").await,
        "the accepted transfer did not actually move the repository"
    );
    assert!(
        !repo_visible_at(&base, &owner_token, "tp6corp", "gadgets").await,
        "the repository still answers at the organization it left"
    );
}

/// The destination gate does not displace the answers the route already gave:
/// anonymous is still `401`, and a caller who does not own the *source* is
/// still refused there — even when the destination they name is their own.
#[tokio::test]
async fn the_destination_gate_does_not_displace_the_source_gate() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "tp5-owner", "tp5-owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "tp5-out", "tp5-out@example.com").await;

    assert_eq!(create_repo(&base, &owner_token, "loot").await, 201);

    let anonymous = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/tp5-owner/loot/transfer"))
        .json(&serde_json::json!({ "new_owner": "tp5-out" }))
        .send()
        .await
        .expect("request")
        .status();
    assert_eq!(anonymous, 401, "no session is still 401");

    assert_eq!(
        transfer(&base, &outsider_token, "tp5-owner", "loot", "tp5-out").await,
        403,
        "an outsider helped themselves to someone else's repository"
    );
    assert!(
        !repo_visible_at(&base, &outsider_token, "tp5-out", "loot").await,
        "the repository was taken anyway"
    );
}
