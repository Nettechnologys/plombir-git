//! card_9148e9ea7a45: a UNIQUE conflict used to be recognised by looking for the
//! word "unique" in an error's `Display`, and `merge_queue_ops::enqueue` did not
//! classify at all — any insert failure was re-read into a success if a row for
//! the PR happened to exist by then.
//!
//! The text match is worse than backend-dependent, it is dead: every `rg_db::ops`
//! entry point returns `anyhow::Result` and attaches a `db: ...` context, and
//! `Display` for `anyhow::Error` renders **only** the outermost context (the
//! repository's own `error.rs` asserts this). So `error.to_string()` at an
//! `rg-http` call site is the string `"db: create ssh key"` — it never contains
//! "unique", and the 409 branch guarded by it could not be reached. A duplicate
//! SSH key, deploy key, tag-protection pattern, reviewer request or CI
//! environment answered 500.
//!
//! What these tests guard:
//!
//! * **The conflict is recognisable through the `anyhow` context** each op adds,
//!   which is the shape the HTTP handlers actually classify.
//! * **The old text match really was dead**, so the fix is not cosmetic and a
//!   revert cannot pass unnoticed.
//! * **A duplicate stays a no-op and a real failure stays a failure** for the two
//!   `rg-db` sites that decide it themselves: `ci_environment_ops::add_approval`
//!   and `merge_queue_ops::enqueue`.

use rg_db::entities::{
    ci_environment, deploy_key, mirror, pr_reviewer_request, protected_tag, pull_request,
    repository, ssh_key,
};
use rg_db::sea_orm::{DatabaseConnection, NotSet, Set};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-unique-classify-{label}-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        Self { path }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

async fn setup(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// An account and a repository to hang the conflicting rows off.
async fn fixture(db: &DatabaseConnection) -> (i64, i64) {
    let user = rg_db::ops::user_ops::create_user(db, "dana", "dana@example.com", "", "Dana")
        .await
        .expect("create the account the rows hang off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set("forge".to_string()),
            description: Set(None),
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
    .expect("create the repository the rows hang off");
    (user.id, repo.id)
}

/// `futures::future::join_all` without taking a dependency on `futures` for one
/// call: poll the futures together by handing them to the runtime as tasks.
async fn join_all<F>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.expect("task panicked"));
    }
    out
}

async fn open_pr(db: &DatabaseConnection, repo_id: i64, author_id: i64, number: i64) -> i64 {
    use rg_db::sea_orm::ActiveModelTrait;
    let now = chrono::Utc::now();
    pull_request::ActiveModel {
        repo_id: Set(repo_id),
        number: Set(number),
        title: Set(format!("pr {number}")),
        state: Set("open".to_string()),
        is_draft: Set(false),
        auto_merge_enabled: Set(false),
        author_id: Set(author_id),
        head_branch: Set(format!("feature-{number}")),
        base_branch: Set("main".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("open the pull request the rows hang off")
    .id
}

/// The core assertion of this file, applied to one op's duplicate error.
///
/// Both halves matter. The first is the contract the HTTP handler needs; the
/// second records why the code it replaced never fired, so re-introducing the
/// text match fails here rather than silently in production.
fn assert_conflict_is_classifiable(error: &anyhow::Error, what: &str) {
    assert!(
        rg_db::is_unique_violation_anyhow(error),
        "a duplicate {what} must be recognisable through the anyhow context: {error:#}",
    );
    assert!(
        !error.to_string().to_ascii_lowercase().contains("unique"),
        "the old text match is expected to be dead for {what}, but `to_string()` \
         returned {:?} — if this now contains \"unique\", the context layering \
         changed and this file's premise needs re-checking",
        error.to_string(),
    );
}

#[tokio::test]
async fn a_duplicate_ssh_key_is_classifiable_through_the_context_the_op_attaches() {
    let (db, _temp) = setup("ssh").await;
    let (user_id, _repo_id) = fixture(&db).await;

    let key = |title: &str| ssh_key::ActiveModel {
        id: NotSet,
        user_id: Set(user_id),
        title: Set(title.to_string()),
        public_key: Set("ssh-ed25519 AAAAC3Nz".to_string()),
        fingerprint: Set("SHA256:duplicate".to_string()),
        created_at: Set(chrono::Utc::now()),
        last_used_at: Set(None),
    };

    rg_db::ops::ssh_key_ops::create(&db, key("laptop"))
        .await
        .expect("the first key registers");
    let error = rg_db::ops::ssh_key_ops::create(&db, key("laptop again"))
        .await
        .expect_err("the fingerprint is UNIQUE");
    assert_conflict_is_classifiable(&error, "SSH key");
}

#[tokio::test]
async fn a_duplicate_deploy_key_is_classifiable_through_the_context_the_op_attaches() {
    let (db, _temp) = setup("deploy").await;
    let (user_id, repo_id) = fixture(&db).await;

    let key = |title: &str| deploy_key::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        created_by_id: Set(user_id),
        title: Set(title.to_string()),
        public_key: Set("ssh-ed25519 AAAAC3Nz".to_string()),
        fingerprint: Set("SHA256:duplicate".to_string()),
        read_only: Set(true),
        created_at: Set(chrono::Utc::now()),
        last_used_at: Set(None),
    };

    rg_db::ops::deploy_key_ops::create(&db, key("ci"))
        .await
        .expect("the first deploy key registers");
    let error = rg_db::ops::deploy_key_ops::create(&db, key("ci again"))
        .await
        .expect_err("the fingerprint is UNIQUE");
    assert_conflict_is_classifiable(&error, "deploy key");
}

#[tokio::test]
async fn a_duplicate_tag_protection_pattern_is_classifiable() {
    let (db, _temp) = setup("tags").await;
    let (_user_id, repo_id) = fixture(&db).await;

    let rule = || protected_tag::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        pattern: Set("v*".to_string()),
        allowed_user_ids: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    };

    rg_db::ops::protected_tag_ops::create(&db, rule())
        .await
        .expect("the first pattern is stored");
    let error = rg_db::ops::protected_tag_ops::create(&db, rule())
        .await
        .expect_err("(repo_id, pattern) is UNIQUE");
    assert_conflict_is_classifiable(&error, "tag protection pattern");
}

/// `mirror::service::create_mirror` reads first and inserts second, so the
/// loser of the `UNIQUE(mirrors.repo_id)` race reaches the insert. It answers
/// the caller's own conflict only while this classification stays alive.
#[tokio::test]
async fn a_duplicate_mirror_for_one_repository_is_classifiable() {
    let (db, _temp) = setup("mirrors").await;
    let (_user_id, repo_id) = fixture(&db).await;

    let mirror = || mirror::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        url: Set("https://example.com/upstream.git".to_string()),
        username: Set(None),
        password_encrypted: Set(None),
        sync_interval_seconds: Set(3600),
        next_sync_at: Set(None),
        last_sync_at: Set(None),
        last_sync_error: Set(None),
        status: Set("active".to_string()),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    };

    rg_db::ops::mirror_ops::create(&db, mirror())
        .await
        .expect("the first mirror registers");
    let error = rg_db::ops::mirror_ops::create(&db, mirror())
        .await
        .expect_err("repo_id is UNIQUE");
    assert_conflict_is_classifiable(&error, "mirror");
}

/// card_8e6bac4c98db: the same read-then-insert shape as the mirror, for wiki
/// pages, releases and repository names. Each service answers the loser of its
/// race with the caller's own conflict, and can only do so while the violation
/// is still recognisable through the `db: ...` context its op attaches.
#[tokio::test]
async fn a_duplicate_wiki_page_title_in_one_repository_is_classifiable() {
    let (db, _temp) = setup("wiki").await;
    let (user_id, repo_id) = fixture(&db).await;

    let page = || rg_db::entities::wiki_page::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        title: Set("Home".to_string()),
        content: Set("first".to_string()),
        message: Set(None),
        author_id: Set(Some(user_id)),
        sha: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    };

    rg_db::ops::wiki_page_ops::create(&db, page())
        .await
        .expect("the first page is created");
    let error = rg_db::ops::wiki_page_ops::create(&db, page())
        .await
        .expect_err("(repo_id, title) is UNIQUE");
    assert_conflict_is_classifiable(&error, "wiki page");
}

#[tokio::test]
async fn a_duplicate_release_tag_in_one_repository_is_classifiable() {
    let (db, _temp) = setup("releases").await;
    let (user_id, repo_id) = fixture(&db).await;

    let release = || rg_db::entities::release::ActiveModel {
        repo_id: Set(repo_id),
        author_id: Set(user_id),
        tag_name: Set("v1.0.0".to_string()),
        title: Set("First".to_string()),
        body: Set(None),
        target_commitish: Set("main".to_string()),
        is_draft: Set(false),
        is_prerelease: Set(false),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };

    rg_db::ops::release_ops::create(&db, release())
        .await
        .expect("the first release is published");
    let error = rg_db::ops::release_ops::create(&db, release())
        .await
        .expect_err("(repo_id, tag_name) is UNIQUE");
    assert_conflict_is_classifiable(&error, "release tag");
}

#[tokio::test]
async fn a_repository_name_taken_in_the_same_namespace_is_classifiable() {
    let (db, _temp) = setup("repositories").await;
    let (user_id, _repo_id) = fixture(&db).await;

    // `fixture` already created `forge` in this account's personal namespace.
    let duplicate = repository::ActiveModel {
        id: NotSet,
        owner_id: Set(user_id),
        name: Set("forge".to_string()),
        description: Set(None),
        is_private: Set(false),
        default_branch: Set("main".to_string()),
        fork_id: Set(None),
        stars_count: Set(0),
        forks_count: Set(0),
        org_id: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        deleted_at: Set(None),
        origin_repo_id: Set(None),
    };

    let error = rg_db::ops::repo_ops::create(&db, duplicate)
        .await
        .expect_err("(namespace_key, name) is UNIQUE");
    assert_conflict_is_classifiable(&error, "repository name");
}

#[tokio::test]
async fn a_duplicate_reviewer_request_is_classifiable() {
    let (db, _temp) = setup("reviewers").await;
    let (user_id, repo_id) = fixture(&db).await;
    let pr_id = open_pr(&db, repo_id, user_id, 1).await;

    let request = || pr_reviewer_request::ActiveModel {
        id: NotSet,
        pr_id: Set(pr_id),
        reviewer_id: Set(user_id),
        requested_by_id: Set(user_id),
        created_at: Set(chrono::Utc::now()),
    };

    rg_db::ops::pr_reviewer_request_ops::create(&db, request())
        .await
        .expect("the first request is stored");
    let error = rg_db::ops::pr_reviewer_request_ops::create(&db, request())
        .await
        .expect_err("(pr_id, reviewer_id) is UNIQUE");
    assert_conflict_is_classifiable(&error, "reviewer request");
}

#[tokio::test]
async fn a_duplicate_ci_environment_name_is_classifiable_on_create_and_on_rename() {
    let (db, _temp) = setup("environments").await;
    let (_user_id, repo_id) = fixture(&db).await;

    let environment = |name: &str| ci_environment::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        name: Set(name.to_string()),
        protected: Set(false),
        required_approvals: Set(1),
        allowed_approver_ids: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    };

    rg_db::ops::ci_environment_ops::create(&db, environment("production"))
        .await
        .expect("the first environment is created");
    let error = rg_db::ops::ci_environment_ops::create(&db, environment("production"))
        .await
        .expect_err("(repo_id, name) is UNIQUE");
    assert_conflict_is_classifiable(&error, "CI environment");

    // The rename path reaches the same constraint from `update`.
    let staging = rg_db::ops::ci_environment_ops::create(&db, environment("staging"))
        .await
        .expect("the second environment is created");
    let mut rename: ci_environment::ActiveModel = staging.into();
    rename.name = Set("production".to_string());
    let error = rg_db::ops::ci_environment_ops::update(&db, rename)
        .await
        .expect_err("renaming onto a taken name is UNIQUE");
    assert_conflict_is_classifiable(&error, "CI environment rename");
}

/// `add_approval` answers "did this add a new approval?" — the one caller-visible
/// bit. A repeat by the same approver is `false`, and anything that is not a
/// duplicate stays an error rather than being reported as an already-recorded
/// approval.
#[tokio::test]
async fn a_repeated_environment_approval_is_a_no_op_and_a_broken_one_is_an_error() {
    use rg_db::sea_orm::ActiveModelTrait;

    let (db, _temp) = setup("approvals").await;
    let (user_id, repo_id) = fixture(&db).await;

    let environment = ci_environment::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        name: Set("production".to_string()),
        protected: Set(true),
        required_approvals: Set(1),
        allowed_approver_ids: Set(None),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
    }
    .insert(&db)
    .await
    .expect("create the environment being approved");

    // `pipeline_jobs.id` is the approval's other half of the unique key. The row
    // only needs to exist for the foreign key; nothing here reads it back.
    let job_id = rg_db::entities::pipeline_job::ActiveModel {
        stage_id: Set(1),
        name: Set("build".to_string()),
        script: Set("true".to_string()),
        allow_failure: Set(false),
        when_condition: Set("on_success".to_string()),
        status: Set("waiting_approval".to_string()),
        environment_id: Set(Some(environment.id)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert the job being approved")
    .id;

    assert!(
        rg_db::ops::ci_environment_ops::add_approval(&db, job_id, environment.id, user_id)
            .await
            .expect("the first approval is recorded"),
        "the first approval by this user is a new approval",
    );
    assert!(
        !rg_db::ops::ci_environment_ops::add_approval(&db, job_id, environment.id, user_id)
            .await
            .expect("a repeat is a no-op, not a failure"),
        "a second approval by the same user adds nothing",
    );

    // Not a duplicate: a *different* approver, so `(job_id, approved_by)` is
    // free and the write fails on the environment's foreign key instead. That
    // has to stay an error. Before the fix it depended on the phrase the backend
    // chose — a MySQL duplicate-entry message does not contain the word "unique"
    // at all, and any error merely *mentioning* a uniquely-named constraint was
    // swallowed into `Ok(false)`, reporting "already approved" for a write that
    // never happened.
    let other = rg_db::ops::user_ops::create_user(&db, "erin", "erin@example.com", "", "Erin")
        .await
        .expect("create the second approver");
    const ORPHAN: i64 = 9999;
    assert!(
        rg_db::ops::ci_environment_ops::add_approval(&db, job_id, ORPHAN, other.id)
            .await
            .is_err(),
        "an approval for an environment that does not exist must stay a failure",
    );
}

/// Concurrent first enqueues of one PR still resolve to a single entry, and every
/// caller gets it rather than a constraint error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_enqueues_of_one_pr_all_succeed_and_leave_one_entry() {
    let (db, _temp) = setup("enqueue-race").await;
    let (user_id, repo_id) = fixture(&db).await;
    let pr_id = open_pr(&db, repo_id, user_id, 1).await;

    let attempts =
        (0..8).map(|_| {
            let db = db.clone();
            async move {
                rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, user_id, "squash").await
            }
        });
    let results = join_all(attempts).await;

    for (i, result) in results.iter().enumerate() {
        assert!(
            result.is_ok(),
            "caller {i} of a concurrent first enqueue failed: {:?}",
            result.as_ref().err(),
        );
    }

    let entries = rg_db::ops::merge_queue_ops::list_by_repo(&db, repo_id)
        .await
        .expect("list the queue");
    assert_eq!(
        entries.len(),
        1,
        "the race must leave exactly one queue entry, found {entries:#?}",
    );
}

/// The re-read is armed by a UNIQUE violation and nothing else. Before the fix,
/// `enqueue` re-read `pr_id` after *any* insert failure and reported success if a
/// row turned up — so a write that never happened was reported as done.
#[tokio::test]
async fn an_enqueue_that_fails_on_a_foreign_key_is_still_a_failure() {
    let (db, _temp) = setup("enqueue-fk").await;
    let (user_id, repo_id) = fixture(&db).await;
    let pr_id = open_pr(&db, repo_id, user_id, 1).await;

    const ORPHAN: i64 = 9999;
    assert!(
        rg_db::ops::merge_queue_ops::enqueue(&db, ORPHAN, pr_id, user_id, "squash")
            .await
            .is_err(),
        "an entry for a repository that does not exist must stay a failure",
    );
    assert!(
        rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, ORPHAN, user_id, "squash")
            .await
            .is_err(),
        "an entry for a pull request that does not exist must stay a failure",
    );
    assert!(
        rg_db::ops::merge_queue_ops::list_by_repo(&db, repo_id)
            .await
            .expect("list the queue")
            .is_empty(),
        "a failed enqueue must not leave an entry behind",
    );
}

/// The branch the raced path shares with the ordinary one. A PR that is already
/// waiting keeps its place; a PR whose previous attempt finished is recycled onto
/// this call's strategy and actor.
#[tokio::test]
async fn re_enqueueing_keeps_a_waiting_entry_and_recycles_a_finished_one() {
    let (db, _temp) = setup("re-enqueue").await;
    let (user_id, repo_id) = fixture(&db).await;
    let pr_id = open_pr(&db, repo_id, user_id, 1).await;

    let first = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, user_id, "squash")
        .await
        .expect("enqueue the PR");
    let again = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, user_id, "rebase")
        .await
        .expect("re-enqueue a PR that is already waiting");
    assert_eq!(again.id, first.id, "the entry must not be duplicated");
    assert_eq!(
        again.strategy, "squash",
        "a PR already waiting its turn keeps the strategy it was queued with",
    );

    rg_db::ops::merge_queue_ops::finish(&db, first.id, "failed", Some("CI failed".into()))
        .await
        .expect("finish the entry");
    let recycled = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, user_id, "rebase")
        .await
        .expect("re-enqueue a PR whose previous attempt finished");
    assert_eq!(recycled.id, first.id, "the entry is reused, not duplicated");
    assert_eq!(recycled.status, "queued");
    assert_eq!(
        recycled.strategy, "rebase",
        "a finished entry takes this call's strategy",
    );
    assert_eq!(
        recycled.failure_reason, None,
        "the previous attempt's failure must not follow the PR into the new one",
    );
}
