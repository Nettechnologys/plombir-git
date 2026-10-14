//! One repository name per namespace, whatever its case (card_552648b36416).
//!
//! `Payments` and `payments` are one name to everybody who reads a link and to
//! every client that lowercases what it is given — Docker refuses an upper-case
//! letter anywhere in an image path. SQLite and PostgreSQL compared repository
//! names exactly, so the twin was one `POST /repos` away for anyone allowed to
//! create repositories in the namespace.

use sea_orm::{ActiveModelTrait, ConnectionTrait, NotSet, Set};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn send(
    method: reqwest::Method,
    url: String,
    token: &str,
    body: serde_json::Value,
) -> (u16, String) {
    let response = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

fn row(owner_id: i64, org_id: Option<i64>, name: &str) -> rg_db::entities::repository::ActiveModel {
    let now = chrono::Utc::now();
    rg_db::entities::repository::ActiveModel {
        id: NotSet,
        owner_id: Set(owner_id),
        name: Set(name.to_string()),
        description: Set(None),
        is_private: Set(false),
        default_branch: Set("main".into()),
        fork_id: Set(None),
        stars_count: Set(0),
        forks_count: Set(0),
        org_id: Set(org_id),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        origin_repo_id: Set(None),
    }
}

/// Every door into a namespace — creation, rename, transfer — refuses another
/// case of a name the namespace holds, with the conflict a taken name gets;
/// and the database refuses it to a write that skips every door.
#[tokio::test]
async fn a_name_that_differs_only_in_case_is_taken_in_its_namespace() {
    let (base, db) = spawn_test_app_with_db().await;
    case_twins_are_refused(&base, &db, "case").await;
}

/// The same, on the server database `PLOMBIR_GIT_TEST_DATABASE_URL` names —
/// PostgreSQL compares names exactly too, and MySQL's key has to keep holding
/// under the folded one.
#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at a disposable server database"]
async fn a_name_that_differs_only_in_case_is_taken_on_a_server_database() {
    let _shared = crate::common::SHARED_SERVER_DB.lock().await;
    let database_url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to disposable server database");
    rg_db::run_migrations(&db)
        .await
        .expect("migrate disposable server database");
    let dir = tempfile::tempdir().unwrap();
    let base = crate::common::spawn_test_app_over_db_with(
        db.clone(),
        dir.path().join("repos"),
        crate::common::StateOverrides::default(),
    )
    .await;
    crate::common::wait_for_listener(base.trim_start_matches("http://")).await;
    // A server database outlives one run, so the names carry a suffix.
    let prefix = format!("case{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    case_twins_are_refused(&base, &db, &prefix).await;
}

async fn case_twins_are_refused(base: &str, db: &rg_db::DatabaseConnection, prefix: &str) {
    let owner = format!("{prefix}-owner");
    let org = format!("{prefix}-org");
    let (token, owner_id) = register_full(base, &owner, &format!("{owner}@example.com")).await;
    create_repo(base, &token, "Payments").await;
    create_repo(base, &token, "ledger").await;

    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        &token,
        serde_json::json!({ "name": "payments" }),
    )
    .await;
    assert_eq!(status, 409, "creating a case twin: {body}");

    let (status, body) = send(
        reqwest::Method::PATCH,
        format!("{base}/api/v1/repos/{owner}/ledger"),
        &token,
        serde_json::json!({ "name": "PAYMENTS" }),
    )
    .await;
    assert_eq!(status, 409, "renaming into a case twin: {body}");

    // A repository may change the case of its own name.
    let (status, body) = send(
        reqwest::Method::PATCH,
        format!("{base}/api/v1/repos/{owner}/Payments"),
        &token,
        serde_json::json!({ "name": "payments" }),
    )
    .await;
    assert_eq!(status, 200, "a case-only rename of its own name: {body}");

    // A transfer into an organization that holds another case of the name.
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/orgs"),
        &token,
        serde_json::json!({ "name": org }),
    )
    .await;
    assert!(status == 201 || status == 200, "{status}: {body}");
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        &token,
        serde_json::json!({ "name": "LEDGER", "org": org }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/{owner}/ledger/transfer"),
        &token,
        serde_json::json!({ "new_owner": org }),
    )
    .await;
    assert_eq!(status, 409, "transferring next to a case twin: {body}");

    // Another namespace holds no spelling of it: free there.
    let other = format!("{prefix}-other");
    let (other_token, _) = register_full(base, &other, &format!("{other}@example.com")).await;
    create_repo(base, &other_token, "PAYMENTS").await;

    // The guarantee is the database's, not the door's.
    let refused = row(owner_id, None, "PayMents")
        .insert(db)
        .await
        .expect_err("the database refuses a case twin a write slipped past the doors");
    assert!(rg_db::is_unique_violation(&refused), "{refused}");
}

/// A pair that predates the rule keeps working and is named at boot; a
/// lowercasing client reaches the older repository of the pair.
#[tokio::test]
async fn case_twins_from_before_the_rule_are_named_and_the_older_one_holds_the_name() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, owner_id) = register_full(&base, "case-legacy", "case-legacy@example.com").await;
    create_repo(&base, &token, "Payments").await;

    // The state the upgrade leaves a twin in: present, marked exempt, keyless.
    db.execute_unprepared("DROP INDEX uq_repositories_namespace_name_key")
        .await
        .expect("lift the folded rule for the legacy seed");
    let twin = row(owner_id, None, "payments")
        .insert(&db)
        .await
        .expect("seed the twin");
    db.execute_unprepared(&format!(
        "UPDATE repositories SET name_case_exempt = 1 WHERE id = {}; \
         CREATE UNIQUE INDEX uq_repositories_namespace_name_key \
           ON repositories (namespace_key, name_key)",
        twin.id
    ))
    .await
    .expect("put the rule back, as the upgrade leaves it");

    let pairs = rg_db::ops::repo_ops::list_names_held_only_in_another_case(&db)
        .await
        .expect("list case twins");
    assert_eq!(
        pairs
            .iter()
            .map(|(twin, holder)| (twin.name.as_str(), holder.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("payments", "Payments")]
    );
    let held = rg_db::ops::repo_ops::find_in_namespace_by_name_key(&db, owner_id, None, "PAYMENTS")
        .await
        .unwrap()
        .expect("the folded name resolves");
    assert_eq!(held.name, "Payments", "the older repository holds the name");

    // The keyless twin survives an unrelated update.
    let (status, body) = send(
        reqwest::Method::PATCH,
        format!("{base}/api/v1/repos/case-legacy/payments"),
        &token,
        serde_json::json!({ "description": "still here" }),
    )
    .await;
    assert_eq!(status, 200, "an unrelated update of a keyless twin: {body}");
}
