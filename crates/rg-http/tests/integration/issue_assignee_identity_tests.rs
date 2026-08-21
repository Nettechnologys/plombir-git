//! Putting an issue on a person by the name the person is known by.
//!
//! `PATCH /repos/{owner}/{name}/issues/{number}` took `assignee_id` and
//! nothing else. Inside the web app that is hidden behind a dropdown, but an
//! API client — the MCP agent this instance ships a server for, most of all —
//! has no dropdown and no way to turn a name into a number: this instance
//! publishes no `/users/{username}`, `/search` does not index accounts, and
//! `/admin/users` belongs to the instance admin (card_e6e65ef58404).
//!
//! So what is pinned here is both directions of the same field: a name is
//! enough to assign, a name that matches nobody is the caller's mistake said
//! out loud, `null` still clears, and the issue says who it is on rather than
//! only which number it is on.

use crate::common::{create_repo, register_full, spawn_test_app};

/// One issue, on a repository whose owner can also see it.
async fn seeded_issue(base: &str, token: &str) -> String {
    create_repo(base, token, "atlas").await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/assign-owner/atlas/issues"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "title": "needs an owner" }))
        .send()
        .await
        .expect("create issue");
    assert_eq!(created.status(), 201, "baseline: the issue exists");
    format!("{base}/api/v1/repos/assign-owner/atlas/issues/1")
}

#[tokio::test]
async fn an_issue_is_assigned_by_username_and_answers_with_the_name() {
    let base = spawn_test_app().await;
    let (token, owner_id) = register_full(&base, "assign-owner", "assign-owner@example.com").await;
    let client = reqwest::Client::new();
    let url = seeded_issue(&base, &token).await;

    let assigned: serde_json::Value = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "assignee": "assign-owner" }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(
        assigned["assignee_id"], owner_id,
        "a username has to be enough to assign: {assigned}"
    );
    assert_eq!(
        assigned["assignee"], "assign-owner",
        "the issue must say who it is on, not only which number: {assigned}"
    );

    // The listing carries the name too — that is where a client renders a
    // board of issues from, one request rather than one per row.
    let listed: serde_json::Value = client
        .get(format!("{base}/api/v1/repos/assign-owner/atlas/issues"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(
        listed["data"][0]["assignee"], "assign-owner",
        "got: {listed}"
    );

    // `null` still clears: the name is a second way to write the same field,
    // not a second field with rules of its own.
    let cleared: serde_json::Value = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "assignee": null }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert!(
        cleared["assignee_id"].is_null() && cleared["assignee"].is_null(),
        "an explicit null must clear the assignee: {cleared}"
    );
}

/// An e-mail is the other name a person has, and the numeric id keeps working
/// — this widened what the endpoint accepts, it did not replace anything.
#[tokio::test]
async fn an_email_and_a_bare_id_are_both_still_accepted() {
    let base = spawn_test_app().await;
    let (token, owner_id) = register_full(&base, "assign-owner", "assign-owner@example.com").await;
    let client = reqwest::Client::new();
    let url = seeded_issue(&base, &token).await;

    let by_email: serde_json::Value = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "assignee": "assign-owner@example.com" }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(by_email["assignee_id"], owner_id, "got: {by_email}");

    client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "assignee": null }))
        .send()
        .await
        .expect("request");

    // The identifier read as a bare id, and the original numeric field.
    let by_id_string: serde_json::Value = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "assignee": owner_id.to_string() }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(by_id_string["assignee_id"], owner_id, "got: {by_id_string}");

    let by_number: serde_json::Value = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "assignee_id": owner_id }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(by_number["assignee_id"], owner_id, "got: {by_number}");
    assert_eq!(
        by_number["assignee"], "assign-owner",
        "the numeric path answers with the name as well: {by_number}"
    );
}

/// The two ways to write one field cannot both be sent: they mean the same
/// thing, so a body carrying both is ambiguous the moment they disagree — and
/// picking a winner would decide which one silently.
#[tokio::test]
async fn naming_the_assignee_twice_is_refused_rather_than_resolved() {
    let base = spawn_test_app().await;
    let (token, owner_id) = register_full(&base, "assign-owner", "assign-owner@example.com").await;
    let client = reqwest::Client::new();
    let url = seeded_issue(&base, &token).await;

    let refused = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "assignee": "assign-owner", "assignee_id": owner_id }))
        .send()
        .await
        .expect("request");
    assert_eq!(refused.status(), 400);
    let body = refused.text().await.expect("body");
    assert!(
        body.contains("assignee") && body.contains("assignee_id"),
        "the refusal has to name both keys it is about: {body}"
    );

    let untouched: serde_json::Value = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert!(
        untouched["assignee_id"].is_null(),
        "a refused request must not have assigned anyone: {untouched}"
    );
}

/// A name matching nobody is the caller's mistake, said out loud — the reason
/// the resolver returns the account row rather than an unchecked number.
#[tokio::test]
async fn a_name_matching_nobody_is_a_400_that_repeats_it() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "assign-owner", "assign-owner@example.com").await;
    let client = reqwest::Client::new();
    let url = seeded_issue(&base, &token).await;

    let refused = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "assignee": "nobody-here" }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        refused.status(),
        400,
        "a name that matches nobody is a bad request, not a server failure"
    );
    let body = refused.text().await.expect("body");
    assert!(
        body.contains("nobody-here"),
        "the error has to repeat what was asked for: {body}"
    );
}
