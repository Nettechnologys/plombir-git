//! Routed contract for every user-id allow-list writer. Missing, inactive and
//! retiring principals must all be rejected as the same client-error class on
//! both create and update, while a live principal remains a positive baseline.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn bad_request_message(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("decode error response");
    assert_eq!(
        status, 400,
        "invalid user grant returned the wrong status: {body}"
    );
    body["error"]["message"]
        .as_str()
        .expect("error response carries a message")
        .to_string()
}

#[tokio::test]
async fn every_grant_endpoint_rejects_unusable_users_and_accepts_a_live_one() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "grant-route-owner", "grant-route-owner@example.com").await;
    let (_live_token, live_id) =
        register_full(&base, "grant-route-live", "grant-route-live@example.com").await;
    let (_inactive_token, inactive_id) = register_full(
        &base,
        "grant-route-inactive",
        "grant-route-inactive@example.com",
    )
    .await;
    let (_retiring_token, retiring_id) = register_full(
        &base,
        "grant-route-retiring",
        "grant-route-retiring@example.com",
    )
    .await;
    rg_db::ops::user_ops::update_by_id(&db, inactive_id, None, None, None, Some(false))
        .await
        .expect("deactivate route fixture")
        .expect("inactive route fixture exists");
    assert!(
        rg_db::ops::user_ops::begin_user_retirement(&db, retiring_id)
            .await
            .expect("retire route fixture")
    );
    let repo_id = create_repo(&base, &owner_token, "grant-route-repo").await;

    let branch_endpoint =
        format!("{base}/api/v1/repos/grant-route-owner/grant-route-repo/branches/protection");
    let tag_endpoint =
        format!("{base}/api/v1/repos/grant-route-owner/grant-route-repo/tags/protection");
    let environment_endpoint =
        format!("{base}/api/v1/repos/grant-route-owner/grant-route-repo/actions/environments");

    for (label, invalid_id, expected) in [
        (
            "missing",
            9_999_999,
            "grant user 9999999 does not exist".to_string(),
        ),
        (
            "inactive",
            inactive_id,
            format!("grant user {inactive_id} is inactive"),
        ),
        (
            "retiring",
            retiring_id,
            format!("grant user {retiring_id} is being retired"),
        ),
    ] {
        let branch_message = bad_request_message(
            client
                .post(&branch_endpoint)
                .bearer_auth(&owner_token)
                .json(&serde_json::json!({
                    "branch_name": format!("{label}-branch"),
                    "allowed_push_user_ids": [invalid_id]
                }))
                .send()
                .await
                .expect("create branch rule with invalid principal"),
        )
        .await;
        let tag_message = bad_request_message(
            client
                .post(&tag_endpoint)
                .bearer_auth(&owner_token)
                .json(&serde_json::json!({
                    "pattern": format!("{label}-*"),
                    "allowed_user_ids": [invalid_id]
                }))
                .send()
                .await
                .expect("create tag rule with invalid principal"),
        )
        .await;
        let environment_message = bad_request_message(
            client
                .post(&environment_endpoint)
                .bearer_auth(&owner_token)
                .json(&serde_json::json!({
                    "name": format!("{label}-environment"),
                    "protected": true,
                    "required_approvals": 1,
                    "allowed_approver_ids": [invalid_id]
                }))
                .send()
                .await
                .expect("create environment with invalid principal"),
        )
        .await;
        assert_eq!(branch_message, expected);
        assert_eq!(tag_message, expected);
        assert_eq!(environment_message, expected);
    }

    let branch = client
        .post(&branch_endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_pr": true,
            "allowed_push_user_ids": [live_id]
        }))
        .send()
        .await
        .expect("create live branch grant");
    assert_eq!(branch.status(), 201);
    let branch_id = branch.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let tag = client
        .post(&tag_endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "pattern": "v*",
            "allowed_user_ids": [live_id]
        }))
        .send()
        .await
        .expect("create live tag grant");
    assert_eq!(tag.status(), 201);
    let tag_id = tag.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let environment = client
        .post(&environment_endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 1,
            "allowed_approver_ids": [live_id]
        }))
        .send()
        .await
        .expect("create live environment grant");
    assert_eq!(environment.status(), 201);
    let environment_id = environment.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    for (invalid_id, expected) in [
        (9_999_999, "grant user 9999999 does not exist".to_string()),
        (inactive_id, format!("grant user {inactive_id} is inactive")),
        (
            retiring_id,
            format!("grant user {retiring_id} is being retired"),
        ),
    ] {
        let branch_message = bad_request_message(
            client
                .patch(format!("{branch_endpoint}/{branch_id}"))
                .bearer_auth(&owner_token)
                .json(&serde_json::json!({"allowed_push_user_ids": [invalid_id]}))
                .send()
                .await
                .expect("update branch rule with invalid principal"),
        )
        .await;
        let tag_message = bad_request_message(
            client
                .patch(format!("{tag_endpoint}/{tag_id}"))
                .bearer_auth(&owner_token)
                .json(&serde_json::json!({"allowed_user_ids": [invalid_id]}))
                .send()
                .await
                .expect("update tag rule with invalid principal"),
        )
        .await;
        let environment_message = bad_request_message(
            client
                .put(format!("{environment_endpoint}/{environment_id}"))
                .bearer_auth(&owner_token)
                .json(&serde_json::json!({
                    "name": "production",
                    "protected": true,
                    "required_approvals": 1,
                    "allowed_approver_ids": [invalid_id]
                }))
                .send()
                .await
                .expect("update environment with invalid principal"),
        )
        .await;
        assert_eq!(branch_message, expected);
        assert_eq!(tag_message, expected);
        assert_eq!(environment_message, expected);
    }

    let branches = rg_db::ops::protected_branch_ops::list_rules_by_repo(&db, repo_id)
        .await
        .expect("load live branch baseline");
    let tags = rg_db::ops::protected_tag_ops::list_rules_by_repo(&db, repo_id)
        .await
        .expect("load live tag baseline");
    let environment = rg_db::ops::ci_environment_ops::find_by_id(&db, environment_id)
        .await
        .expect("load live environment baseline")
        .expect("environment exists");
    assert_eq!(branches[0].allowed_push_user_ids, vec![live_id]);
    assert_eq!(tags[0].allowed_user_ids, vec![live_id]);
    assert_eq!(
        rg_db::ops::ci_environment_ops::allowed_approver_ids(&db, &environment)
            .await
            .expect("load live approver baseline"),
        vec![live_id]
    );
}
