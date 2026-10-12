//! Accepted remote values round-trip through HTTP and repository retirement
//! removes the credentials and grants owned by the deleted repository.

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use sea_orm_migration::{MigrationTrait, SchemaManager};

use crate::common::{
    create_repo, register_full, spawn_test_app_over_db, spawn_test_app_with_db, TEST_ENCRYPTION_KEY,
};

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    )
}

fn remote_url(length: usize) -> String {
    let prefix = "https://example.invalid/";
    format!("{prefix}{}", "a".repeat(length - prefix.len()))
}

async fn server_database() -> rg_db::DatabaseConnection {
    let url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must name a disposable database");
    assert!(url.starts_with("postgres://") || url.starts_with("mysql://"));
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to disposable database");
    rg_db::run_migrations(&db).await.expect("migrate database");
    db
}

async fn exercise_limits(base: &str, db: &rg_db::DatabaseConnection) {
    let owner = unique("remotevalues");
    let (token, _) = register_full(base, &owner, &format!("{owner}@example.invalid")).await;
    let repo_id = create_repo(base, &token, "remote").await;
    let client = reqwest::Client::new();
    let mirror_endpoint = format!("{base}/api/v1/repos/{owner}/remote/mirror");
    let max_url = remote_url(rg_core::net::MAX_REMOTE_URL_BYTES);
    let max_username = "u".repeat(rg_core::net::MAX_REMOTE_USERNAME_BYTES);
    let max_password = "p".repeat(rg_core::mirror::service::MAX_MIRROR_PASSWORD_BYTES);

    for body in [
        serde_json::json!({"url": remote_url(rg_core::net::MAX_REMOTE_URL_BYTES + 1)}),
        serde_json::json!({"url": "https://example.invalid/remote.git", "username": "u".repeat(rg_core::net::MAX_REMOTE_USERNAME_BYTES + 1)}),
        serde_json::json!({"url": "https://example.invalid/remote.git", "password": "p".repeat(rg_core::mirror::service::MAX_MIRROR_PASSWORD_BYTES + 1)}),
    ] {
        let response = client
            .post(&mirror_endpoint)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{}", response.text().await.unwrap());
    }

    let response = client
        .post(&mirror_endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": max_url,
            "username": max_username,
            "password": max_password,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, max_url);
    assert_eq!(stored.username.as_deref(), Some(max_username.as_str()));
    let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
    assert_eq!(
        rg_core::auth::encryption::decrypt(stored.password_encrypted.as_deref().unwrap(), &key)
            .unwrap(),
        max_password
    );
    let ciphertext_before_upgrade = stored.password_encrypted.unwrap();
    rg_db::migrations::m20261012_000005_remote_addresses_mysql_text::Migration
        .up(&SchemaManager::new(db))
        .await
        .expect("widen remote columns over a real encrypted mirror credential");
    let ciphertext_after_upgrade = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await
        .unwrap()
        .unwrap()
        .password_encrypted
        .unwrap();
    assert_eq!(ciphertext_after_upgrade, ciphertext_before_upgrade);
    assert_eq!(
        rg_core::auth::encryption::decrypt(&ciphertext_after_upgrade, &key).unwrap(),
        max_password
    );

    let changed_url = remote_url(rg_core::net::MAX_REMOTE_URL_BYTES - 1);
    let changed_username = "v".repeat(rg_core::net::MAX_REMOTE_USERNAME_BYTES);
    let changed_password = "q".repeat(rg_core::mirror::service::MAX_MIRROR_PASSWORD_BYTES);
    let response = client
        .patch(&mirror_endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": changed_url,
            "username": changed_username,
            "password": changed_password,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, changed_url);
    assert_eq!(stored.username.as_deref(), Some(changed_username.as_str()));
    assert_eq!(
        rg_core::auth::encryption::decrypt(stored.password_encrypted.as_deref().unwrap(), &key)
            .unwrap(),
        changed_password
    );

    for body in [
        serde_json::json!({"url": remote_url(rg_core::net::MAX_REMOTE_URL_BYTES + 1)}),
        serde_json::json!({"username": "u".repeat(rg_core::net::MAX_REMOTE_USERNAME_BYTES + 1)}),
        serde_json::json!({"password": "p".repeat(rg_core::mirror::service::MAX_MIRROR_PASSWORD_BYTES + 1)}),
    ] {
        let response = client
            .patch(&mirror_endpoint)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{}", response.text().await.unwrap());
    }
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, changed_url);
    assert_eq!(stored.username.as_deref(), Some(changed_username.as_str()));

    let import_endpoint = format!("{base}/api/v1/imports");
    let import_name = unique("source");
    let response = client
        .post(&import_endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "git",
            "source_url": max_url,
            "target_owner": owner,
            "target_name": import_name,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let task_id = response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let task = rg_db::entities::import_task::Entity::find_by_id(task_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.source_url, max_url);
    let response = client
        .post(&import_endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "git",
            "source_url": remote_url(rg_core::net::MAX_REMOTE_URL_BYTES + 1),
            "target_owner": owner,
            "target_name": unique("rejected"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400, "{}", response.text().await.unwrap());
}

#[tokio::test]
async fn remote_values_fit_sqlite_and_oversize_values_are_requests_errors() {
    let (base, db) = spawn_test_app_with_db().await;
    exercise_limits(&base, &db).await;
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
async fn remote_values_fit_server_database_and_oversize_values_are_request_errors() {
    let db = server_database().await;
    let base = spawn_test_app_over_db(db.clone()).await;
    exercise_limits(&base, &db).await;
}

async fn seed_owned_rows(db: &rg_db::DatabaseConnection, repo_id: i64, label: &str) -> String {
    let now = chrono::Utc::now();
    rg_db::entities::ci_secret::ActiveModel {
        repo_id: Set(repo_id),
        name: Set("DEPLOY_TOKEN".to_string()),
        encrypted_value: Set("sealed".to_string()),
        created_by_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    let fingerprint = format!("SHA256:{label}");
    rg_db::entities::deploy_key::ActiveModel {
        repo_id: Set(repo_id),
        created_by_id: Set(None),
        title: Set("retirement test".to_string()),
        public_key: Set(format!("ssh-ed25519 {label}")),
        fingerprint: Set(fingerprint.clone()),
        read_only: Set(true),
        created_at: Set(now),
        last_used_at: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    rg_db::entities::mirror::ActiveModel {
        repo_id: Set(repo_id),
        url: Set("https://example.invalid/retired.git".to_string()),
        username: Set(Some("bot".to_string())),
        password_encrypted: Set(Some("sealed".to_string())),
        sync_interval_seconds: Set(3600),
        next_sync_at: Set(None),
        last_sync_at: Set(None),
        last_sync_error: Set(None),
        status: Set("inactive".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    fingerprint
}

async fn owned_counts(db: &rg_db::DatabaseConnection, repo_id: i64) -> (u64, u64, u64) {
    let secrets = rg_db::entities::ci_secret::Entity::find()
        .filter(rg_db::entities::ci_secret::Column::RepoId.eq(repo_id))
        .count(db)
        .await
        .unwrap();
    let keys = rg_db::entities::deploy_key::Entity::find()
        .filter(rg_db::entities::deploy_key::Column::RepoId.eq(repo_id))
        .count(db)
        .await
        .unwrap();
    let mirrors = rg_db::entities::mirror::Entity::find()
        .filter(rg_db::entities::mirror::Column::RepoId.eq(repo_id))
        .count(db)
        .await
        .unwrap();
    (secrets, keys, mirrors)
}

async fn exercise_retirement(base: &str, db: &rg_db::DatabaseConnection) {
    let owner = unique("retireconfig");
    let (token, _) = register_full(base, &owner, &format!("{owner}@example.invalid")).await;
    let repo_id = create_repo(base, &token, "retired").await;
    let fingerprint = seed_owned_rows(db, repo_id, &unique("key")).await;
    assert_eq!(owned_counts(db, repo_id).await, (1, 1, 1));

    let client = reqwest::Client::new();
    let response = client
        .delete(format!("{base}/api/v1/repos/{owner}/retired"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(owned_counts(db, repo_id).await, (0, 0, 0));
    assert!(
        rg_db::ops::deploy_key_ops::find_by_fingerprint(db, &fingerprint)
            .await
            .unwrap()
            .is_none()
    );

    let org = unique("retireorg");
    let response = client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": org}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let response = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "inside", "org": org}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let org_repo_id = response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    seed_owned_rows(db, org_repo_id, &unique("orgkey")).await;
    let response = client
        .delete(format!("{base}/api/v1/orgs/{org}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(owned_counts(db, org_repo_id).await, (0, 0, 0));
}

#[tokio::test]
async fn repository_and_organization_delete_retire_owned_credentials() {
    let (base, db) = spawn_test_app_with_db().await;
    exercise_retirement(&base, &db).await;
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
async fn server_database_repository_and_organization_delete_retire_owned_credentials() {
    let db = server_database().await;
    let base = spawn_test_app_over_db(db.clone()).await;
    exercise_retirement(&base, &db).await;
}

#[tokio::test]
async fn failed_owned_row_delete_rolls_back_repository_retirement() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let (base, db) = spawn_test_app_with_db().await;
    let owner = unique("retryconfig");
    let (token, _) = register_full(&base, &owner, &format!("{owner}@example.invalid")).await;
    let repo_id = create_repo(&base, &token, "survivor").await;
    seed_owned_rows(&db, repo_id, &unique("retrykey")).await;
    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        format!(
            "CREATE TRIGGER fail_mirror_retirement BEFORE DELETE ON mirrors \
             WHEN OLD.repo_id = {repo_id} BEGIN SELECT RAISE(ABORT, 'injected mirror failure'); END"
        ),
    ))
    .await
    .unwrap();

    let endpoint = format!("{base}/api/v1/repos/{owner}/survivor");
    let client = reqwest::Client::new();
    let response = client
        .delete(&endpoint)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 500, "{}", response.text().await.unwrap());
    assert_eq!(owned_counts(&db, repo_id).await, (1, 1, 1));
    assert!(rg_db::ops::repo_ops::find_by_id(&db, repo_id)
        .await
        .unwrap()
        .unwrap()
        .deleted_at
        .is_none());

    db.execute_unprepared("DROP TRIGGER fail_mirror_retirement")
        .await
        .unwrap();
    let response = client
        .delete(&endpoint)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(owned_counts(&db, repo_id).await, (0, 0, 0));
}
