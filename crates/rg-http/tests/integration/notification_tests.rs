use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::{ConnectionTrait, Statement};

#[tokio::test]
async fn notification_mutations_require_owner() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "notifowner", "notifowner@example.com").await;
    let (other_token, _other_id) =
        register_full(&base, "notifother", "notifother@example.com").await;
    let client = reqwest::Client::new();

    let notification = rg_db::ops::notification_ops::create_notification(
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

/// The trigger lands the DELETE inside the real conditional UPDATE, after the
/// route has authenticated the owner but before SQLite can change the row. The
/// old read-then-ActiveModel path surfaced `RecordNotUpdated` as a 500 here.
#[tokio::test]
async fn notification_deleted_inside_mark_read_is_a_typed_absent_outcome() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "readraceowner", "readraceowner@example.com").await;
    let (other_token, other_id) =
        register_full(&base, "readraceother", "readraceother@example.com").await;
    let target = rg_db::ops::notification_ops::create_notification(
        &db,
        owner_id,
        "issue",
        "Delete wins the read race",
        None,
        None,
    )
    .await
    .expect("create the notification targeted by the race");
    rg_db::ops::notification_ops::create_notification(
        &db,
        other_id,
        "issue",
        "Another inbox stays private",
        None,
        None,
    )
    .await
    .expect("create another user's notification");

    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_notification_before_mark_read \
             BEFORE UPDATE ON notifications WHEN OLD.id = {} \
             BEGIN DELETE FROM notifications WHERE id = OLD.id; END",
            target.id
        ),
    ))
    .await
    .expect("install the competing notification delete");

    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/notifications/{}/read", base, target.id))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("race mark-read against delete");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a DELETE that wins the read mutation must not become a 500"
    );
    assert_eq!(unread_count(&base, &owner_token).await, 0);
    assert_eq!(
        unread_count(&base, &other_token).await,
        1,
        "the typed absence must reveal nothing about another inbox"
    );
}

/// A bulk read is one scoped UPDATE. Deleting one matching row from a BEFORE
/// UPDATE trigger must neither abort the survivors nor inflate the reported
/// count with the row that disappeared.
#[tokio::test]
async fn mark_all_read_counts_only_rows_that_survive_a_concurrent_delete() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "batchraceowner", "batchraceowner@example.com").await;
    let (other_token, other_id) =
        register_full(&base, "batchraceother", "batchraceother@example.com").await;
    let doomed = rg_db::ops::notification_ops::create_notification(
        &db,
        owner_id,
        "issue",
        "Deleted during the batch",
        None,
        None,
    )
    .await
    .expect("create the notification deleted during the batch");
    rg_db::ops::notification_ops::create_notification(
        &db,
        owner_id,
        "issue",
        "Updated by the batch",
        None,
        None,
    )
    .await
    .expect("create the notification that survives the batch");
    rg_db::ops::notification_ops::create_notification(
        &db,
        other_id,
        "issue",
        "Another inbox stays unread",
        None,
        None,
    )
    .await
    .expect("create another user's notification");

    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_notification_during_mark_all \
             BEFORE UPDATE ON notifications WHEN OLD.id = {} \
             BEGIN DELETE FROM notifications WHERE id = OLD.id; END",
            doomed.id
        ),
    ))
    .await
    .expect("install the competing batch delete");

    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/notifications/mark-all-read"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("race mark-all-read against delete");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["marked_read"],
        1,
        "only the surviving unread row was changed"
    );
    assert_eq!(unread_count(&base, &owner_token).await, 0);
    assert_eq!(
        unread_count(&base, &other_token).await,
        1,
        "the batch UPDATE must remain scoped to its authenticated user"
    );
}

/// The bell in the header polls `unread-count` on every page and "mark all
/// read" empties the panel, so both routes run on every session — and neither
/// was named by a test until now.
///
/// The assertion that matters is the second person: both handlers take the
/// user from the token and nothing in the path, so the only way to notice that
/// one of them dropped the `user_id` filter is to keep a second inbox next
/// door and check it survived the sweep.
#[tokio::test]
async fn unread_count_and_mark_all_read_stay_inside_one_inbox() {
    let (base, db) = spawn_test_app_with_db().await;
    let (mine_token, mine_id) = register_full(&base, "bellmine", "bellmine@example.com").await;
    let (theirs_token, theirs_id) =
        register_full(&base, "belltheirs", "belltheirs@example.com").await;
    let client = reqwest::Client::new();

    for (user_id, title) in [
        (mine_id, "First issue assigned"),
        (mine_id, "Second issue assigned"),
        (theirs_id, "Their issue assigned"),
    ] {
        rg_db::ops::notification_ops::create_notification(&db, user_id, "issue", title, None, None)
            .await
            .unwrap();
    }

    let anonymous = client
        .get(format!("{base}/api/v1/notifications/unread-count"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        anonymous.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the count is per person, so there is no answer without one"
    );

    assert_eq!(unread_count(&base, &mine_token).await, 2);
    assert_eq!(unread_count(&base, &theirs_token).await, 1);

    let anonymous_sweep = client
        .post(format!("{base}/api/v1/notifications/mark-all-read"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        anonymous_sweep.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "an anonymous caller must not be able to empty anybody's panel"
    );

    let swept = client
        .post(format!("{base}/api/v1/notifications/mark-all-read"))
        .bearer_auth(&mine_token)
        .send()
        .await
        .unwrap();
    assert_eq!(swept.status(), reqwest::StatusCode::OK);
    assert_eq!(
        swept.json::<serde_json::Value>().await.unwrap()["marked_read"]
            .as_i64()
            .expect("marked_read is a number"),
        2,
        "the sweep reports the rows it actually touched"
    );

    assert_eq!(unread_count(&base, &mine_token).await, 0);
    assert_eq!(
        unread_count(&base, &theirs_token).await,
        1,
        "one person's sweep must not read another person's mail"
    );

    let repeat = client
        .post(format!("{base}/api/v1/notifications/mark-all-read"))
        .bearer_auth(&mine_token)
        .send()
        .await
        .unwrap();
    assert_eq!(repeat.status(), reqwest::StatusCode::OK);
    assert_eq!(
        repeat.json::<serde_json::Value>().await.unwrap()["marked_read"]
            .as_i64()
            .expect("marked_read is a number"),
        0,
        "a second sweep has nothing left to mark"
    );
}

/// The bell's own reading of one inbox.
async fn unread_count(base: &str, token: &str) -> i64 {
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/notifications/unread-count"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json::<serde_json::Value>().await.unwrap()["unread_count"]
        .as_i64()
        .expect("unread_count is a number")
}
