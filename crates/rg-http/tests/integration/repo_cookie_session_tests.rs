//! card_7210b02c0ae9: `DELETE /repos/{owner}/{name}` and
//! `POST /repos/{owner}/{name}/transfer` answered `401` to the owner the gate
//! had just admitted.
//!
//! Both routes are gated by an extractor in the signature — `RepoOwner`, plus
//! `NamespaceCreate` over the transfer's destination — and both then re-derived
//! the caller in the body with `extract_bearer_claims`, purely to put a name in
//! the audit trail. That reader accepts `Authorization: Bearer` and nothing
//! else, while the gate above it resolves the session through `extract_user_id`,
//! which reads the HttpOnly `forgekeep_token` cookie first. So the owner passed
//! the gate and got a `401` out of the handler body.
//!
//! The cookie is not an exotic shape: the web client holds its token in memory
//! only (`web/src/lib/api/_base.svelte.ts`), so from any reloaded tab the cookie
//! is the entire session and the header is absent. Same mechanism as the fork
//! button in `fork_tests::a_cookie_session_may_fork_and_an_anonymous_caller_may_not`.
//!
//! Each test carries its anonymous baseline in the same body: taking the cookie
//! must not mean taking nobody, and a denial proves nothing if the route is
//! broken for everybody. The audit assertion is the other half of the same
//! lines — the actor column used to receive `claims.sub`, a number
//! (card_fcc07f8d1505).

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

/// The actor name the journal recorded for one action of one user.
async fn audit_actor(db: &rg_db::DatabaseConnection, user_id: i64, action: &str) -> Option<String> {
    let (rows, total) = rg_db::ops::audit_log_ops::list_paginated(
        db,
        0,
        10,
        Some(user_id),
        Some(action),
        None,
        None,
        None,
    )
    .await
    .expect("audit log query");
    assert_eq!(
        total, 1,
        "expected exactly one '{action}' entry, got {total}"
    );
    rows.into_iter().next().expect("the entry").username
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

#[tokio::test]
async fn a_cookie_session_owner_may_delete_their_repository_and_an_anonymous_caller_may_not() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "ckdel", "ckdel@example.com").await;
    create_repo(&base, &token, "doomed").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/ckdel/doomed");

    let anonymous = client.delete(&url).send().await.expect("request");
    assert_eq!(
        anonymous.status(),
        401,
        "a delete with no session at all must stay a 401"
    );
    assert!(
        repo_visible_at(&base, &token, "ckdel", "doomed").await,
        "baseline: the refused anonymous delete left the repository in place"
    );

    let resp = client
        .delete(&url)
        .header("cookie", format!("forgekeep_token={token}"))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 200,
        "the cookie session is the same session as the bearer token, got {status} (body: {body})"
    );
    assert_eq!(
        body["deleted"], true,
        "the delete did not report success: {body}"
    );
    assert!(
        !repo_visible_at(&base, &token, "ckdel", "doomed").await,
        "the accepted delete did not actually remove the repository"
    );

    assert_eq!(
        audit_actor(&db, user_id, "repo.delete").await.as_deref(),
        Some("ckdel"),
        "the journal must name the actor, not their numeric id"
    );
}

#[tokio::test]
async fn a_cookie_session_owner_may_transfer_their_repository_and_an_anonymous_caller_may_not() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "cktr", "cktr@example.com").await;
    let (host_token, _) = register_full(&base, "cktrhost", "cktrhost@example.com").await;
    create_repo(&base, &token, "movable").await;

    let client = reqwest::Client::new();

    // The destination is an organization the actor belongs to but does not own,
    // exactly as in `transfer_namespace_tests::an_org_member_transfers_into_the_organization`:
    // an organization's repository is stored under `org.owner_id`, so a transfer
    // into an organization of one's *own* is refused by a collision check that
    // finds the source repository itself (card_92019cc97dcd) — an unrelated
    // defect that would mask the session shape this test is about.
    let status = client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&host_token)
        .json(&serde_json::json!({ "name": "cktrcorp", "visibility": "public" }))
        .send()
        .await
        .expect("request")
        .status();
    assert_eq!(status, 201, "baseline: the destination organization exists");

    let status = client
        .post(format!("{base}/api/v1/orgs/cktrcorp/members"))
        .bearer_auth(&host_token)
        .json(&serde_json::json!({ "user_id": user_id, "role": "member" }))
        .send()
        .await
        .expect("request")
        .status();
    assert_eq!(status, 201, "baseline: the actor is a member of the org");

    let url = format!("{base}/api/v1/repos/cktr/movable/transfer");
    let payload = serde_json::json!({ "new_owner": "cktrcorp" });

    let anonymous = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .expect("request");
    assert_eq!(
        anonymous.status(),
        401,
        "a transfer with no session at all must stay a 401"
    );
    assert!(
        repo_visible_at(&base, &token, "cktr", "movable").await,
        "baseline: the refused anonymous transfer left the repository where it was"
    );

    let resp = client
        .post(&url)
        .header("cookie", format!("forgekeep_token={token}"))
        .json(&payload)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 200,
        "the cookie session is the same session as the bearer token, got {status} (body: {body})"
    );
    assert!(
        repo_visible_at(&base, &token, "cktrcorp", "movable").await,
        "the accepted transfer did not actually move the repository"
    );

    assert_eq!(
        audit_actor(&db, user_id, "repo.transfer").await.as_deref(),
        Some("cktr"),
        "the journal must name the actor, not their numeric id"
    );
}
