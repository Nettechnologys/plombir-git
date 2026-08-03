//! card_cfba32a77acd: "that name is already taken" is a `409`, not a `400`.
//!
//! Every route here answered `400 Bad Request` to a request that was correct in
//! every part the caller controls — the name was well-formed, the body parsed,
//! the rights were there — and was refused by a row that already exists.
//! Editing the request cannot fix that; picking another name, or removing what
//! holds the current one, can. `400` sent the caller looking for a mistake they
//! had not made, and it made the duplicate indistinguishable from the genuinely
//! malformed bodies these same handlers answer.
//!
//! Each test carries both halves, because either one alone proves nothing: a
//! handler that `409`s everything passes the first, and a handler that never
//! changed passes the second.
//!
//! The race-loser half of the same claim lives in
//! [`super::service_failure_status_sweep_tests`] (deterministic UNIQUE
//! injectors for organizations, branch protection and SSO providers) and in
//! `rg-core/tests/create_unique_race_tests.rs` — a loser that answered
//! differently from a sequential duplicate would turn the status code into a
//! side channel for "you lost the race".

use crate::common::{register_full, spawn_test_app, spawn_test_app_with_db};

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn create_repo(base: &str, token: &str, body: serde_json::Value) -> u16 {
    client()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

// ── repositories ────────────────────────────────────────────────────────────

/// The two answers `POST /repos` has to keep apart: a name this account already
/// uses, and a name no account could ever use.
#[tokio::test]
async fn a_taken_repository_name_is_a_conflict_and_a_malformed_one_is_still_a_bad_request() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "dupe-repo", "dupe-repo@example.test").await;

    assert_eq!(
        create_repo(&base, &token, serde_json::json!({"name": "widgets"})).await,
        201,
        "baseline: the first repository is created"
    );
    assert_eq!(
        create_repo(&base, &token, serde_json::json!({"name": "widgets"})).await,
        409,
        "a name this account already uses is refused by the repository that holds it"
    );
    assert_eq!(
        create_repo(&base, &token, serde_json::json!({"name": "wid/gets"})).await,
        400,
        "a name with an invalid character is still the request's own fault"
    );
}

/// `POST /repos/{owner}/{name}/transfer` — the destination namespace already
/// holds that name. The transfer is refused, and the repository stays where it
/// was: a `409` that moved the row anyway would be worse than the `400`.
#[tokio::test]
async fn a_transfer_onto_a_taken_name_is_a_conflict_and_leaves_the_repository_alone() {
    let base = spawn_test_app().await;
    let (owner, _) = register_full(&base, "dupe-src", "dupe-src@example.test").await;

    assert_eq!(
        create_repo(&base, &owner, serde_json::json!({"name": "movable"})).await,
        201
    );
    // The organization is the destination namespace, and it is given the same
    // name up front so the transfer has something to collide with.
    assert_eq!(
        client()
            .post(format!("{base}/api/v1/orgs"))
            .bearer_auth(&owner)
            .json(&serde_json::json!({"name": "dupecorp", "visibility": "public"}))
            .send()
            .await
            .expect("request")
            .status()
            .as_u16(),
        201,
        "baseline: the destination organization exists"
    );
    assert_eq!(
        create_repo(
            &base,
            &owner,
            serde_json::json!({"name": "movable", "org": "dupecorp"})
        )
        .await,
        201,
        "baseline: the destination already holds the name"
    );

    let refused = client()
        .post(format!("{base}/api/v1/repos/dupe-src/movable/transfer"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"new_owner": "dupecorp"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        refused.status(),
        409,
        "a destination that already holds the name refuses the transfer"
    );

    let still_home = client()
        .get(format!("{base}/api/v1/repos/dupe-src/movable"))
        .bearer_auth(&owner)
        .send()
        .await
        .expect("request");
    assert_eq!(
        still_home.status(),
        200,
        "the refused transfer must leave the repository in its own namespace"
    );
}

// ── organizations ───────────────────────────────────────────────────────────

/// `POST /orgs` — a taken name against a `visibility` that is not one of the
/// two the route accepts. The second is the request's shape and stays a `400`;
/// `validate_username` owns everything else the caller can get wrong.
#[tokio::test]
async fn a_taken_organization_name_is_a_conflict_and_a_bad_visibility_is_still_a_bad_request() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "dupe-org", "dupe-org@example.test").await;

    let create = |body: serde_json::Value| {
        client()
            .post(format!("{base}/api/v1/orgs"))
            .bearer_auth(&token)
            .json(&body)
            .send()
    };

    assert_eq!(
        create(serde_json::json!({"name": "acmedupe", "visibility": "public"}))
            .await
            .expect("request")
            .status(),
        201,
        "baseline: the first organization is created"
    );

    let taken = create(serde_json::json!({"name": "acmedupe", "visibility": "public"}))
        .await
        .expect("request");
    assert_eq!(
        taken.status(),
        409,
        "an organization name that is taken is refused by the organization holding it"
    );
    let body: serde_json::Value = taken.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "organization name 'acmedupe' is already taken",
        "the refusal keeps its wording, and carries no database detail"
    );

    assert_eq!(
        create(serde_json::json!({"name": "acmeother", "visibility": "secret"}))
            .await
            .expect("request")
            .status(),
        400,
        "a visibility outside the two allowed values is still the request's own fault"
    );
}

// ── collaborators ───────────────────────────────────────────────────────────

/// `POST .../collaborators` — the same user twice, against a permission that
/// does not exist. Both used to be `400`; only the second one is.
#[tokio::test]
async fn a_repeated_collaborator_is_a_conflict_and_an_unknown_permission_is_still_a_bad_request() {
    let base = spawn_test_app().await;
    let (owner, _) = register_full(&base, "dupe-owner", "dupe-owner@example.test").await;
    let (_, _friend_id) = register_full(&base, "dupe-friend", "dupe-friend@example.test").await;
    assert_eq!(
        create_repo(&base, &owner, serde_json::json!({"name": "shared"})).await,
        201
    );

    let add = |body: serde_json::Value| {
        client()
            .post(format!(
                "{base}/api/v1/repos/dupe-owner/shared/collaborators"
            ))
            .bearer_auth(&owner)
            .json(&body)
            .send()
    };

    assert_eq!(
        add(serde_json::json!({"username": "dupe-friend", "permission": "write"}))
            .await
            .expect("request")
            .status(),
        201,
        "baseline: the first invitation is accepted"
    );
    assert_eq!(
        add(serde_json::json!({"username": "dupe-friend", "permission": "read"}))
            .await
            .expect("request")
            .status(),
        409,
        "a user who is already a collaborator is refused by the membership that exists"
    );
    assert_eq!(
        add(serde_json::json!({"username": "dupe-friend", "permission": "root"}))
            .await
            .expect("request")
            .status(),
        400,
        "a permission outside read/write/admin is still the request's own fault"
    );
}

// ── SSO providers ───────────────────────────────────────────────────────────

/// `POST` **and** `PATCH /admin/sso/providers` — the slug is unique, and both
/// doors have their own pre-check. The update half is the one a sweep by
/// message literal misses: it is a *rename* onto a slug a different provider
/// holds.
#[tokio::test]
async fn a_taken_sso_slug_is_a_conflict_on_both_the_create_and_the_update_door() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = register_full(&base, "dupe-sso", "dupe-sso@example.test").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .expect("promote test user")
        .expect("registered user must exist");

    let create = |body: serde_json::Value| {
        client()
            .post(format!("{base}/api/v1/admin/sso/providers"))
            .bearer_auth(&token)
            .json(&body)
            .send()
    };

    let first = create(serde_json::json!({
        "name": "First",
        "slug": "dupe-idp",
        "provider_type": "oidc",
        "enabled": false
    }))
    .await
    .expect("request");
    assert_eq!(
        first.status(),
        201,
        "baseline: the first provider is created"
    );

    let taken = create(serde_json::json!({
        "name": "Second",
        "slug": "dupe-idp",
        "provider_type": "oidc",
        "enabled": false
    }))
    .await
    .expect("request");
    assert_eq!(
        taken.status(),
        409,
        "a slug another provider holds is refused by that provider"
    );
    let body: serde_json::Value = taken.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "an SSO provider with slug 'dupe-idp' already exists",
        "the refusal keeps its wording, and carries no constraint text"
    );

    // A second provider under a free slug, so the rename below has somewhere to
    // move from.
    let other = create(serde_json::json!({
        "name": "Other",
        "slug": "other-idp",
        "provider_type": "oidc",
        "enabled": false
    }))
    .await
    .expect("request");
    assert_eq!(other.status(), 201);
    let other_id = other.json::<serde_json::Value>().await.expect("json body")["id"]
        .as_i64()
        .expect("provider id");

    let renamed = client()
        .patch(format!("{base}/api/v1/admin/sso/providers/{other_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "Other",
            "slug": "dupe-idp",
            "provider_type": "oidc",
            "enabled": false
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        renamed.status(),
        409,
        "renaming a provider onto a slug someone else holds is the same refusal"
    );

    // Keeping its own slug is not a collision with itself — the check that
    // makes the case above a 409 must not make an ordinary edit one.
    let edited = client()
        .patch(format!("{base}/api/v1/admin/sso/providers/{other_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "Other renamed",
            "slug": "other-idp",
            "provider_type": "oidc",
            "enabled": false
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        edited.status(),
        200,
        "a provider keeping its own slug must still be editable"
    );
}
