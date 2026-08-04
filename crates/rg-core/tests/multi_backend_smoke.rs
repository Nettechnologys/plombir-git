//! Runtime smoke for PostgreSQL/MySQL CI service containers.
//!
//! Run with:
//! `FORGEKEEP_TEST_DATABASE_URL=... cargo test -p rg-core --test multi_backend_smoke -- --ignored`

use sea_orm::{NotSet, Set};

/// A repository row in the namespace given by `org_id` (`None` = personal).
fn namespace_repo(
    owner_id: i64,
    org_id: Option<i64>,
    name: &str,
) -> rg_db::entities::repository::ActiveModel {
    let now = chrono::Utc::now();
    rg_db::entities::repository::ActiveModel {
        id: NotSet,
        owner_id: Set(owner_id),
        name: Set(name.to_string()),
        description: Set(None),
        is_private: Set(false),
        default_branch: Set("main".to_string()),
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

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn migrations_crud_counters_and_fts_work_on_server_database() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
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
    let counted_repo =
        rg_db::ops::repo_ops::find_personal_by_owner_and_name(&db, user.id, &repo_name)
            .await
            .expect("read repository")
            .expect("repository exists");
    assert_eq!(counted_repo.stars_count, 1);

    let mut live_fork = namespace_repo(user.id, None, &format!("forklive{suffix}"));
    live_fork.origin_repo_id = Set(Some(repo.id));
    let live_fork = rg_db::ops::repo_ops::create(&db, live_fork)
        .await
        .expect("create live fork row");

    let mut deleted_fork = namespace_repo(user.id, None, &format!("forkgone{suffix}"));
    deleted_fork.origin_repo_id = Set(Some(repo.id));
    let deleted_fork = rg_db::ops::repo_ops::create(&db, deleted_fork)
        .await
        .expect("create deleted fork row");
    rg_db::ops::repo_ops::soft_delete(&db, deleted_fork.id)
        .await
        .expect("soft-delete one fork before refreshing the count");

    rg_db::ops::repo_ops::update_forks_count(&db, repo.id)
        .await
        .expect("refresh fork counter with backend-specific atomic SQL");
    let counted_repo = rg_db::ops::repo_ops::find_by_id(&db, repo.id)
        .await
        .expect("read source after fork count refresh")
        .expect("source repository exists");
    assert_eq!(
        counted_repo.forks_count, 1,
        "only the live fork row contributes to forks_count"
    );

    // card_615e00843297: `repositories` used to hold one name per *account*, so
    // a personal repository and one in an organization the same account owns
    // could not share a name. The replacement — a `namespace_key` generated
    // column plus `UNIQUE (namespace_key, name)` — is spelled differently on
    // each backend (`STORED` here, `VIRTUAL` on MySQL, a table rebuild on
    // SQLite), so "the migration applied" is not the same claim as "it
    // enforces the right thing". Assert the behaviour, on the real server.
    let org = rg_db::ops::org_ops::create_org(
        &db,
        &format!("{username}org"),
        None,
        None,
        user.id,
        "public",
    )
    .await
    .expect("create an organization owned by the same account");

    let twin = format!("twin{suffix}");
    let personal_twin = rg_db::ops::repo_ops::create(&db, namespace_repo(user.id, None, &twin))
        .await
        .expect("create the personal repository");
    rg_db::ops::repo_ops::create(&db, namespace_repo(user.id, Some(org.id), &twin))
        .await
        .expect(
            "the two namespaces still cannot hold the same name — the account-wide \
             constraint is still on this backend",
        );
    assert!(
        rg_db::ops::repo_ops::create(&db, namespace_repo(user.id, None, &twin))
            .await
            .is_err(),
        "a duplicate name inside one namespace was accepted — uniqueness has to stay \
         enforced by the database, not only by the service layer"
    );

    rg_db::ops::repo_ops::soft_delete(&db, personal_twin.id)
        .await
        .expect("soft-delete the personal repository");
    rg_db::ops::repo_ops::create(&db, namespace_repo(user.id, None, &twin))
        .await
        .expect(
            "a soft-deleted repository still reserves its name: every lookup filters \
             `deleted_at IS NULL`, so recreating it surfaced as an anonymous 5xx",
        );

    let initial_wiki_content = format!("This page contains {wiki_term} for full text search.");
    let page = rg_core::wiki::service::create_page(
        &db,
        repo.id,
        "Home",
        &initial_wiki_content,
        Some("initial page"),
        Some(user.id),
    )
    .await
    .expect("create wiki page and synchronize FTS");

    let (wiki_results, wiki_total) = rg_core::search::service::search(
        &db,
        &format!("{wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search wiki FTS with repository filter");
    assert_eq!(wiki_total, 1);
    assert_eq!(wiki_results.first().map(|result| result.id), Some(page.id));

    let (first_edit, second_edit) = tokio::join!(
        rg_core::wiki::service::update_page(
            &db,
            repo.id,
            "Home",
            "first concurrent server edit",
            None,
            Some(user.id),
        ),
        rg_core::wiki::service::update_page(
            &db,
            repo.id,
            "Home",
            "second concurrent server edit",
            None,
            Some(user.id),
        ),
    );
    first_edit.expect("store the first concurrent wiki edit");
    second_edit.expect("store the second concurrent wiki edit");

    let revisions = rg_core::wiki::service::list_revisions(&db, repo.id, "Home")
        .await
        .expect("read concurrent wiki revisions");
    assert_eq!(
        revisions
            .iter()
            .map(|revision| revision.version)
            .collect::<Vec<_>>(),
        vec![2, 1],
        "parallel edits must leave one uniquely numbered revision each"
    );
    let current = rg_core::wiki::service::get_page(&db, repo.id, "Home")
        .await
        .expect("read current wiki page after concurrent edits")
        .expect("wiki page still exists");
    let mut preserved_states = revisions
        .iter()
        .map(|revision| revision.content.as_str())
        .chain(std::iter::once(current.content.as_str()))
        .collect::<Vec<_>>();
    preserved_states.sort_unstable();
    let mut expected_states = vec![
        initial_wiki_content.as_str(),
        "first concurrent server edit",
        "second concurrent server edit",
    ];
    expected_states.sort_unstable();
    assert_eq!(
        preserved_states, expected_states,
        "both successful edit texts must survive in current state or history"
    );

    let (repo_results, repo_total) = rg_core::search::service::search(
        &db,
        &format!("{repo_name} author:{username}"),
        "repos",
        Some(user.id),
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
        Some(user.id),
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
    rg_db::ops::repo_ops::delete_by_id(&db, live_fork.id)
        .await
        .expect("delete live fork row");
    rg_db::ops::repo_ops::delete_by_id(&db, deleted_fork.id)
        .await
        .expect("delete soft-deleted fork row");
    rg_db::ops::repo_ops::delete_by_id(&db, repo.id)
        .await
        .expect("delete smoke-test repository");
    // `organizations.owner_id` carries no foreign key, so deleting the user
    // below would leave this row behind.
    assert!(
        rg_db::ops::org_ops::delete_org(&db, org.id)
            .await
            .expect("delete smoke-test organization"),
        "deleting the smoke-test organization removed no row"
    );
    rg_db::ops::user_ops::delete_by_id(&db, user.id)
        .await
        .expect("delete smoke-test user");
    assert!(rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .expect("verify smoke-test user cleanup")
        .is_none());
}
