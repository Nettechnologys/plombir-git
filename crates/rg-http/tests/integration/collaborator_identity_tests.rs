//! Naming the people a repository's access list is about.
//!
//! `GET /repos/{owner}/{name}/collaborators` served `repo_collaborators` rows
//! straight out of the table, and a row holds a `user_id` and nothing else. So
//! the page that answers "who can push here" listed `#7`, the removal
//! confirmation asked about `#7`, and the issue page's assignee picker — built
//! from this same response — offered `User #7` for everyone but the reader
//! (card_73ce6d28518b). Adding a collaborator by username already worked; only
//! reading the result back did not.
//!
//! The asymmetry is the point of these assertions: adding by name already
//! worked, so what is pinned here is the reading side — the listing, the row
//! the permission change answers with, and the `display_name` a page shows
//! next to a handle. That a grant leaves with its account is a separate
//! promise, kept by the foreign key and covered by
//! `rg-db/tests/integration/user_delete_decisions.rs` (card_dd3f86fde48e).

use sea_orm::{ConnectionTrait, Value};

use crate::common::{register_full, spawn_test_app, spawn_test_app_with_db};

async fn create_repo(base: &str, token: &str, name: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .expect("create repo")
        .status();
    assert_eq!(status, 201, "baseline: the repository exists");
}

/// The whole round trip an owner makes: add by name, read the list, see names.
#[tokio::test]
async fn the_collaborator_list_names_the_people_it_lists() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "atlas-owner", "atlas-owner@example.com").await;
    let (_, guest_id) = register_full(&base, "Zubrenok", "zubrenok@example.com").await;
    let client = reqwest::Client::new();

    create_repo(&base, &owner_token, "ledger").await;

    let added = client
        .post(format!(
            "{base}/api/v1/repos/atlas-owner/ledger/collaborators"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "username": "Zubrenok", "permission": "write" }))
        .send()
        .await
        .expect("request");
    assert_eq!(added.status(), 201, "a username must be enough to add");
    let body: serde_json::Value = added.json().await.expect("json body");
    assert_eq!(body["user_id"], guest_id);
    assert_eq!(
        body["username"], "Zubrenok",
        "the response has to confirm *who* was added, got: {body}"
    );

    let listed: serde_json::Value = client
        .get(format!(
            "{base}/api/v1/repos/atlas-owner/ledger/collaborators"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    let rows = listed.as_array().expect("the listing is an array");
    assert_eq!(rows.len(), 1, "one collaborator was added: {listed}");
    assert_eq!(
        rows[0]["username"], "Zubrenok",
        "the list that answers \"who has access\" does not name anyone: {listed}"
    );
    assert_eq!(rows[0]["user_id"], guest_id, "the id stays available too");
    assert_eq!(rows[0]["permission"], "write");

    // The permission change answers in the same shape, so a client that
    // re-renders from the response does not lose the name it just showed.
    let row_id = rows[0]["id"].as_i64().expect("the row carries its own id");
    let updated: serde_json::Value = client
        .patch(format!(
            "{base}/api/v1/repos/atlas-owner/ledger/collaborators/{row_id}"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "permission": "admin" }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(updated["username"], "Zubrenok", "got: {updated}");
    assert_eq!(updated["permission"], "admin");
}

/// A `display_name`, when the account has one, travels with the row — that is
/// what lets a page show a human's name next to their handle without a second
/// request it has no endpoint for.
#[tokio::test]
async fn a_collaborator_with_a_display_name_carries_it() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "atlas-owner", "atlas-owner@example.com").await;
    let (_, guest_id) = register_full(&base, "kestrel", "kestrel@example.com").await;
    let client = reqwest::Client::new();

    create_repo(&base, &owner_token, "ledger").await;

    // Registration takes a username and an e-mail; the display name is set
    // directly because this instance has no self-service profile endpoint —
    // which is also part of why a listing has to carry the name it knows.
    let backend = db.get_database_backend();
    let sql = rg_db::prepare_sql(backend, "UPDATE users SET display_name = ? WHERE id = ?");
    db.execute(sea_orm::Statement::from_sql_and_values(
        backend,
        &sql,
        [Value::from("Kes Trel".to_string()), Value::from(guest_id)],
    ))
    .await
    .expect("name the guest account");

    client
        .post(format!(
            "{base}/api/v1/repos/atlas-owner/ledger/collaborators"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "username": "kestrel", "permission": "read" }))
        .send()
        .await
        .expect("request");

    let listed: serde_json::Value = client
        .get(format!(
            "{base}/api/v1/repos/atlas-owner/ledger/collaborators"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(listed[0]["username"], "kestrel", "got: {listed}");
    assert_eq!(listed[0]["display_name"], "Kes Trel", "got: {listed}");
}
