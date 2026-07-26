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

/// The five star/watch calls, as (label, request-builder) pairs.
async fn star_watch_statuses(base: &str, owner: &str, repo: &str, token: Option<&str>) -> Vec<u16> {
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/{owner}/{repo}");
    let requests = vec![
        ("PUT star", client.put(format!("{url}/star"))),
        ("GET starred", client.get(format!("{url}/starred"))),
        ("GET watch", client.get(format!("{url}/watch"))),
        (
            "PUT watch",
            client
                .put(format!("{url}/watch"))
                .json(&serde_json::json!({"state": "watching"})),
        ),
        ("DELETE watch", client.delete(format!("{url}/watch"))),
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
        repo_id,
        owner,
        "New push to watchrevokerepo",
        "push",
        Some(format!("{owner} pushed to secret-branch")),
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
