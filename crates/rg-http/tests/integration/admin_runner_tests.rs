use crate::common::{register_full, spawn_test_app_with_db};

async fn promote_user_to_admin(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote user to admin")
        .expect("registered user must exist");
}

/// `GET /admin/runners/{id}` was implemented, documented in the published spec,
/// and mounted by nothing — a generated client would call it and get a 404
/// (card_a76ad95240d5). The route table is now the only place that can answer
/// "does this door exist", so the test asks the running server.
///
/// The 404 for an id nothing created is the other half of the argument: without
/// it, a route that answered `200 {}` to everything would pass the first half.
#[tokio::test]
async fn the_documented_single_runner_endpoint_answers_the_admin_who_asks() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "runneradmin", "runneradmin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;

    let registered = client
        .post(format!("{base}/api/v1/runners/register"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "name": "linux-runner-01",
            "labels": ["linux", "x86_64"],
        }))
        .send()
        .await
        .expect("register a runner");
    assert_eq!(registered.status(), 201);
    let runner_id = registered.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .expect("registration must return the new runner's id");

    let found = client
        .get(format!("{base}/api/v1/admin/runners/{runner_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("read the runner back");
    assert_eq!(
        found.status(),
        200,
        "the spec advertises this endpoint — it must be mounted"
    );
    let body: serde_json::Value = found.json().await.unwrap();
    assert_eq!(body["id"], runner_id);
    assert_eq!(body["name"], "linux-runner-01");

    let missing = client
        .get(format!("{base}/api/v1/admin/runners/{}", runner_id + 9_999))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("read a runner that does not exist");
    assert_eq!(
        missing.status(),
        404,
        "an id nothing created must not be answered as if it existed"
    );
}
