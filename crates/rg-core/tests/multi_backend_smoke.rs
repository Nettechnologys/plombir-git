//! Runtime smoke for PostgreSQL/MySQL CI service containers.
//!
//! Run with:
//! `IRONFORGE_TEST_DATABASE_URL=... cargo test -p rg-core --test multi_backend_smoke -- --ignored`

use sea_orm::{NotSet, Set};

#[tokio::test]
#[ignore = "requires IRONFORGE_TEST_DATABASE_URL pointing at a disposable database"]
async fn migrations_crud_counters_and_fts_work_on_server_database() {
    let database_url = std::env::var("IRONFORGE_TEST_DATABASE_URL")
        .expect("IRONFORGE_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, 15, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("dbsmoke{suffix}");
    let repo_name = format!("crossbackendrepo{suffix}");
    let wiki_term = format!("crossbackendneedle{suffix}");

    let user = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "Database Smoke",
    )
    .await
    .expect("create user");

    let (first, second, third, fourth, fifth) = tokio::join!(
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
    );
    for result in [first, second, third, fourth, fifth] {
        result.expect("atomically record concurrent failed login");
    }
    let locked_user = rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .expect("read locked user")
        .expect("locked user exists");
    assert_eq!(locked_user.login_attempts, 5);
    assert!(locked_user.locked_until.is_some());
    rg_db::ops::user_ops::reset_login_failures(&db, user.id)
        .await
        .expect("reset failed logins");

    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set(repo_name.clone()),
            description: Set(Some("cross backend repository search".to_string())),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        },
    )
    .await
    .expect("create repository");

    assert!(
        rg_db::ops::repo_star_ops::toggle_star(&db, user.id, repo.id)
            .await
            .expect("create star")
    );
    rg_db::ops::repo_ops::update_stars_count(&db, repo.id)
        .await
        .expect("update star counter with backend-specific placeholders");
    let counted_repo = rg_db::ops::repo_ops::find_by_owner_and_name(&db, user.id, &repo_name)
        .await
        .expect("read repository")
        .expect("repository exists");
    assert_eq!(counted_repo.stars_count, 1);

    let page = rg_core::wiki::service::create_page(
        &db,
        repo.id,
        "Home",
        &format!("This page contains {wiki_term} for full text search."),
        Some("initial page"),
        Some(user.id),
    )
    .await
    .expect("create wiki page and synchronize FTS");

    let (wiki_results, wiki_total) = rg_core::search::service::search(
        &db,
        &format!("{wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        1,
        20,
    )
    .await
    .expect("search wiki FTS with repository filter");
    assert_eq!(wiki_total, 1);
    assert_eq!(wiki_results.first().map(|result| result.id), Some(page.id));

    let (repo_results, repo_total) = rg_core::search::service::search(
        &db,
        &format!("{repo_name} author:{username}"),
        "repos",
        1,
        20,
    )
    .await
    .expect("search repository FTS with owner filter");
    assert_eq!(repo_total, 1);
    assert_eq!(repo_results.first().map(|result| result.id), Some(repo.id));

    rg_core::wiki::service::delete_page(&db, repo.id, "Home")
        .await
        .expect("delete wiki page and FTS row");
    let (_, wiki_total_after_delete) = rg_core::search::service::search(
        &db,
        &format!("{wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        1,
        20,
    )
    .await
    .expect("verify wiki FTS deletion");
    assert_eq!(wiki_total_after_delete, 0);

    assert!(
        !rg_db::ops::repo_star_ops::toggle_star(&db, user.id, repo.id)
            .await
            .expect("remove smoke-test star")
    );
    rg_db::ops::repo_ops::delete_by_id(&db, repo.id)
        .await
        .expect("delete smoke-test repository");
    rg_db::ops::user_ops::delete_by_id(&db, user.id)
        .await
        .expect("delete smoke-test user");
    assert!(rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .expect("verify smoke-test user cleanup")
        .is_none());
}
