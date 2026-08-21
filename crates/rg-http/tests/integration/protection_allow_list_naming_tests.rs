//! The exception lists of branch and tag protection, named (card_ce28fbce054c).
//!
//! Both allow-lists took and returned numeric ids alone, and the settings form
//! that fills them carried the placeholder `42, 108`. There is no
//! `/users/{username}` on this instance, `/search` does not index accounts and
//! `/admin/users` belongs to the instance admin — so the owner of a repository
//! who wanted to let one colleague push to `main` had no way to obtain the
//! number the form asked for. That is the dead end `UserRef` exists for, and
//! the one the live incident behind this phase came out of.
//!
//! Every assertion below is about the pair: a name goes in, and the id the push
//! gate compares against comes out of it — a rule that accepted the name and
//! stored nobody would read as success on both ends.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn bad_request_message(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("decode error response");
    assert_eq!(status, 400, "expected a client error, got {status}: {body}");
    body["error"]["message"]
        .as_str()
        .expect("an error response carries a message")
        .to_string()
}

/// The names on a rule's allow-list, in the order the response carries them.
fn named(rule: &serde_json::Value, field: &str) -> Vec<String> {
    rule[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} is an array: {rule}"))
        .iter()
        .map(|user| {
            user["username"]
                .as_str()
                .unwrap_or_else(|| panic!("an allow-list entry carries a username: {rule}"))
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn a_branch_rule_takes_the_names_of_the_people_it_excepts_and_gives_them_back() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _) =
        register_full(&base, "namedpush-owner", "namedpush-owner@example.com").await;
    let (_, alice_id) =
        register_full(&base, "namedpush-alice", "namedpush-alice@example.com").await;
    let (_, bob_id) = register_full(&base, "namedpush-bob", "namedpush-bob@example.com").await;
    create_repo(&base, &owner_token, "namedpush-repo").await;
    let endpoint =
        format!("{base}/api/v1/repos/namedpush-owner/namedpush-repo/branches/protection");

    // One username and one e-mail: `UserRef::from_identifier` decides which is
    // which, so the form can ask for "user" and mean any of the three.
    let created = client
        .post(&endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_pr": true,
            "allowed_push_users": ["namedpush-alice", "namedpush-bob@example.com"]
        }))
        .send()
        .await
        .expect("create the rule");
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.expect("a JSON rule");
    assert_eq!(
        named(&created, "allowed_push_users"),
        vec!["namedpush-alice", "namedpush-bob"],
        "the rule did not name the people it was created for: {created}"
    );

    // The half that matters: the names resolved to the ids the push gate reads.
    // `allowed_push_user_ids` is the stored mirror of exactly that list.
    let stored: Vec<i64> = serde_json::from_str(
        created["allowed_push_user_ids"]
            .as_str()
            .unwrap_or_else(|| panic!("the stored mirror is a JSON string: {created}")),
    )
    .expect("the stored mirror is a JSON array of ids");
    assert_eq!(
        stored
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [alice_id, bob_id].into_iter().collect(),
        "the names were accepted but the grant stored somebody else: {created}"
    );

    // Reading the rule back names them too — the listing is what the settings
    // page fills its form from, and filling it from ids is the whole defect.
    let listed: serde_json::Value = client
        .get(&endpoint)
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("list the rules")
        .json()
        .await
        .expect("a JSON listing");
    assert_eq!(
        named(&listed[0], "allowed_push_users"),
        vec!["namedpush-alice", "namedpush-bob"],
        "the listing did not name the allow-list: {listed}"
    );

    // Revoking one grant, by name.
    let rule_id = created["id"].as_i64().expect("the rule carries an id");
    let updated: serde_json::Value = client
        .patch(format!("{endpoint}/{rule_id}"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "allowed_push_users": ["namedpush-alice"] }))
        .send()
        .await
        .expect("update the rule")
        .json()
        .await
        .expect("a JSON rule");
    assert_eq!(
        named(&updated, "allowed_push_users"),
        vec!["namedpush-alice"],
        "the update did not replace the allow-list: {updated}"
    );

    // A name that matches nobody is the caller's to fix, and the refusal says
    // which one — a number would have been guessed at and silently stored.
    let message = bad_request_message(
        client
            .patch(format!("{endpoint}/{rule_id}"))
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({ "allowed_push_users": ["namedpush-alice", "nobody-here"] }))
            .send()
            .await
            .expect("update with an unknown name"),
    )
    .await;
    assert!(
        message.contains("nobody-here"),
        "the refusal did not name the entry that failed: {message}"
    );

    // And the refused update changed nothing.
    let after: serde_json::Value = client
        .get(format!("{endpoint}/{rule_id}"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("read the rule back")
        .json()
        .await
        .expect("a JSON rule");
    assert_eq!(named(&after, "allowed_push_users"), vec!["namedpush-alice"]);
}

#[tokio::test]
async fn a_tag_rule_takes_the_names_of_the_people_it_excepts_and_gives_them_back() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _) =
        register_full(&base, "namedtag-owner", "namedtag-owner@example.com").await;
    let (_, alice_id) = register_full(&base, "namedtag-alice", "namedtag-alice@example.com").await;
    create_repo(&base, &owner_token, "namedtag-repo").await;
    let endpoint = format!("{base}/api/v1/repos/namedtag-owner/namedtag-repo/tags/protection");

    let created = client
        .post(&endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "pattern": "v*",
            "allowed_users": ["namedtag-alice"]
        }))
        .send()
        .await
        .expect("create the rule");
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.expect("a JSON rule");
    assert_eq!(named(&created, "allowed_users"), vec!["namedtag-alice"]);
    assert_eq!(
        created["allowed_user_ids"],
        serde_json::json!([alice_id]),
        "the name was accepted but the grant stored somebody else: {created}"
    );

    let rule_id = created["id"].as_i64().expect("the rule carries an id");

    // `PATCH` updates the allow-list and nothing else, so a body that names no
    // list at all is a request with no content — answering it `200` would tell
    // the operator a change was made.
    let message = bad_request_message(
        client
            .patch(format!("{endpoint}/{rule_id}"))
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("update with an empty body"),
    )
    .await;
    assert!(
        message.contains("allowed_users"),
        "the refusal did not say what the body was missing: {message}"
    );

    // Clearing it is still possible, and still means "nobody": an empty list is
    // not a missing one.
    let cleared: serde_json::Value = client
        .patch(format!("{endpoint}/{rule_id}"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "allowed_users": [] }))
        .send()
        .await
        .expect("clear the allow-list")
        .json()
        .await
        .expect("a JSON rule");
    assert_eq!(cleared["allowed_user_ids"], serde_json::json!([]));
    assert_eq!(cleared["allowed_users"], serde_json::json!([]));
}
