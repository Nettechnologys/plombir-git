//! Maintenance mode, end to end (card_42d82e91dbe3).
//!
//! Two things were wrong and only one of them was visible. The middleware
//! rejected mutating requests with a `200 OK` carrying an error body, so every
//! client saw a success; and `build_test_router` never mounted the layer, so no
//! test could have noticed. The second defect is what kept the first alive —
//! these tests exist so neither can come back quietly.

use crate::common::{register_full, spawn_test_app_with_db};

/// Promote a freshly registered user to instance admin and hand back its token.
async fn admin_token(base: &str, db: &rg_db::DatabaseConnection, name: &str) -> String {
    let (token, id) = register_full(base, name, &format!("{name}@example.com")).await;
    rg_db::ops::user_ops::update_by_id(db, id, None, None, Some(true), None)
        .await
        .unwrap();
    token
}

/// Flip maintenance mode through the public admin API and assert it took.
async fn set_maintenance(base: &str, token: &str, on: bool) -> reqwest::Response {
    reqwest::Client::new()
        .patch(format!("{base}/api/v1/admin/settings"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "maintenance_mode": on }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn maintenance_mode_rejects_a_mutating_request_with_503() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    // Registration is itself a POST, so every account this test needs has to
    // exist before the gate closes.
    let admin = admin_token(&base, &db, "maint_503_admin").await;
    let (user, user_id) =
        register_full(&base, "maint_503_user", "maint_503_user@example.com").await;

    assert_eq!(set_maintenance(&base, &admin, true).await.status(), 200);

    let resp = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&user)
        .json(&serde_json::json!({"name": "blocked-by-maintenance"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        503,
        "a request the instance refused to serve must not answer 2xx"
    );
    assert!(
        resp.headers().contains_key("retry-after"),
        "a 503 from a transient, self-clearing condition should tell the client when to come back"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "MAINTENANCE_MODE");

    // The repo really was not created — the rejection happened before the
    // handler, not after it.
    assert!(
        rg_db::ops::repo_ops::find_by_owner_and_name(&db, user_id, "blocked-by-maintenance")
            .await
            .unwrap()
            .is_none(),
        "the request was rejected but the write still landed"
    );
}

#[tokio::test]
async fn maintenance_mode_still_serves_reads_and_the_admin_api() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let admin = admin_token(&base, &db, "maint_read_admin").await;
    let (user, _) = register_full(&base, "maint_read_user", "maint_read_user@example.com").await;
    crate::common::create_repo(&base, &user, "readable-in-maintenance").await;

    assert_eq!(set_maintenance(&base, &admin, true).await.status(), 200);

    // Safe method: read-only maintenance is still read.
    let read = client
        .get(format!(
            "{base}/api/v1/repos/maint_read_user/readable-in-maintenance"
        ))
        .bearer_auth(&user)
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), 200, "maintenance mode blocked a read");

    // The admin API is the way out of the mode, so it cannot be blocked by it —
    // a mutating admin request has to go through even now.
    assert_eq!(
        set_maintenance(&base, &admin, false).await.status(),
        200,
        "maintenance mode locked out the only API that can turn it off"
    );

    // And with the mode off, the ordinary write path is open again.
    let resp = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&user)
        .json(&serde_json::json!({"name": "after-maintenance"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

/// The layer has to be mounted in the router the other tests drive, not just in
/// the production one — that gap is what hid the 200-instead-of-503 for as long
/// as it lived. This asserts the mounting itself, independently of any handler.
#[tokio::test]
async fn the_test_router_carries_the_maintenance_layer() {
    let (base, db) = spawn_test_app_with_db().await;
    let admin = admin_token(&base, &db, "maint_layer_admin").await;

    // An unauthenticated POST to a route no handler claims: whatever the router
    // answers on its own, it is not a 503 — so a 503 here can only come from a
    // gate mounted in front of the whole table.
    let path = format!("{base}/api/v1/there-is-no-such-route");
    let before = reqwest::Client::new().post(&path).send().await.unwrap();
    assert_ne!(before.status(), 503);

    assert_eq!(set_maintenance(&base, &admin, true).await.status(), 200);

    let after = reqwest::Client::new().post(&path).send().await.unwrap();
    assert_eq!(
        after.status(),
        503,
        "the maintenance layer is not mounted in the test router"
    );
}
