//! card_a156a521ca3b: `null` must clear a field, and absence must not.
//!
//! Every test here asserts the same three states on one field — set it, clear
//! it with an explicit `null`, then send a body that omits it — and that
//! grouping is the whole point. The defect was two of those three collapsing
//! into one value at the deserializer, which no single-state test can see: a
//! test that only sends `null` and checks the field is unchanged passes both
//! before and after the fix, because "unchanged" was the bug.
//!
//! The service layer under these endpoints was always written to clear on
//! `Some(None)`. What was missing was any way for a request to produce it.

use crate::common::{create_repo, register_full, spawn_test_app, spawn_test_app_with_db};

/// Set the assignee, clear it with `null`, then prove an absent key is not the
/// same thing as `null` — a PATCH about the title must leave the assignee be.
#[tokio::test]
async fn an_issue_assignee_is_set_cleared_by_null_and_untouched_by_absence() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let (token, owner_id) = register_full(&base, "clearowner", "clearowner@example.com").await;
    create_repo(&base, &token, "clearing").await;

    let created = client
        .post(format!("{base}/api/v1/repos/clearowner/clearing/issues"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "needs an owner"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "{}", created.text().await.unwrap());
    let number = 1;
    let url = format!("{base}/api/v1/repos/clearowner/clearing/issues/{number}");

    let patch = |body: serde_json::Value| {
        let client = client.clone();
        let url = url.clone();
        let token = token.clone();
        async move {
            let resp = client
                .patch(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
            resp.json::<serde_json::Value>().await.unwrap()
        }
    };

    let assigned = patch(serde_json::json!({"assignee_id": owner_id})).await;
    assert_eq!(assigned["assignee_id"], owner_id, "a value must set");

    let cleared = patch(serde_json::json!({"assignee_id": null})).await;
    assert!(
        cleared["assignee_id"].is_null(),
        "an explicit null must clear the assignee: {cleared}"
    );

    let reassigned = patch(serde_json::json!({"assignee_id": owner_id})).await;
    assert_eq!(reassigned["assignee_id"], owner_id);
    let untouched = patch(serde_json::json!({"title": "still owned"})).await;
    assert_eq!(
        untouched["assignee_id"], owner_id,
        "a body that never mentions the assignee must not clear it: {untouched}"
    );
    assert_eq!(untouched["title"], "still owned");
}

/// The admin user editor carries the same contract on `display_name` and
/// `bio`. It was the other half of the same defect and reached the same dead
/// end from the opposite direction: the handler wrapped the field with
/// `map(Some)`, which is exactly right — except serde had already folded the
/// explicit `null` into the absent-field `None` one layer above it.
#[tokio::test]
async fn an_admin_clears_a_display_name_with_null_and_not_by_omission() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "clearadmin", "clearadmin@example.com").await;
    let (_, target_id) = register_full(&base, "cleartarget", "cleartarget@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .expect("promote the caller to instance admin")
        .expect("the registered admin must exist");

    let url = format!("{base}/api/v1/admin/users/{target_id}");
    let patch = |body: serde_json::Value| {
        let client = client.clone();
        let url = url.clone();
        let token = admin_token.clone();
        async move {
            let resp = client
                .patch(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
            resp.json::<serde_json::Value>().await.unwrap()
        }
    };

    let named = patch(serde_json::json!({"display_name": "Target Person", "bio": "hello"})).await;
    assert_eq!(named["display_name"], "Target Person");

    let flagged = patch(serde_json::json!({"is_active": true})).await;
    assert_eq!(
        flagged["display_name"], "Target Person",
        "a body about the active flag must not clear the display name: {flagged}"
    );

    let cleared = patch(serde_json::json!({"display_name": null})).await;
    assert!(
        cleared["display_name"].is_null(),
        "an explicit null must clear the display name: {cleared}"
    );
    assert_eq!(
        cleared["bio"], "hello",
        "clearing one field must not clear its neighbour: {cleared}"
    );
}

/// The same three states on the issue's milestone link.
#[tokio::test]
async fn an_issue_milestone_is_set_cleared_by_null_and_untouched_by_absence() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "msowner", "msowner@example.com").await;
    create_repo(&base, &token, "milestoned").await;

    let milestone = client
        .post(format!("{base}/api/v1/repos/msowner/milestoned/milestones"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "v1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        milestone.status(),
        201,
        "{}",
        milestone.text().await.unwrap()
    );
    let milestone_id = milestone.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let created = client
        .post(format!("{base}/api/v1/repos/msowner/milestoned/issues"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "planned"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let url = format!("{base}/api/v1/repos/msowner/milestoned/issues/1");

    let patch = |body: serde_json::Value| {
        let client = client.clone();
        let url = url.clone();
        let token = token.clone();
        async move {
            let resp = client
                .patch(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
            resp.json::<serde_json::Value>().await.unwrap()
        }
    };

    assert_eq!(
        patch(serde_json::json!({"milestone_id": milestone_id})).await["milestone_id"],
        milestone_id
    );
    let cleared = patch(serde_json::json!({"milestone_id": null})).await;
    assert!(
        cleared["milestone_id"].is_null(),
        "an explicit null must take the issue off its milestone: {cleared}"
    );
    assert_eq!(
        patch(serde_json::json!({"milestone_id": milestone_id})).await["milestone_id"],
        milestone_id
    );
    let untouched = patch(serde_json::json!({"title": "still planned"})).await;
    assert_eq!(
        untouched["milestone_id"], milestone_id,
        "a body that never mentions the milestone must not clear it: {untouched}"
    );
}

/// A milestone's own two clearable fields, both in one pass: `description` and
/// `due_date` are independent columns, and a deserializer bug on either one
/// would otherwise hide behind the other.
#[tokio::test]
async fn milestone_description_and_due_date_clear_on_null_only() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "dueowner", "dueowner@example.com").await;
    create_repo(&base, &token, "dated").await;

    let created = client
        .post(format!("{base}/api/v1/repos/dueowner/dated/milestones"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "v1",
            "description": "the first one",
            "due_date": "2030-01-01T00:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "{}", created.text().await.unwrap());
    let body = created.json::<serde_json::Value>().await.unwrap();
    let id = body["id"].as_i64().unwrap();
    assert_eq!(body["description"], "the first one");
    assert!(
        !body["due_date"].is_null(),
        "the fixture must start with a due date: {body}"
    );
    let url = format!("{base}/api/v1/repos/dueowner/dated/milestones/{id}");

    let patch = |body: serde_json::Value| {
        let client = client.clone();
        let url = url.clone();
        let token = token.clone();
        async move {
            let resp = client
                .patch(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
            resp.json::<serde_json::Value>().await.unwrap()
        }
    };

    let renamed = patch(serde_json::json!({"title": "v1.0"})).await;
    assert_eq!(
        renamed["description"], "the first one",
        "a title-only PATCH must not clear the description: {renamed}"
    );
    assert!(
        !renamed["due_date"].is_null(),
        "a title-only PATCH must not clear the due date: {renamed}"
    );

    let cleared = patch(serde_json::json!({"description": null, "due_date": null})).await;
    assert!(
        cleared["description"].is_null(),
        "an explicit null must clear the description: {cleared}"
    );
    assert!(
        cleared["due_date"].is_null(),
        "an explicit null must clear the due date: {cleared}"
    );

    let refilled = patch(serde_json::json!({
        "description": "again",
        "due_date": "2031-02-03T00:00:00Z"
    }))
    .await;
    assert_eq!(refilled["description"], "again");
    assert!(!refilled["due_date"].is_null());
}

/// Detaching a board card from its issue — the same three states, and the one
/// place where "absent" and "null" carry visibly different intent: a PATCH that
/// only edits the note must not silently unlink the card.
#[tokio::test]
async fn a_board_card_is_detached_from_its_issue_by_null_only() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "boardowner", "boardowner@example.com").await;
    create_repo(&base, &token, "boarded").await;

    let issue = client
        .post(format!("{base}/api/v1/repos/boardowner/boarded/issues"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "tracked"}))
        .send()
        .await
        .unwrap();
    assert_eq!(issue.status(), 201);
    let issue_id = issue.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let board = client
        .post(format!("{base}/api/v1/repos/boardowner/boarded/boards"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "work"}))
        .send()
        .await
        .unwrap();
    assert_eq!(board.status(), 201, "{}", board.text().await.unwrap());
    let board_id = board.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let column = client
        .post(format!(
            "{base}/api/v1/repos/boardowner/boarded/boards/{board_id}/columns"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "todo", "position": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(column.status(), 201, "{}", column.text().await.unwrap());
    let column_id = column.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let card = client
        .post(format!(
            "{base}/api/v1/repos/boardowner/boarded/boards/{board_id}/columns/{column_id}/cards"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"issue_id": issue_id, "note": "look at this"}))
        .send()
        .await
        .unwrap();
    assert_eq!(card.status(), 201, "{}", card.text().await.unwrap());
    let card_body = card.json::<serde_json::Value>().await.unwrap();
    let card_id = card_body["id"].as_i64().unwrap();
    assert_eq!(card_body["issue_id"], issue_id);
    let url = format!("{base}/api/v1/repos/boardowner/boarded/boards/{board_id}/cards/{card_id}");

    let patch = |body: serde_json::Value| {
        let client = client.clone();
        let url = url.clone();
        let token = token.clone();
        async move {
            let resp = client
                .patch(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
            resp.json::<serde_json::Value>().await.unwrap()
        }
    };

    let noted = patch(serde_json::json!({"note": "still tracking"})).await;
    assert_eq!(
        noted["issue_id"], issue_id,
        "editing the note must not unlink the card: {noted}"
    );

    let detached = patch(serde_json::json!({"issue_id": null})).await;
    assert!(
        detached["issue_id"].is_null(),
        "an explicit null must detach the card from its issue: {detached}"
    );

    let reattached = patch(serde_json::json!({"issue_id": issue_id})).await;
    assert_eq!(reattached["issue_id"], issue_id);
}
