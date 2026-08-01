use crate::common::{register_full, spawn_test_app_with_db, TEST_ENCRYPTION_KEY};

async fn create_repo(base: &str, token: &str, name: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
}

#[tokio::test]
async fn ci_secrets_are_admin_only_encrypted_and_never_return_values() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "secret-owner", "secret-owner@example.com").await;
    let (writer_token, _) =
        register_full(&base, "secret-writer", "secret-writer@example.com").await;
    create_repo(&base, &owner_token, "vault").await;
    let client = reqwest::Client::new();
    let collaborator = client
        .post(format!(
            "{base}/api/v1/repos/secret-owner/vault/collaborators"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username":"secret-writer","permission":"write"}))
        .send()
        .await
        .unwrap();
    assert_eq!(collaborator.status(), 201);

    let endpoint = format!("{base}/api/v1/repos/secret-owner/vault/actions/secrets/DEPLOY_TOKEN");
    let denied = client
        .put(&endpoint)
        .bearer_auth(&writer_token)
        .json(&serde_json::json!({"value":"plain-secret-value"}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let created = client
        .put(&endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"value":"plain-secret-value"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let payload = created.text().await.unwrap();
    assert!(!payload.contains("plain-secret-value"));
    assert!(!payload.contains("encrypted_value"));

    let repo = rg_core::repo::service::find_repo_by_owner_name(&db, "secret-owner", "vault")
        .await
        .unwrap()
        .unwrap();
    let stored = rg_db::ops::ci_secret_ops::find_by_repo_and_name(&db, repo.id, "DEPLOY_TOKEN")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(stored.encrypted_value, "plain-secret-value");
    let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
    assert_eq!(
        rg_core::auth::encryption::decrypt(&stored.encrypted_value, &key).unwrap(),
        "plain-secret-value"
    );

    let listed = client
        .get(format!(
            "{base}/api/v1/repos/secret-owner/vault/actions/secrets"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let payload = listed.text().await.unwrap();
    assert!(payload.contains("DEPLOY_TOKEN"));
    assert!(!payload.contains("plain-secret-value"));
}

#[tokio::test]
async fn tag_protection_crud_is_repo_scoped_and_admin_only() {
    let (base, _) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "tag-owner", "tag-owner@example.com").await;
    let (other_token, _) = register_full(&base, "tag-other", "tag-other@example.com").await;
    create_repo(&base, &owner_token, "releases").await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/repos/tag-owner/releases/tags/protection");
    let denied = client
        .post(&endpoint)
        .bearer_auth(&other_token)
        .json(&serde_json::json!({"pattern":"v*"}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let created = client
        .post(&endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"pattern":"v*","allowed_user_ids":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let listed = client
        .get(&endpoint)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    assert_eq!(
        listed.json::<Vec<serde_json::Value>>().await.unwrap().len(),
        1
    );
    let deleted = client
        .delete(format!("{endpoint}/{id}"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
}

#[tokio::test]
async fn branch_protection_persists_signed_commit_requirement() {
    let (base, _) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "signed-owner", "signed-owner@example.com").await;
    create_repo(&base, &owner_token, "signed").await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/repos/signed-owner/signed/branches/protection");
    let created = client
        .post(&endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_signed_commits": true,
            "allow_force_push": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    assert_eq!(
        created.json::<serde_json::Value>().await.unwrap()["require_signed_commits"],
        true
    );
    let listed = client
        .get(&endpoint)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    assert_eq!(
        listed.json::<Vec<serde_json::Value>>().await.unwrap()[0]["require_signed_commits"],
        true
    );
}

/// A `PATCH` on a protection rule used to answer `200` with the row echoed back
/// and write nothing: the edits were applied to the `Model`, and the
/// `ActiveModel` built from it afterwards carried every field as `Unchanged`,
/// so the `UPDATE` had no `SET` clause. The operator who just turned on
/// `require_signed_commits` read that `200` as "the branch is protected now"
/// while `check_push_allowed` kept enforcing the old rule — so the assertion
/// that matters is the re-read, not the status code.
#[tokio::test]
async fn branch_protection_patch_actually_writes_the_new_rule() {
    let (base, _) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "bpatch-owner", "bpatch-owner@example.com").await;
    create_repo(&base, &owner_token, "bpatch").await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/repos/bpatch-owner/bpatch/branches/protection");

    let created = client
        .post(&endpoint)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_pr": false,
            "require_signed_commits": false,
            // Left out of the PATCH below, so they double as the check that a
            // partial update does not clobber what it was not asked to touch.
            "require_status_check": true,
            "require_approval": true,
            "required_approvals": 2
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    let rule_id = created["id"].as_i64().expect("created rule carries an id");
    let created_updated_at = created["updated_at"].as_str().unwrap().to_string();

    let patched = client
        .patch(format!("{endpoint}/{rule_id}"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "require_pr": true,
            "require_signed_commits": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), 200);
    let patched: serde_json::Value = patched.json().await.unwrap();
    assert_eq!(
        patched["require_pr"], true,
        "the PATCH response still echoed the pre-update rule: {patched}"
    );

    let reread = client
        .get(format!("{endpoint}/{rule_id}"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(reread.status(), 200);
    let reread: serde_json::Value = reread.json().await.unwrap();
    assert_eq!(
        reread["require_pr"], true,
        "the 200 was a no-op — require_pr was never written: {reread}"
    );
    assert_eq!(
        reread["require_signed_commits"], true,
        "the 200 was a no-op — require_signed_commits was never written: {reread}"
    );
    assert_ne!(
        reread["updated_at"].as_str().unwrap(),
        created_updated_at,
        "updated_at did not move, so no UPDATE reached the row: {reread}"
    );

    assert_eq!(
        reread["require_status_check"], true,
        "a field absent from the PATCH body was clobbered: {reread}"
    );
    assert_eq!(
        reread["require_approval"], true,
        "a field absent from the PATCH body was clobbered: {reread}"
    );
    assert_eq!(
        reread["required_approvals"], 2,
        "a field absent from the PATCH body was clobbered: {reread}"
    );
}
