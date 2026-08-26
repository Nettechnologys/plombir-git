//! Runtime smoke for PostgreSQL/MySQL CI service containers.
//!
//! Run with:
//! `FORGEKEEP_TEST_DATABASE_URL=... cargo test -p rg-core --test multi_backend_smoke -- --ignored`

use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, EntityTrait, NotSet, Set, Statement,
    TransactionTrait,
};

/// Assert a spawned probe is still parked behind the boundary under test — and
/// say what it actually did when it is not.
///
/// `assert!(timeout(window, &mut probe).await.is_err())` reads `Err` as "still
/// parked". A probe that returned an error, and a probe that panicked, both
/// resolve the future *immediately* — so `is_err()` is false and the assertion
/// reports the opposite of the truth: "it crossed the boundary" when in fact it
/// never got in, with the real failure left in the probe's own output.
async fn assert_probe_stays_blocked<T>(probe: &mut tokio::task::JoinHandle<T>, crossed: &str)
where
    T: std::fmt::Debug,
{
    match tokio::time::timeout(std::time::Duration::from_millis(200), probe).await {
        Err(_still_parked) => {}
        Ok(Ok(outcome)) => {
            panic!("{crossed} — the probe finished with {outcome:?} instead of waiting")
        }
        Ok(Err(panic)) => {
            panic!("{crossed} — the probe panicked instead of waiting: {panic}")
        }
    }
}

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

/// The hashes of a user's *live* backup codes, oldest first.
async fn live_backup_hashes<C: ConnectionTrait>(db: &C, user_id: i64) -> Vec<String> {
    let mut rows = rg_db::ops::mfa_backup_code_ops::list_codes(db, user_id)
        .await
        .expect("read stored backup codes");
    rows.sort_by_key(|row| row.id);
    rows.into_iter()
        .filter(|row| !row.used)
        .map(|row| row.code_hash)
        .collect()
}

fn backup_hashes(codes: &[String]) -> Vec<String> {
    codes
        .iter()
        .map(|code| rg_db::ops::mfa_backup_code_ops::hash_code(code))
        .collect()
}

async fn repo_fts_snapshot(db: &DatabaseConnection, repo_id: i64) -> Option<(String, String)> {
    let backend = db.get_database_backend();
    db.query_one(Statement::from_sql_and_values(
        backend,
        rg_db::prepare_sql(
            backend,
            "SELECT name, description FROM repos_fts WHERE rowid = ?",
        ),
        [repo_id.into()],
    ))
    .await
    .expect("read repository FTS row")
    .map(|row| {
        (
            row.try_get("", "name").expect("decode FTS name"),
            row.try_get("", "description")
                .expect("decode FTS description"),
        )
    })
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
    let repo_update_term = format!("repoupdatedneedle{suffix}");
    let repo_restore_term = format!("reporestoredneedle{suffix}");
    let wiki_term = format!("crossbackendneedle{suffix}");
    let first_wiki_edit_term = format!("firstwikiedit{suffix}");
    let second_wiki_edit_term = format!("secondwikiedit{suffix}");
    let first_wiki_edit_content = format!("first concurrent server edit {first_wiki_edit_term}");
    let second_wiki_edit_content = format!("second concurrent server edit {second_wiki_edit_term}");

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
    assert_eq!(
        repo_fts_snapshot(&db, repo.id).await,
        Some((
            repo_name.clone(),
            "cross backend repository search".to_string()
        )),
        "the repository INSERT trigger did not publish the source snapshot"
    );

    // card_61e4278d15e3: grouped CI publication is arbitrated by a durable
    // `(repo_id, group_name)` row. Prove the server backends lock that exact key
    // rather than the whole repository, the whole lock table, or this process.
    // SQLite has a single writer by design and is covered by rg-ci's full
    // concurrent trigger tests; PostgreSQL/MySQL are where row-level scope must
    // be demonstrated explicitly.
    let other_repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(user.id, None, &format!("lockscope{suffix}")),
    )
    .await
    .expect("create second repository for concurrency-lock scope");
    let held = db.begin().await.expect("begin held group transaction");
    rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(
        &held,
        repo.id,
        "deploy-production",
    )
    .await
    .expect("hold first CI concurrency group");

    let different_group_db = db.clone();
    let different_group_repo = repo.id;
    let different_group = tokio::spawn(async move {
        let tx = different_group_db
            .begin()
            .await
            .expect("begin different-group transaction");
        rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(
            &tx,
            different_group_repo,
            "nightly-audit",
        )
        .await
        .expect("different group must not wait for the held group");
        tx.rollback().await.expect("rollback different-group probe");
    });
    let different_repo_db = db.clone();
    let different_repo_id = other_repo.id;
    let different_repo = tokio::spawn(async move {
        let tx = different_repo_db
            .begin()
            .await
            .expect("begin different-repository transaction");
        rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(
            &tx,
            different_repo_id,
            "deploy-production",
        )
        .await
        .expect("same group name in another repository must not wait");
        tx.rollback()
            .await
            .expect("rollback different-repository probe");
    });
    let (different_group_result, different_repo_result) =
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(different_group, different_repo)
        })
        .await
        .expect("unrelated CI concurrency keys serialized globally");
    different_group_result.expect("different-group probe task panicked");
    different_repo_result.expect("different-repository probe task panicked");

    let same_group_db = db.clone();
    let same_group_repo = repo.id;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut same_group = tokio::spawn(async move {
        let tx = same_group_db
            .begin()
            .await
            .expect("begin same-group transaction");
        started_tx.send(()).expect("announce same-group attempt");
        rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(
            &tx,
            same_group_repo,
            "deploy-production",
        )
        .await
        .expect("same group acquires after its predecessor commits");
        tx.rollback().await.expect("rollback same-group probe");
    });
    started_rx.await.expect("same-group probe started");
    assert_probe_stays_blocked(
        &mut same_group,
        "the same repository/group key was not held until transaction commit",
    )
    .await;
    held.commit().await.expect("release held CI group");
    tokio::time::timeout(std::time::Duration::from_secs(3), same_group)
        .await
        .expect("same group did not resume after commit")
        .expect("same-group probe task panicked");

    // card_04db226ae9b3: the same normalized grant writer and lock sequence must
    // behave identically on PostgreSQL and MySQL. CI invokes this ignored smoke
    // once per disposable server database; SQLite has a dedicated deterministic
    // integration test in rg-db.
    let branch = rg_db::ops::protected_branch_ops::create_with_push_grants(
        &db,
        rg_db::entities::protected_branch::ActiveModel {
            repo_id: Set(repo.id),
            branch_name: Set(format!("grant-{suffix}")),
            require_pr: Set(true),
            require_status_check: Set(false),
            required_status_checks: Set(None),
            require_approval: Set(false),
            required_approvals: Set(None),
            allow_force_push: Set(false),
            require_signed_commits: Set(false),
            allowed_push_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user.id]),
    )
    .await
    .expect("create cross-backend branch grant");
    let tag = rg_db::ops::protected_tag_ops::create_with_push_grants(
        &db,
        rg_db::entities::protected_tag::ActiveModel {
            repo_id: Set(repo.id),
            pattern: Set(format!("grant-{suffix}-*")),
            allowed_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user.id]),
    )
    .await
    .expect("create cross-backend tag grant");
    let environment = rg_db::ops::ci_environment_ops::create_with_approvers(
        &db,
        rg_db::entities::ci_environment::ActiveModel {
            repo_id: Set(repo.id),
            name: Set(format!("grant-{suffix}")),
            protected: Set(true),
            required_approvals: Set(1),
            allowed_approver_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        vec![user.id],
    )
    .await
    .expect("create cross-backend environment grant");
    let grant_targets = [
        rg_db::user_grants::Target::ProtectedBranch(branch.id),
        rg_db::user_grants::Target::ProtectedTag(tag.id),
        rg_db::user_grants::Target::CiEnvironment(environment.id),
    ];

    let inactive = rg_db::ops::user_ops::create_user(
        &db,
        &format!("inactive{suffix}"),
        &format!("inactive{suffix}@example.invalid"),
        "unused",
        "Inactive Grant Principal",
    )
    .await
    .expect("create inactive grant principal");
    rg_db::ops::user_ops::update_by_id(&db, inactive.id, None, None, None, Some(false))
        .await
        .expect("deactivate grant principal")
        .expect("inactive grant principal exists");
    let retiring = rg_db::ops::user_ops::create_user(
        &db,
        &format!("retiring{suffix}"),
        &format!("retiring{suffix}@example.invalid"),
        "unused",
        "Retiring Grant Principal",
    )
    .await
    .expect("create retiring grant principal");
    assert!(rg_db::ops::user_ops::find_by_id(&db, retiring.id)
        .await
        .expect("preflight retiring principal")
        .is_some());
    assert!(
        rg_db::ops::user_ops::begin_user_retirement(&db, retiring.id)
            .await
            .expect("begin grant principal retirement")
    );

    for (invalid_id, expected) in [
        (i64::MAX, format!("grant user {} does not exist", i64::MAX)),
        (
            inactive.id,
            format!("grant user {} is inactive", inactive.id),
        ),
        (
            retiring.id,
            format!("grant user {} is being retired", retiring.id),
        ),
    ] {
        for target in grant_targets {
            let transaction = db.begin().await.expect("begin invalid grant write");
            let error = rg_db::user_grants::replace(&transaction, target, Some(&[invalid_id]))
                .await
                .expect_err("cross-backend invalid principal must be rejected");
            assert_eq!(
                rg_db::user_grants::invalid_principal_message(&error).as_deref(),
                Some(expected.as_str()),
                "different cross-backend semantic for {target:?}: {error:#}"
            );
            transaction
                .rollback()
                .await
                .expect("roll back invalid cross-backend grant write");
        }
    }
    assert_eq!(
        rg_db::user_grants::load_verified(
            &db,
            grant_targets[0],
            branch.allowed_push_user_ids.as_deref(),
        )
        .await
        .expect("load cross-backend branch grants"),
        vec![user.id]
    );
    assert_eq!(
        rg_db::user_grants::load_verified(&db, grant_targets[1], tag.allowed_user_ids.as_deref(),)
            .await
            .expect("load cross-backend tag grants"),
        vec![user.id]
    );
    assert_eq!(
        rg_db::user_grants::load_verified(
            &db,
            grant_targets[2],
            environment.allowed_approver_ids.as_deref(),
        )
        .await
        .expect("load cross-backend environment grants"),
        vec![user.id]
    );

    let mut updated_repo: rg_db::entities::repository::ActiveModel = repo.clone().into();
    updated_repo.description = Set(Some(repo_update_term.clone()));
    updated_repo.updated_at = Set(chrono::Utc::now());
    let repo = updated_repo
        .update(&db)
        .await
        .expect("update repository metadata through the source row");
    assert_eq!(
        repo_fts_snapshot(&db, repo.id).await,
        Some((repo_name.clone(), repo_update_term.clone())),
        "the repository UPDATE trigger did not replace the FTS snapshot"
    );

    // card_9d3b68368396: open-PR head refresh uses the same snapshot CAS on
    // SQLite, PostgreSQL and MySQL, including the nullable state of a deleted
    // branch. The deterministic overtaken-writer interleaving lives beside the
    // primitive in rg-db; this server smoke proves the generated predicates and
    // NULL transition have the same external result on both server dialects.
    let pr_now = chrono::Utc::now();
    let initial_pr_head = "1111111111111111111111111111111111111111";
    let refreshed_pr_head = "2222222222222222222222222222222222222222";
    let recreated_pr_head = "3333333333333333333333333333333333333333";
    let head_pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("cross-backend head refresh".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(user.id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some(initial_pr_head.to_string())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(pr_now),
            updated_at: Set(pr_now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("create cross-backend PR head fixture");
    let refreshed = rg_db::ops::pull_request_ops::update_open_head_sha(
        &db,
        repo.id,
        "feature",
        Some(refreshed_pr_head),
    )
    .await
    .expect("CAS-refresh the PR head");
    assert_eq!(refreshed.stale_rows, 0);
    assert_eq!(
        refreshed.open_prs[0].head_sha.as_deref(),
        Some(refreshed_pr_head)
    );
    let stale_swap = rg_db::ops::pull_request_ops::compare_and_swap_open_head_sha(
        &db,
        head_pr.id,
        Some(initial_pr_head),
        Some(recreated_pr_head),
    )
    .await
    .expect("run a stale PR-head compare-and-swap");
    assert!(
        !stale_swap,
        "a stale expected head must lose on every database backend"
    );
    let after_stale_swap = rg_db::ops::pull_request_ops::find_by_id(&db, head_pr.id)
        .await
        .expect("reload PR after stale compare-and-swap")
        .expect("cross-backend PR still exists");
    assert_eq!(
        after_stale_swap.head_sha.as_deref(),
        Some(refreshed_pr_head),
        "the losing writer must not roll the PR head back"
    );
    let deleted = rg_db::ops::pull_request_ops::update_open_head_sha(&db, repo.id, "feature", None)
        .await
        .expect("clear the PR head for a deleted branch");
    assert_eq!(deleted.stale_rows, 0);
    assert_eq!(deleted.open_prs[0].head_sha, None);
    let recreated = rg_db::ops::pull_request_ops::update_open_head_sha(
        &db,
        repo.id,
        "feature",
        Some(recreated_pr_head),
    )
    .await
    .expect("restore the PR head after branch recreation");
    assert_eq!(recreated.stale_rows, 0);
    assert_eq!(
        recreated.open_prs[0].head_sha.as_deref(),
        Some(recreated_pr_head)
    );
    assert_eq!(recreated.open_prs[0].id, head_pr.id);

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
    rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
        &db,
        deleted_fork.id,
        chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
    )
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

    // card_957cc2683f70: deleting an account retracts its stars through
    // `repo_stars.user_id ON DELETE CASCADE`, and the repositories they sat on
    // belong to other people, so `user_ops::delete_by_id` refreshes a whole
    // inventory of counters at once. That statement carries a variadic `IN`
    // list and a correlated subquery over its own target table — MySQL rejects
    // a subquery that reads the target in its `FROM` (error 1093) — so the
    // multi-id form needs proving on every backend, not just the single-id one
    // above.
    rg_db::ops::repo_ops::refresh_stars_counts(&db, &[repo.id, live_fork.id])
        .await
        .expect("refresh several star counters in one statement on every backend");
    let counted_repo = rg_db::ops::repo_ops::find_by_id(&db, repo.id)
        .await
        .expect("read source after batched star count refresh")
        .expect("source repository exists");
    assert_eq!(
        counted_repo.stars_count, 1,
        "the batched refresh lost the star the single-id refresh had counted"
    );
    let counted_fork = rg_db::ops::repo_ops::find_by_id(&db, live_fork.id)
        .await
        .expect("read fork after batched star count refresh")
        .expect("fork repository exists");
    assert_eq!(
        counted_fork.stars_count, 0,
        "the batched refresh credited one repository's stars to another"
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

    rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
        &db,
        personal_twin.id,
        chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
    )
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
            &first_wiki_edit_content,
            None,
            Some(user.id),
        ),
        rg_core::wiki::service::update_page(
            &db,
            repo.id,
            "Home",
            &second_wiki_edit_content,
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
        first_wiki_edit_content.as_str(),
        second_wiki_edit_content.as_str(),
    ];
    expected_states.sort_unstable();
    assert_eq!(
        preserved_states, expected_states,
        "both successful edit texts must survive in current state or history"
    );

    let (current_wiki_term, superseded_wiki_term) = if current.content == first_wiki_edit_content {
        (&first_wiki_edit_term, &second_wiki_edit_term)
    } else if current.content == second_wiki_edit_content {
        (&second_wiki_edit_term, &first_wiki_edit_term)
    } else {
        panic!("concurrent updates left an unexpected current wiki state")
    };
    let (current_wiki_results, current_wiki_total) = rg_core::search::service::search(
        &db,
        &format!("{current_wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search the current concurrent wiki edit");
    assert_eq!(current_wiki_total, 1);
    assert_eq!(
        current_wiki_results.first().map(|result| result.id),
        Some(page.id),
        "FTS does not reflect the current source row"
    );
    let (_, superseded_wiki_total) = rg_core::search::service::search(
        &db,
        &format!("{superseded_wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search the superseded concurrent wiki edit");
    assert_eq!(
        superseded_wiki_total, 0,
        "FTS retained the superseded concurrent wiki content"
    );

    // Repository-local issue numbers under the same treatment. On a server
    // backend the losing insert reaches the UNIQUE index and comes back as a
    // duplicate key — the primitive SQLite rarely produces here, because it
    // refuses the write on the snapshot first.
    let (first_issue, second_issue) = tokio::join!(
        rg_core::issue::service::create_issue(
            &db,
            repo.id,
            user.id,
            "first concurrent issue".to_string(),
            None,
            None,
            None,
        ),
        rg_core::issue::service::create_issue(
            &db,
            repo.id,
            user.id,
            "second concurrent issue".to_string(),
            None,
            None,
            None,
        )
    );
    let first_issue = first_issue.expect("store the first concurrent issue");
    let second_issue = second_issue.expect("store the second concurrent issue");
    let mut issue_numbers = [first_issue.number, second_issue.number];
    issue_numbers.sort_unstable();
    assert_eq!(
        issue_numbers,
        [1, 2],
        "parallel issue creates must each keep a distinct consecutive number"
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
        &format!("{current_wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("verify wiki FTS deletion");
    assert_eq!(wiki_total_after_delete, 0);

    // card_04054a87159b: a board reorder is published as one serialized
    // transaction. The three backends refuse an overtaken writer in three
    // different ways — a lost WAL snapshot, a serialization failure, a deadlock
    // victim — so the external promise (both callers succeed, and the board
    // holds one submitted order in full) has to be checked on the server
    // databases too, not only on SQLite.
    let board_now = chrono::Utc::now();
    let board = rg_db::ops::board_ops::create_board(
        &db,
        rg_db::entities::board::ActiveModel {
            id: NotSet,
            repo_id: Set(Some(repo.id)),
            org_id: Set(None),
            name: Set("Smoke".to_string()),
            description: Set(None),
            created_by: Set(Some(user.id)),
            created_at: Set(board_now),
            updated_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test board");
    let board_column = rg_db::ops::board_ops::create_column(
        &db,
        rg_db::entities::board_column::ActiveModel {
            id: NotSet,
            board_id: Set(board.id),
            name: Set("Todo".to_string()),
            color: Set(None),
            position: Set(0),
            created_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test board column");
    let mut card_ids = Vec::with_capacity(3);
    for index in 0..3 {
        let card = rg_db::ops::board_ops::create_card(
            &db,
            rg_db::entities::board_card::ActiveModel {
                id: NotSet,
                column_id: Set(board_column.id),
                issue_id: Set(None),
                note: Set(Some(format!("smoke card {index}"))),
                position: Set(index),
                created_at: Set(board_now),
                updated_at: Set(board_now),
            },
        )
        .await
        .expect("create smoke-test board card");
        card_ids.push(card.id);
    }

    let reversed_order: Vec<(i64, i32)> =
        vec![(card_ids[0], 2), (card_ids[1], 1), (card_ids[2], 0)];
    let shifted_order: Vec<(i64, i32)> =
        vec![(card_ids[0], 10), (card_ids[1], 11), (card_ids[2], 12)];
    let (first_reorder, second_reorder) = tokio::join!(
        rg_db::ops::board_ops::update_card_positions(
            &db,
            board.id,
            Some(board_column.id),
            &reversed_order,
        ),
        rg_db::ops::board_ops::update_card_positions(
            &db,
            board.id,
            Some(board_column.id),
            &shifted_order,
        ),
    );
    assert_eq!(
        first_reorder.expect("store the first concurrent reorder"),
        rg_db::ops::board_ops::ReorderOutcome::Applied
    );
    assert_eq!(
        second_reorder.expect("store the second concurrent reorder"),
        rg_db::ops::board_ops::ReorderOutcome::Applied
    );

    let mut stored_positions = Vec::with_capacity(card_ids.len());
    for card_id in &card_ids {
        stored_positions.push(
            rg_db::ops::board_ops::find_card_by_id(&db, *card_id)
                .await
                .expect("read reordered board card")
                .expect("board card still exists")
                .position,
        );
    }
    assert!(
        stored_positions == vec![2, 1, 0] || stored_positions == vec![10, 11, 12],
        "concurrent reorders left a blended order: {stored_positions:?}"
    );

    let absent_card = card_ids.iter().copied().max().unwrap_or_default() + 10_000;
    let refused = rg_db::ops::board_ops::update_card_positions(
        &db,
        board.id,
        Some(board_column.id),
        &[(card_ids[0], 30), (absent_card, 31)],
    )
    .await
    .expect("a card this board does not own is an answer, not a failure");
    assert_eq!(
        refused,
        rg_db::ops::board_ops::ReorderOutcome::NotOnBoard(vec![absent_card])
    );
    let refused_positions = rg_db::ops::board_ops::find_card_by_id(&db, card_ids[0])
        .await
        .expect("read the in-scope card of a refused batch")
        .expect("board card still exists")
        .position;
    assert_eq!(
        refused_positions, stored_positions[0],
        "a refused batch wrote the card that was in scope"
    );

    assert!(
        rg_db::ops::board_ops::delete_board_by_id(&db, board.id)
            .await
            .expect("delete smoke-test board"),
        "deleting the smoke-test board removed no row"
    );

    // ── MFA enrolment atomicity (card_3c33caaf7402) ──────────────────────────
    //
    // The precise halves of this live where a fault can be aimed at one row of
    // the set: `rg-db/tests/mfa_backup_code_set_atomicity` and `rg-http`'s
    // `mfa_enable_atomicity_tests`, both on a SQLite trigger. Neither seam is
    // portable, so what this backend has to answer for is the mechanism those
    // two rest on — the whole replacement joins the caller's unit of work (a
    // nested SAVEPOINT under an outer transaction), so a failure anywhere in that
    // unit takes the set with it instead of leaving the account with a second
    // factor and no codes.
    let first_codes = rg_db::ops::mfa_backup_code_ops::generate_codes(
        rg_db::ops::mfa_backup_code_ops::BACKUP_CODE_COUNT,
    );
    let enrolled = rg_db::ops::user_ops::enable_mfa_with_backup_codes(&db, user.id, &first_codes)
        .await
        .expect("enrol a second factor and its backup codes in one commit");
    assert!(
        enrolled.mfa_enabled,
        "the enrolment committed the codes without the flag"
    );
    assert_eq!(
        live_backup_hashes(&db, user.id).await,
        backup_hashes(&first_codes),
        "the codes handed to the owner are not the codes that were stored"
    );

    let second_codes = rg_db::ops::mfa_backup_code_ops::generate_codes(
        rg_db::ops::mfa_backup_code_ops::BACKUP_CODE_COUNT,
    );
    let doomed = db.begin().await.expect("begin a doomed re-issue");
    rg_db::ops::mfa_backup_code_ops::set_codes(&doomed, user.id, &second_codes)
        .await
        .expect("the replacement must nest inside the caller's transaction");
    assert_eq!(
        live_backup_hashes(&doomed, user.id).await,
        backup_hashes(&second_codes),
        "the nested replacement is not visible to the transaction that made it"
    );
    // A later step of the same enrolment failing is exactly the case the split
    // commits could not survive. `i64::MAX` is nobody's account.
    rg_db::ops::user_ops::enable_mfa(&doomed, i64::MAX)
        .await
        .expect_err("the doomed step must fail");
    doomed
        .rollback()
        .await
        .expect("roll the doomed re-issue back");
    assert_eq!(
        live_backup_hashes(&db, user.id).await,
        backup_hashes(&first_codes),
        "a re-issue that failed after writing its codes revoked the set the owner still holds"
    );

    // ── Repository transfer lease (card_507ff03ec043) ────────────────────────
    //
    // The lease is the boundary that keeps a transfer's storage move and the
    // deletion of the namespace it is moving out of from each other, and both of
    // its halves are backend-specific: the claim rides an upsert whose DO NOTHING
    // is a MySQL polyfill, and the source lifecycle is verified through
    // `lock_exclusive`, which only becomes `FOR UPDATE` on a server database. A
    // SQLite-only proof says nothing about either.
    let stale_before = || chrono::Utc::now() - rg_db::ops::repo_ops::TRANSFER_LEASE_STALE_AFTER;
    let first_holder = format!("holder-a-{suffix}");
    let second_holder = format!("holder-b-{suffix}");
    async fn lease_bid(
        db: &DatabaseConnection,
        repo_id: i64,
        owner_id: i64,
        source: &str,
        token: &str,
    ) -> rg_db::ops::repo_ops::TransferLeaseBid {
        rg_db::ops::repo_ops::bid_for_transfer_lease(
            db,
            repo_id,
            owner_id,
            None,
            source,
            "elsewhere",
            token,
            chrono::Utc::now() - rg_db::ops::repo_ops::TRANSFER_LEASE_STALE_AFTER,
        )
        .await
        .expect("bid for the repository transfer lease")
    }
    assert_eq!(
        lease_bid(&db, repo.id, user.id, &username, &first_holder).await,
        rg_db::ops::repo_ops::TransferLeaseBid::Granted,
        "the first bid on an untouched repository must win"
    );
    assert_eq!(
        lease_bid(&db, repo.id, user.id, &username, &second_holder).await,
        rg_db::ops::repo_ops::TransferLeaseBid::Busy,
        "two transfers hold the same repository at once on this backend"
    );
    assert!(
        rg_db::ops::repo_ops::transfer_lease_in_flight(&db, repo.id, stale_before())
            .await
            .expect("read the transfer lease")
            .is_some(),
        "the deletion path cannot see a lease this backend accepted"
    );
    // A losing bid must not release the winner's hold.
    assert!(
        !rg_db::ops::repo_ops::release_transfer_lease(&db, repo.id, &second_holder)
            .await
            .expect("attempt a release from the wrong holder"),
        "a token that never held the lease released it"
    );
    assert!(
        rg_db::ops::repo_ops::release_transfer_lease(&db, repo.id, &first_holder)
            .await
            .expect("release the transfer lease"),
        "the holder could not release its own lease"
    );
    assert!(
        rg_db::ops::repo_ops::transfer_lease_in_flight(&db, repo.id, stale_before())
            .await
            .expect("read the released transfer lease")
            .is_none(),
        "a released lease is still visible to the deletion path"
    );
    // And the source lifecycle half: a claimed namespace refuses the bid, under
    // the row lock a retirement claim contends for on this backend.
    assert!(rg_db::ops::user_ops::begin_user_retirement(&db, user.id)
        .await
        .expect("claim the source account"));
    assert_eq!(
        lease_bid(&db, repo.id, user.id, &username, &first_holder).await,
        rg_db::ops::repo_ops::TransferLeaseBid::SourceAccountClosed,
        "a transfer was admitted out of a namespace that is being retired"
    );
    assert!(
        rg_db::ops::repo_ops::transfer_lease_in_flight(&db, repo.id, stale_before())
            .await
            .expect("read the refused transfer lease")
            .is_none(),
        "a refused bid left its lease row behind"
    );
    rg_db::ops::user_ops::abort_user_retirement(&db, user.id)
        .await
        .expect("reopen the source account");

    // ── Mirror sync lease (card_a1f2a20281af) ────────────────────────────────
    //
    // Same protocol, same backend-specific halves — an upsert whose DO NOTHING is
    // a MySQL polyfill and a `lock_exclusive` that only becomes `FOR UPDATE` on a
    // server database — plus one this table has and the transfer lease does not:
    // the guarded soft-delete, whose whole correctness is that its UPDATE takes
    // the repository row *before* it reads this table, in one transaction.
    let sync_stale_before = || chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER;
    let sync_holder = format!("sync-a-{suffix}");
    let rival_holder = format!("sync-b-{suffix}");
    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(&db, repo.id, &sync_holder, sync_stale_before())
            .await
            .expect("bid for the mirror sync lease"),
        rg_db::ops::mirror_ops::SyncLeaseBid::Granted,
        "the first bid on an unsynced mirror must win"
    );
    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(
            &db,
            repo.id,
            &rival_holder,
            sync_stale_before()
        )
        .await
        .expect("bid for a mirror sync lease somebody else holds"),
        rg_db::ops::mirror_ops::SyncLeaseBid::Busy,
        "two passes write the same mirror clone at once on this backend"
    );
    assert!(
        rg_db::ops::mirror_ops::sync_lease_in_flight(&db, repo.id, sync_stale_before())
            .await
            .expect("read the mirror sync lease")
            .is_some(),
        "the deletion path cannot see a lease this backend accepted"
    );
    // The half that has to hold inside a transaction: the row must survive a
    // refused retirement, not be soft-deleted and then reported as refused.
    assert_eq!(
        rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(&db, repo.id, sync_stale_before())
            .await
            .expect("run the guarded soft-delete under a held mirror sync lease"),
        rg_db::ops::repo_ops::RepositoryRetirement::MirrorSyncInFlight,
        "the guarded soft-delete cannot see a lease this backend accepted"
    );
    assert!(
        rg_db::ops::repo_ops::find_by_id(&db, repo.id)
            .await
            .expect("re-read the repository after a refused retirement")
            .is_some(),
        "the refused retirement rolled forward instead of back on this backend"
    );
    assert!(
        !rg_db::ops::mirror_ops::release_sync_lease(&db, repo.id, &rival_holder)
            .await
            .expect("attempt a release from the wrong holder"),
        "a token that never held the mirror sync lease released it"
    );
    assert!(
        rg_db::ops::mirror_ops::release_sync_lease(&db, repo.id, &sync_holder)
            .await
            .expect("release the mirror sync lease"),
        "the holder could not release its own lease"
    );
    assert!(
        rg_db::ops::mirror_ops::sync_lease_in_flight(&db, repo.id, sync_stale_before())
            .await
            .expect("read the released mirror sync lease")
            .is_none(),
        "a released lease is still visible to the deletion path"
    );
    assert_eq!(
        rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(&db, repo.id, sync_stale_before())
            .await
            .expect("retire the repository once no pass holds it"),
        rg_db::ops::repo_ops::RepositoryRetirement::Deleted,
        "the guarded soft-delete kept refusing after the pass had finished"
    );
    // …and the other direction: a bid against a repository that is already gone
    // must decline and leave no row behind to block anything.
    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(&db, repo.id, &sync_holder, sync_stale_before())
            .await
            .expect("bid for the mirror sync lease of a deleted repository"),
        rg_db::ops::mirror_ops::SyncLeaseBid::RepositoryGone,
        "a pass was admitted against a repository this backend had already retired"
    );
    assert!(
        rg_db::ops::mirror_ops::sync_lease_in_flight(&db, repo.id, sync_stale_before())
            .await
            .expect("read the refused mirror sync lease")
            .is_none(),
        "a refused bid left its lease row behind"
    );

    rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
        &db,
        repo.id,
        chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
    )
    .await
    .expect("soft-delete the repository through the source row");
    assert_eq!(
        repo_fts_snapshot(&db, repo.id).await,
        None,
        "the repository UPDATE trigger left a soft-deleted row searchable"
    );

    let deleted_repo = rg_db::entities::repository::Entity::find_by_id(repo.id)
        .one(&db)
        .await
        .expect("read the raw soft-deleted repository")
        .expect("soft-deleted repository row still exists");
    let mut restored_repo: rg_db::entities::repository::ActiveModel = deleted_repo.into();
    restored_repo.deleted_at = Set(None);
    restored_repo.description = Set(Some(repo_restore_term.clone()));
    restored_repo.updated_at = Set(chrono::Utc::now());
    restored_repo
        .update(&db)
        .await
        .expect("restore repository through the source row");
    assert_eq!(
        repo_fts_snapshot(&db, repo.id).await,
        Some((repo_name.clone(), repo_restore_term.clone())),
        "restoring the repository did not recreate its current FTS snapshot"
    );
    let (restored_results, restored_total) = rg_core::search::service::search(
        &db,
        &format!("{repo_restore_term} author:{username}"),
        "repos",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search the restored repository metadata");
    assert_eq!(restored_total, 1);
    assert_eq!(
        restored_results.first().map(|result| result.id),
        Some(repo.id)
    );

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
    assert_eq!(repo_fts_snapshot(&db, repo.id).await, None);
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
    rg_db::ops::user_ops::delete_by_id(&db, inactive.id)
        .await
        .expect("delete inactive smoke-test user");
    rg_db::ops::user_ops::delete_by_id(&db, retiring.id)
        .await
        .expect("delete retiring smoke-test user");
    assert!(rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .expect("verify smoke-test user cleanup")
        .is_none());
}
