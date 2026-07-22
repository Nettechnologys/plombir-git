mod common;

use common::{register_full, spawn_test_app_with_db};

#[tokio::test]
async fn notification_mutations_require_owner() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "notifowner", "notifowner@example.com").await;
    let (other_token, _other_id) =
        register_full(&base, "notifother", "notifother@example.com").await;
    let client = reqwest::Client::new();

    let notification = rg_core::notification::notify(
        &db,
        owner_id,
        "issue",
        "Issue assigned",
        Some("You were assigned an issue"),
        None,
    )
    .await
    .unwrap();

    let unauthenticated = client
        .post(format!(
            "{}/api/v1/notifications/{}/read",
            base, notification.id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let cross_user = client
        .post(format!(
            "{}/api/v1/notifications/{}/read",
            base, notification.id
        ))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cross_user.status(), reqwest::StatusCode::NOT_FOUND);

    let owner_mark = client
        .post(format!(
            "{}/api/v1/notifications/{}/read",
            base, notification.id
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_mark.status(), reqwest::StatusCode::OK);

    let cross_delete = client
        .delete(format!("{}/api/v1/notifications/{}", base, notification.id))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cross_delete.status(), reqwest::StatusCode::NOT_FOUND);

    let owner_delete = client
        .delete(format!("{}/api/v1/notifications/{}", base, notification.id))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_delete.status(), reqwest::StatusCode::OK);
}
