//! A webhook and its delivery journal leave together with whatever owns them,
//! and every value the webhook API accepts fits the columns of every backend.
//!
//! - A routed `DELETE` of a repository, organization or instance hook removes
//!   the hook's journal in the same transaction (card_bbf80f673828). Before, the
//!   deliveries — full request payload and receiver response — stayed behind
//!   with no route left to address them.
//! - Deleting a repository retires its repository-scoped hooks with the row
//!   (card_c714cc272445): the signing secret and the journal used to outlive the
//!   repository, and a failure before the commit must leave both for the retry.
//! - The full event canon and a long HTTPS URL are accepted values, and both
//!   outgrew MySQL's `VARCHAR(255)` (card_c16a9cc05033, card_535dbac9dcec).
//!
//! The server-database variants are `#[ignore]`d: they need
//! `PLOMBIR_GIT_TEST_DATABASE_URL` pointing at a disposable PostgreSQL or MySQL.

use sea_orm::{ActiveValue::Set, ConnectionTrait, DatabaseBackend, EntityTrait, Statement};

use crate::common::{create_repo, register_full, spawn_test_app_over_db, spawn_test_app_with_db};

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    )
}

async fn server_database() -> rg_db::DatabaseConnection {
    let database_url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    assert!(database_url.starts_with("postgres://") || database_url.starts_with("mysql://"));
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to disposable server database");
    rg_db::run_migrations(&db)
        .await
        .expect("migrate disposable server database");
    db
}

/// An instance admin: system hooks need one, and the same account owns the
/// repository and the organization.
async fn admin(base: &str, db: &rg_db::DatabaseConnection, username: &str) -> String {
    let (token, id) = register_full(base, username, &format!("{username}@example.invalid")).await;
    rg_db::ops::user_ops::update_by_id(db, id, None, None, Some(true), None)
        .await
        .expect("promote the test account")
        .expect("the registered account exists");
    token
}

async fn create_org(base: &str, token: &str, name: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .expect("create organization");
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
}

async fn create_org_repo(base: &str, token: &str, org: &str, name: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "org": org }))
        .send()
        .await
        .expect("create organization repository");
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// Create a hook through `collection` and return its id.
async fn create_hook(base: &str, token: &str, collection: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/{collection}"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "url": "https://hooks.example.invalid/lifecycle",
            "secret": "a-signing-secret",
            "events": ["push"]
        }))
        .send()
        .await
        .expect("create hook");
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn record_delivery(
    db: &rg_db::DatabaseConnection,
    hook_id: i64,
    label: &str,
) -> anyhow::Result<i64> {
    rg_db::ops::webhook_ops::create_delivery(
        db,
        rg_db::entities::webhook_delivery::ActiveModel {
            webhook_id: Set(hook_id),
            event: Set("push".to_string()),
            delivery_id: Set(format!("{label}-{hook_id}-{}", uuid::Uuid::new_v4())),
            request_payload: Set(Some(r#"{"ref":"refs/heads/main"}"#.to_string())),
            response_body: Set(Some("ok".to_string())),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        },
    )
    .await
    .map(|delivery| delivery.id)
}

async fn hook_exists(db: &rg_db::DatabaseConnection, id: i64) -> bool {
    rg_db::entities::webhook::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("read hook")
        .is_some()
}

async fn delivery_exists(db: &rg_db::DatabaseConnection, id: i64) -> bool {
    rg_db::entities::webhook_delivery::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("read delivery")
        .is_some()
}

async fn delete(base: &str, token: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .delete(format!("{base}/api/v1/{path}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("send DELETE")
}

// ── Manual hook deletion takes its journal ───────────────────────────────

async fn exercise_scoped_hook_delete(base: &str, db: &rg_db::DatabaseConnection) {
    let owner = unique("hookdel");
    let org = unique("hookorg");
    let token = admin(base, db, &owner).await;
    create_repo(base, &token, "hooked").await;
    create_org(base, &token, &org).await;

    for collection in [
        format!("repos/{owner}/hooked/hooks"),
        format!("orgs/{org}/hooks"),
        "admin/hooks".to_string(),
    ] {
        let hook_id = create_hook(base, &token, &collection).await;
        let first = record_delivery(db, hook_id, "first").await.unwrap();
        let second = record_delivery(db, hook_id, "second").await.unwrap();

        let response = delete(base, &token, &format!("{collection}/{hook_id}")).await;
        assert_eq!(
            response.status(),
            200,
            "{collection}: {}",
            response.text().await.unwrap()
        );
        assert!(!hook_exists(db, hook_id).await, "{collection}: hook stayed");
        for delivery in [first, second] {
            assert!(
                !delivery_exists(db, delivery).await,
                "{collection}: delivery {delivery} outlived its hook"
            );
        }
        assert!(
            record_delivery(db, hook_id, "late").await.is_err(),
            "{collection}: a delivery finishing after the delete recreated an orphan"
        );
    }
}

#[tokio::test]
async fn deleting_a_hook_in_any_scope_removes_its_deliveries() {
    let (base, db) = spawn_test_app_with_db().await;
    exercise_scoped_hook_delete(&base, &db).await;
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
async fn server_database_deleting_a_hook_in_any_scope_removes_its_deliveries() {
    let db = server_database().await;
    let base = spawn_test_app_over_db(db.clone()).await;
    exercise_scoped_hook_delete(&base, &db).await;
}

/// The journal is deleted before the hook. A failure of the hook delete must
/// put the journal back, or the API would answer 500 over a hook whose history
/// is already gone.
#[tokio::test]
async fn a_failed_hook_delete_keeps_the_hook_and_its_deliveries() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = unique("hookfault");
    let token = admin(&base, &db, &owner).await;
    create_repo(&base, &token, "hooked").await;
    let collection = format!("repos/{owner}/hooked/hooks");
    let hook_id = create_hook(&base, &token, &collection).await;
    let delivery = record_delivery(&db, hook_id, "kept").await.unwrap();

    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        format!(
            "CREATE TRIGGER fail_hook_delete BEFORE DELETE ON webhooks \
             WHEN OLD.id = {hook_id} BEGIN SELECT RAISE(ABORT, 'injected hook delete failure'); END"
        ),
    ))
    .await
    .unwrap();
    let response = delete(&base, &token, &format!("{collection}/{hook_id}")).await;
    assert_eq!(response.status(), 500);
    assert!(hook_exists(&db, hook_id).await);
    assert!(
        delivery_exists(&db, delivery).await,
        "the journal was deleted although the hook delete rolled back"
    );

    db.execute_unprepared("DROP TRIGGER fail_hook_delete")
        .await
        .unwrap();
    let response = delete(&base, &token, &format!("{collection}/{hook_id}")).await;
    assert_eq!(response.status(), 200);
    assert!(!hook_exists(&db, hook_id).await);
    assert!(!delivery_exists(&db, delivery).await);
}

// ── Repository deletion takes its hooks ──────────────────────────────────

async fn exercise_repository_delete(base: &str, db: &rg_db::DatabaseConnection) {
    let owner = unique("repohook");
    let token = admin(base, db, &owner).await;
    let repo_id = create_repo(base, &token, "retired").await;
    let collection = format!("repos/{owner}/retired/hooks");
    let hook_id = create_hook(base, &token, &collection).await;
    let delivery = record_delivery(db, hook_id, "repo").await.unwrap();

    let response = delete(base, &token, &format!("repos/{owner}/retired")).await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert!(
        !hook_exists(db, hook_id).await,
        "the repository hook survived"
    );
    assert!(!delivery_exists(db, delivery).await);
    assert!(rg_db::ops::webhook_ops::list_by_repo(db, repo_id)
        .await
        .unwrap()
        .is_empty());
    assert!(record_delivery(db, hook_id, "late").await.is_err());

    // The name is free again; the new repository must not inherit a hook.
    let reused = create_repo(base, &token, "retired").await;
    assert_ne!(reused, repo_id);
    let listed = reqwest::Client::new()
        .get(format!("{base}/api/v1/{collection}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    assert_eq!(
        listed.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!([])
    );

    // An organization's retirement deletes its repositories through the same
    // path, so their hooks leave too.
    let org = unique("repohookorg");
    create_org(base, &token, &org).await;
    let org_repo = create_org_repo(base, &token, &org, "inside").await;
    let org_repo_hook = create_hook(base, &token, &format!("repos/{org}/inside/hooks")).await;
    let org_repo_delivery = record_delivery(db, org_repo_hook, "org-repo")
        .await
        .unwrap();
    let response = delete(base, &token, &format!("orgs/{org}")).await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert!(!hook_exists(db, org_repo_hook).await);
    assert!(!delivery_exists(db, org_repo_delivery).await);
    assert!(rg_db::ops::webhook_ops::list_by_repo(db, org_repo)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn deleting_a_repository_removes_its_hooks_and_deliveries() {
    let (base, db) = spawn_test_app_with_db().await;
    exercise_repository_delete(&base, &db).await;
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
async fn server_database_deleting_a_repository_removes_its_hooks_and_deliveries() {
    let db = server_database().await;
    let base = spawn_test_app_over_db(db.clone()).await;
    exercise_repository_delete(&base, &db).await;
}

/// The hooks are deleted inside the soft-delete transaction. A failure there
/// rolls the repository back to live, and the hook and journal stay for the
/// retry instead of vanishing from a repository that still exists.
#[tokio::test]
async fn a_failed_repository_delete_keeps_its_hooks_for_the_retry() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = unique("repofault");
    let token = admin(&base, &db, &owner).await;
    let repo_id = create_repo(&base, &token, "survivor").await;
    let hook_id = create_hook(&base, &token, &format!("repos/{owner}/survivor/hooks")).await;
    let delivery = record_delivery(&db, hook_id, "survivor").await.unwrap();

    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        format!(
            "CREATE TRIGGER fail_repo_hook_delete BEFORE DELETE ON webhooks \
             WHEN OLD.repo_id = {repo_id} BEGIN SELECT RAISE(ABORT, 'injected hook delete failure'); END"
        ),
    ))
    .await
    .unwrap();
    let response = delete(&base, &token, &format!("repos/{owner}/survivor")).await;
    assert_eq!(response.status(), 500, "{}", response.text().await.unwrap());
    let repo = rg_db::ops::repo_ops::find_by_id(&db, repo_id)
        .await
        .unwrap()
        .expect("the repository row exists");
    assert!(
        repo.deleted_at.is_none(),
        "the soft delete committed without its hooks"
    );
    assert!(hook_exists(&db, hook_id).await);
    assert!(delivery_exists(&db, delivery).await);

    db.execute_unprepared("DROP TRIGGER fail_repo_hook_delete")
        .await
        .unwrap();
    let response = delete(&base, &token, &format!("repos/{owner}/survivor")).await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert!(!hook_exists(&db, hook_id).await);
    assert!(!delivery_exists(&db, delivery).await);
}

// ── Accepted values fit the columns ──────────────────────────────────────

fn url_of_length(length: usize) -> String {
    let prefix = "https://hooks.example.invalid/";
    format!("{prefix}{}", "p".repeat(length - prefix.len()))
}

async fn exercise_value_limits(base: &str, db: &rg_db::DatabaseConnection) {
    let owner = unique("hookvalues");
    let token = admin(base, db, &owner).await;
    let repo_id = create_repo(base, &token, "wide").await;
    let collection = format!("{base}/api/v1/repos/{owner}/wide/hooks");
    let client = reqwest::Client::new();
    let every_event = rg_core::webhook::service::WEBHOOK_EVENTS;
    let canon = every_event.join(",");
    assert!(
        canon.len() > 255,
        "the canon no longer exceeds VARCHAR(255)"
    );
    let max_url = url_of_length(rg_core::webhook::service::MAX_WEBHOOK_URL_BYTES);

    let created = client
        .post(&collection)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": max_url, "events": every_event }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "{}", created.text().await.unwrap());
    let hook_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let stored = rg_db::ops::webhook_ops::find_by_id(db, hook_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, max_url);
    assert_eq!(stored.events, canon);

    // Every event reaches the hook, the last one in the list included.
    for event in every_event {
        let matched = rg_db::ops::webhook_ops::list_active_for_event(db, repo_id, None, event)
            .await
            .unwrap();
        assert!(
            matched.iter().any(|hook| hook.id == hook_id),
            "dispatch of {event} does not find the hook subscribed to everything"
        );
    }

    // A repeated name subscribes to nothing more and is stored once.
    let doubled: Vec<&str> = every_event
        .iter()
        .chain(every_event.iter())
        .copied()
        .collect();
    let other_url = url_of_length(rg_core::webhook::service::MAX_WEBHOOK_URL_BYTES - 1);
    let updated = client
        .patch(format!("{collection}/{hook_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": other_url, "events": doubled }))
        .send()
        .await
        .unwrap();
    assert_eq!(updated.status(), 200, "{}", updated.text().await.unwrap());
    let stored = rg_db::ops::webhook_ops::find_by_id(db, hook_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, other_url);
    assert_eq!(stored.events, canon);

    // Past the declared limit the answer is the caller's 400, before storage.
    let too_long = url_of_length(rg_core::webhook::service::MAX_WEBHOOK_URL_BYTES + 1);
    let rejected = client
        .post(&collection)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": too_long, "events": ["push"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 400);
    let rejected = client
        .patch(format!("{collection}/{hook_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": too_long }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 400);
    assert_eq!(
        rg_db::ops::webhook_ops::find_by_id(db, hook_id)
            .await
            .unwrap()
            .unwrap()
            .url,
        other_url
    );
}

#[tokio::test]
async fn the_full_event_canon_and_the_longest_url_are_stored() {
    let (base, db) = spawn_test_app_with_db().await;
    exercise_value_limits(&base, &db).await;
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable PostgreSQL or MySQL"]
async fn server_database_the_full_event_canon_and_the_longest_url_are_stored() {
    let db = server_database().await;
    let base = spawn_test_app_over_db(db.clone()).await;
    exercise_value_limits(&base, &db).await;
}
