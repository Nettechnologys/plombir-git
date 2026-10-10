use crate::common::{register_user, spawn_test_app_with_db};
use sea_orm::{ActiveModelTrait, Set};

const SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA signing@test";

#[tokio::test]
async fn signing_keys_require_a_verified_email_and_do_not_grant_other_accounts_access() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = register_user(&base, "signowner", "signowner@example.com", "Qz7$wRtm").await;
    let other = register_user(&base, "signother", "signother@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/users/signing-keys");
    let body = serde_json::json!({"title": "Signing laptop", "kind": "ssh", "public_key": SSH_KEY});

    let unverified = client
        .post(&url)
        .bearer_auth(&owner)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(unverified.status(), 403);

    let account = rg_db::ops::user_ops::find_by_username(&db, "signowner")
        .await
        .unwrap()
        .unwrap();
    let mut active: rg_db::entities::user::ActiveModel = account.into();
    active.email_verified_at = Set(Some(chrono::Utc::now()));
    active.update(&db).await.unwrap();

    let created = client
        .post(&url)
        .bearer_auth(&owner)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    assert_eq!(created["kind"], "ssh");
    let id = created["id"].as_i64().unwrap();
    let trusted = rg_db::ops::commit_signing_key_ops::list_active_verified(&db)
        .await
        .unwrap();
    assert_eq!(trusted.len(), 1);
    assert_eq!(trusted[0].1, "signowner@example.com");

    let duplicate = client
        .post(&url)
        .bearer_auth(&owner)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 409);
    let hidden = client
        .delete(format!("{url}/{id}"))
        .bearer_auth(&other)
        .send()
        .await
        .unwrap();
    assert_eq!(hidden.status(), 404);
    let deleted = client
        .delete(format!("{url}/{id}"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
    assert!(
        rg_db::ops::commit_signing_key_ops::list_active_verified(&db)
            .await
            .unwrap()
            .is_empty()
    );
}
