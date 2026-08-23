//! Editing and deleting a tag-protection rule — the two controls on the
//! repository's "Tag protection" settings screen that no test named.
//!
//! Creating a rule was covered from several directions already (allow-list
//! validation, audit journalling, cross-repository id scoping). What nothing
//! exercised is the rest of a rule's life: the pencil that rewrites who may
//! move a protected tag, and the bin that removes the rule entirely. Both are
//! `RepoAdmin`, both change who can rewrite a released version's tag, and
//! neither had an assertion behind it — so a `PATCH` that reports `200` and
//! persists nothing, or a `DELETE` that answers `204` over a rule that stays,
//! would read as success in the UI and leave the repository protected by
//! something other than what the screen shows.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

const OWNER: &str = "tagline-owner";
const REPO: &str = "tagline-repo";

/// The rules this repository currently has, as the settings screen reads them.
async fn list_rules(base: &str, token: &str) -> Vec<serde_json::Value> {
    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/tagline-owner/tagline-repo/tags/protection"
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("list tag protection rules");
    assert_eq!(response.status(), 200);
    response.json().await.expect("the list decodes")
}

#[tokio::test]
async fn a_tag_rule_can_be_rewritten_and_removed() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) = register_full(&base, OWNER, "tagline-owner@example.com").await;
    let (_releaser_token, releaser_id) =
        register_full(&base, "tagline-releaser", "tagline-releaser@example.com").await;
    let (outsider_token, _outsider_id) =
        register_full(&base, "tagline-outsider", "tagline-outsider@example.com").await;
    create_repo(&base, &owner_token, REPO).await;

    let created = client
        .post(format!(
            "{base}/api/v1/repos/tagline-owner/tagline-repo/tags/protection"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"pattern": "v*", "allowed_user_ids": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "baseline: the rule exists");
    let rule: serde_json::Value = created.json().await.unwrap();
    let rule_id = rule["id"].as_i64().expect("the new rule carries an id");
    assert!(
        rule["allowed_user_ids"].as_array().unwrap().is_empty(),
        "baseline: the rule starts admitting nobody"
    );

    // The allow-list is the whole content of a `PATCH`, so a body that names
    // neither spelling of it is a request with nothing in it.
    let empty_edit = client
        .patch(format!(
            "{base}/api/v1/repos/tagline-owner/tagline-repo/tags/protection/{rule_id}"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        empty_edit.status(),
        400,
        "an edit that changes nothing must not be reported as an edit"
    );

    let by_outsider = client
        .patch(format!(
            "{base}/api/v1/repos/tagline-owner/tagline-repo/tags/protection/{rule_id}"
        ))
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"allowed_users": ["tagline-outsider"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        by_outsider.status(),
        403,
        "who may move a protected tag is an administrator's decision"
    );

    let edited = client
        .patch(format!(
            "{base}/api/v1/repos/tagline-owner/tagline-repo/tags/protection/{rule_id}"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"allowed_users": ["tagline-releaser"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(edited.status(), 200);
    let edited: serde_json::Value = edited.json().await.unwrap();
    assert_eq!(
        edited["allowed_user_ids"],
        serde_json::json!([releaser_id]),
        "the exception the operator named is the exception that was stored"
    );
    assert_eq!(
        edited["allowed_users"][0]["username"], "tagline-releaser",
        "and it comes back named, for a screen with nowhere to look an id up"
    );

    // Read it back through the listing the settings screen actually uses: a
    // handler that answered from the request body would pass every assertion
    // above and still have written nothing.
    let listed = list_rules(&base, &owner_token).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"].as_i64(), Some(rule_id));
    assert_eq!(
        listed[0]["allowed_user_ids"],
        serde_json::json!([releaser_id])
    );

    let by_outsider = client
        .delete(format!(
            "{base}/api/v1/repos/tagline-owner/tagline-repo/tags/protection/{rule_id}"
        ))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(by_outsider.status(), 403);

    let removed = client
        .delete(format!(
            "{base}/api/v1/repos/tagline-owner/tagline-repo/tags/protection/{rule_id}"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), 204);
    assert!(
        list_rules(&base, &owner_token).await.is_empty(),
        "the rule is gone from the screen that lists it"
    );

    let again = client
        .delete(format!(
            "{base}/api/v1/repos/tagline-owner/tagline-repo/tags/protection/{rule_id}"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        again.status(),
        404,
        "a second delete removed nothing and must say so"
    );
}
