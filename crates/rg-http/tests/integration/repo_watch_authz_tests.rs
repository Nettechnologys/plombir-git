//! Regression coverage for card_bcd996893337: starring and watching a private
//! repository must require read access, and the watch fan-out must re-check it.
//!
//! Five handlers in `api/repos.rs` authenticated the caller and resolved the
//! repository, then acted — `can_read_repo` was never consulted, while the
//! neighbouring `get_stargazers` was already behind the shared gate. That let
//! any logged-in account `PUT /repos/alice/secret/watch` and, from then on,
//! passively receive notifications carrying the private repo's content: PR
//! titles, pusher and branch names, milestone titles.
//!
//! The gate on the endpoint is only half of it. A watch row outlives the access
//! that created it — the repo can be flipped to private, or a collaborator
//! removed — so `notification::notify_watchers` re-checks every recipient at
//! delivery time. Both halves are pinned here, plus the public-repo path, or
//! the fix has traded a leak for an outage.

use crate::common::{register_full, register_user, spawn_test_app_with_db};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

const PW: &str = "Qz7$wRtm";

/// Create a repository with an explicit `is_private`; returns its id.
async fn create_repo_with_visibility(base: &str, token: &str, name: &str, private: bool) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": private}))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "create_repo({name}) should succeed");
    resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("repo id")
}

#[tokio::test]
async fn repository_cascade_during_existing_watch_update_is_typed_not_found() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "watch-repo-race-owner";
    let repo = "watch-repo-race";
    let token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let repo_id = create_repo_with_visibility(&base, &token, repo, false).await;
    let endpoint = format!("{base}/api/v1/repos/{owner}/{repo}/watch");
    let client = reqwest::Client::new();

    let created = client
        .put(&endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "watching"}))
        .send()
        .await
        .expect("create the watch the raced request observes");
    assert_eq!(created.status(), 200);
    let watch = rg_db::entities::repo_watch::Entity::find()
        .filter(
            rg_db::entities::repo_watch::Column::UserId.eq(rg_db::ops::user_ops::find_by_username(
                &db, owner,
            )
            .await
            .expect("read the owner")
            .expect("the owner exists")
            .id),
        )
        .filter(rg_db::entities::repo_watch::Column::RepoId.eq(repo_id))
        .one(&db)
        .await
        .expect("read the watch")
        .expect("the watch exists");
    db.execute(sea_orm::Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_watch_repo_before_http_update \
             BEFORE UPDATE ON repo_watches WHEN OLD.id = {} \
             BEGIN DELETE FROM repositories WHERE id = OLD.repo_id; END",
            watch.id
        ),
    ))
    .await
    .expect("install the competing repository delete");

    let raced = client
        .put(&endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "ignoring"}))
        .send()
        .await
        .expect("race watch update against repository deletion");
    assert_eq!(raced.status(), 404);
    let body = raced.text().await.expect("read the typed error body");
    assert!(body.contains("repository not found"), "{body}");
    assert!(!body.contains("RecordNotUpdated"), "{body}");
    assert!(rg_db::ops::repo_ops::find_by_id(&db, repo_id)
        .await
        .expect("look for the deleted repository")
        .is_none());
    assert!(
        rg_db::ops::repo_watch_ops::get_watch_state(&db, watch.user_id, repo_id)
            .await
            .expect("look for a resurrected watch")
            .is_none(),
        "the losing request recreated a watch after repository deletion"
    );
}

#[tokio::test]
async fn user_cascade_during_existing_watch_update_is_typed_not_found() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "watch-user-race-owner";
    let watcher = "watch-user-race-watcher";
    let repo = "watch-user-race";
    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let (watcher_token, watcher_id) =
        register_full(&base, watcher, &format!("{watcher}@example.com")).await;
    let repo_id = create_repo_with_visibility(&base, &owner_token, repo, false).await;
    let endpoint = format!("{base}/api/v1/repos/{owner}/{repo}/watch");
    let client = reqwest::Client::new();

    let created = client
        .put(&endpoint)
        .bearer_auth(&watcher_token)
        .json(&serde_json::json!({"state": "watching"}))
        .send()
        .await
        .expect("create the watch the raced request observes");
    assert_eq!(created.status(), 200);
    let watch = rg_db::entities::repo_watch::Entity::find()
        .filter(rg_db::entities::repo_watch::Column::UserId.eq(watcher_id))
        .filter(rg_db::entities::repo_watch::Column::RepoId.eq(repo_id))
        .one(&db)
        .await
        .expect("read the watch")
        .expect("the watch exists");
    db.execute(sea_orm::Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_watch_user_before_http_update \
             BEFORE UPDATE ON repo_watches WHEN OLD.id = {} \
             BEGIN DELETE FROM users WHERE id = OLD.user_id; END",
            watch.id
        ),
    ))
    .await
    .expect("install the competing user delete");

    let raced = client
        .put(&endpoint)
        .bearer_auth(&watcher_token)
        .json(&serde_json::json!({"state": "ignoring"}))
        .send()
        .await
        .expect("race watch update against user deletion");
    assert_eq!(raced.status(), 404);
    let body = raced.text().await.expect("read the typed error body");
    assert!(body.contains("user not found"), "{body}");
    assert!(!body.contains("RecordNotUpdated"), "{body}");
    assert!(rg_db::ops::user_ops::find_by_id(&db, watcher_id)
        .await
        .expect("look for the deleted watcher")
        .is_none());
    assert!(
        rg_db::ops::repo_ops::find_by_id(&db, repo_id)
            .await
            .expect("look for the surviving repository")
            .is_some(),
        "deleting a watcher must not delete somebody else's repository"
    );
    assert!(
        rg_db::ops::repo_watch_ops::get_watch_state(&db, watcher_id, repo_id)
            .await
            .expect("look for a resurrected watch")
            .is_none(),
        "the losing request recreated a watch after user deletion"
    );
}

/// The five star/watch calls, as (label, request-builder) pairs.
async fn star_watch_statuses(base: &str, owner: &str, repo: &str, token: Option<&str>) -> Vec<u16> {
    let client = reqwest::Client::new();
    let requests = vec![
        (
            "PUT star",
            client.put(format!("{base}/api/v1/repos/{owner}/{repo}/star")),
        ),
        (
            "GET starred",
            client.get(format!("{base}/api/v1/repos/{owner}/{repo}/starred")),
        ),
        (
            "GET watch",
            client.get(format!("{base}/api/v1/repos/{owner}/{repo}/watch")),
        ),
        (
            "PUT watch",
            client
                .put(format!("{base}/api/v1/repos/{owner}/{repo}/watch"))
                .json(&serde_json::json!({"state": "watching"})),
        ),
        (
            "DELETE watch",
            client.delete(format!("{base}/api/v1/repos/{owner}/{repo}/watch")),
        ),
    ];

    let mut statuses = Vec::new();
    for (label, mut req) in requests {
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let status = req.send().await.expect("request").status().as_u16();
        statuses.push(status);
        eprintln!("{label} -> {status}");
    }
    statuses
}

#[tokio::test]
async fn private_repo_star_and_watch_reject_anonymous_and_outsiders() {
    let (base, _db) = spawn_test_app_with_db().await;
    let owner = "watchgateowner";
    let outsider = "watchgateoutsider";
    let repo = "watchgatesecret";

    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let outsider_token =
        register_user(&base, outsider, &format!("{outsider}@example.com"), PW).await;
    create_repo_with_visibility(&base, &owner_token, repo, true).await;

    for status in star_watch_statuses(&base, owner, repo, None).await {
        assert_eq!(
            status, 401,
            "anonymous star/watch of a private repo must be 401"
        );
    }
    for status in star_watch_statuses(&base, owner, repo, Some(&outsider_token)).await {
        assert_eq!(
            status, 403,
            "outsider star/watch of a private repo must be 403"
        );
    }
}

#[tokio::test]
async fn owner_and_collaborator_still_star_and_watch_a_private_repo() {
    let (base, _db) = spawn_test_app_with_db().await;
    let owner = "watchkeepowner";
    let collab = "watchkeepcollab";
    let repo = "watchkeepsecret";

    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let collab_token = register_user(&base, collab, &format!("{collab}@example.com"), PW).await;
    create_repo_with_visibility(&base, &owner_token, repo, true).await;

    let added = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username": collab, "permission": "read"}))
        .send()
        .await
        .expect("add collaborator");
    assert!(
        added.status().is_success(),
        "adding a collaborator failed: {}",
        added.status()
    );

    for (label, token) in [("owner", &owner_token), ("collaborator", &collab_token)] {
        for status in star_watch_statuses(&base, owner, repo, Some(token)).await {
            assert_eq!(
                status, 200,
                "{label} must still star/watch their private repo"
            );
        }
    }
}

#[tokio::test]
async fn public_repo_star_and_watch_stay_open_to_any_account() {
    let (base, _db) = spawn_test_app_with_db().await;
    let owner = "watchopenowner";
    let stranger = "watchopenstranger";
    let repo = "watchopenrepo";

    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let stranger_token =
        register_user(&base, stranger, &format!("{stranger}@example.com"), PW).await;
    create_repo_with_visibility(&base, &owner_token, repo, false).await;

    for status in star_watch_statuses(&base, owner, repo, Some(&stranger_token)).await {
        assert_eq!(status, 200, "a public repo must stay starrable/watchable");
    }
    // Anonymous still gets 401 — these five all write or read per-user state.
    for status in star_watch_statuses(&base, owner, repo, None).await {
        assert_eq!(status, 401, "star/watch still needs an account");
    }
}

// ── card_37ef65a84cec: the state itself must be validated ────────────────────
//
// `PUT /watch` wrote `body.state` into `repo_watch.watch_state` verbatim, and
// the delivery side reads that column through an allowlist. So `{"state":
// "wathcing"}` answered `200 OK` with `{"watch_state": "wathcing"}` — the user
// believed they had subscribed and received nothing, for good. A typo must be
// a rejected request, not a silently dead subscription.

/// The stored `watch_state` for (user, repo), read straight from the DB.
async fn stored_watch_state(db: &sea_orm::DatabaseConnection, repo_id: i64) -> Option<String> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    rg_db::entities::repo_watch::Entity::find()
        .filter(rg_db::entities::repo_watch::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .expect("query watch row")
        .map(|row| row.watch_state)
}

/// `PUT /watch` with the given state; returns (status, `watch_state` echoed).
async fn put_watch_state(
    base: &str,
    owner: &str,
    repo: &str,
    token: &str,
    state: &str,
) -> (u16, Option<String>) {
    let resp = reqwest::Client::new()
        .put(format!("{base}/api/v1/repos/{owner}/{repo}/watch"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "state": state }))
        .send()
        .await
        .expect("watch request");
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.expect("json");
    (
        status,
        body["watch_state"].as_str().map(ToString::to_string),
    )
}

#[tokio::test]
async fn an_unknown_watch_state_is_rejected_and_leaves_the_row_untouched() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "watchtypoowner";
    let repo = "watchtyporepo";

    let token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let repo_id = create_repo_with_visibility(&base, &token, repo, false).await;

    // A real subscription first, so the rejection has something to not clobber.
    let (status, echoed) = put_watch_state(&base, owner, repo, &token, "watching").await;
    assert_eq!(status, 200, "subscribing with a legal state must work");
    assert_eq!(echoed.as_deref(), Some("watching"));

    for typo in ["wathcing", "WATCHING", "watching ", "", "subscribed"] {
        let (status, _) = put_watch_state(&base, owner, repo, &token, typo).await;
        assert_eq!(
            status, 400,
            "watch state {typo:?} is not one of the three and must be a 400, \
             not a stored row"
        );
        assert_eq!(
            stored_watch_state(&db, repo_id).await.as_deref(),
            Some("watching"),
            "a rejected state {typo:?} must not have touched the existing row"
        );
    }
}

#[tokio::test]
async fn the_three_legal_watch_states_are_still_accepted() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "watchstatesowner";
    let repo = "watchstatesrepo";

    let token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let repo_id = create_repo_with_visibility(&base, &token, repo, false).await;

    for state in ["watching", "ignoring", "not_watching"] {
        let (status, echoed) = put_watch_state(&base, owner, repo, &token, state).await;
        assert_eq!(status, 200, "{state} is a legal watch state");
        assert_eq!(echoed.as_deref(), Some(state), "response must echo {state}");
        assert_eq!(
            stored_watch_state(&db, repo_id).await.as_deref(),
            Some(state),
            "{state} must be what lands in the row"
        );
    }
}

/// Count the notifications a user can see.
async fn notification_count(base: &str, token: &str) -> usize {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/notifications"))
        .bearer_auth(token)
        .send()
        .await
        .expect("list notifications")
        .json()
        .await
        .expect("json");
    // The endpoint answers either a bare array or a paginated envelope.
    body.as_array()
        .map(|a| a.len())
        .or_else(|| body["data"].as_array().map(|a| a.len()))
        .unwrap_or_else(|| panic!("unexpected notifications payload: {body}"))
}

#[tokio::test]
async fn watch_rows_that_outlive_read_access_stop_receiving_notifications() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "watchrevokeowner";
    let stranger = "watchrevokestranger";
    let collab = "watchrevokecollab";
    let repo = "watchrevokerepo";

    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let (stranger_token, _stranger_id) =
        register_full(&base, stranger, &format!("{stranger}@example.com")).await;
    let (collab_token, _collab_id) =
        register_full(&base, collab, &format!("{collab}@example.com")).await;

    // The repo starts public, so both outsiders may legitimately subscribe.
    let repo_id = create_repo_with_visibility(&base, &owner_token, repo, false).await;
    let added = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username": collab, "permission": "read"}))
        .send()
        .await
        .expect("add collaborator");
    assert!(added.status().is_success());

    for token in [&stranger_token, &collab_token] {
        let watched = reqwest::Client::new()
            .put(format!("{base}/api/v1/repos/{owner}/{repo}/watch"))
            .bearer_auth(token)
            .json(&serde_json::json!({"state": "watching"}))
            .send()
            .await
            .expect("watch");
        assert_eq!(watched.status(), 200, "watching a public repo must work");
    }

    // Now the repository turns private. There is no REST route for this, and
    // that is the point: the watch rows were written while the subscribe was
    // legitimate, so no endpoint gate can retract them.
    use sea_orm::{ActiveModelTrait, ActiveValue, EntityTrait};
    let model = rg_db::entities::repository::Entity::find_by_id(repo_id)
        .one(&db)
        .await
        .expect("find repo")
        .expect("repo exists");
    let mut active: rg_db::entities::repository::ActiveModel = model.into();
    active.is_private = ActiveValue::Set(true);
    active.update(&db).await.expect("privatize repo");

    rg_core::notification::notify_watchers(
        &db,
        &rg_core::notification::WatchEvent {
            repo_id,
            author_name: owner.to_string(),
            title: "New push to watchrevokerepo".to_string(),
            notification_type: "push".to_string(),
            body: Some(format!("{owner} pushed to secret-branch")),
        },
    )
    .await
    .expect("notify watchers");

    assert_eq!(
        notification_count(&base, &stranger_token).await,
        0,
        "a watcher who cannot read the repo must not be notified about it"
    );
    assert_eq!(
        notification_count(&base, &collab_token).await,
        1,
        "a collaborator who can still read the repo must keep being notified"
    );
}
