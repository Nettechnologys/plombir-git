//! Coverage for the watch fan-out producers (card_dc66742badc5).
//!
//! `notify_watchers_push` and `notify_watchers_pr` existed with **no caller
//! anywhere in the tree**: pressing "Watch" wrote a `repo_watches` row, bumped
//! the watchers count, and then produced nothing at all for a push or a PR —
//! the only live watch notification was milestone-closed. This pins the wiring
//! at the point the hooks actually run, so a future refactor that drops the
//! call is a red test rather than a feature that silently stops existing.
//!
//! It also pins the two things the fan-out must *not* do: notify the actor about
//! their own action, and notify a subscriber who cannot read the repository.

use std::path::Path;

use sea_orm::{ActiveValue::NotSet, EntityTrait, Set};

use crate::common::{accepted_push, drain, git, run_post_push_hooks};

async fn fresh_db(dir: &Path) -> sea_orm::DatabaseConnection {
    crate::common::migrated_sqlite(&dir.join("test.db"), 2).await
}

async fn user(db: &sea_orm::DatabaseConnection, name: &str) -> rg_db::entities::user::Model {
    rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.invalid"), "", name)
        .await
        .unwrap_or_else(|e| panic!("create user {name}: {e:#}"))
}

async fn watch(db: &sea_orm::DatabaseConnection, user_id: i64, repo_id: i64, state: &str) {
    rg_db::ops::repo_watch_ops::set_watch_state(db, user_id, repo_id, state)
        .await
        .expect("set watch state");
}

/// The `event_type`s a user has been notified about.
async fn notified_events(db: &sea_orm::DatabaseConnection, user_id: i64) -> Vec<String> {
    rg_db::ops::notification_ops::list_notifications(db, user_id, false)
        .await
        .expect("list notifications")
        .into_iter()
        .map(|notification| notification.event_type)
        .collect()
}

/// Give a PR fixture the same git preconditions a real push creates: a base
/// commit on `main` and a distinct commit on `feature` in the bare repository.
fn seed_pr_branches(bare_path: &Path) {
    let worktree = tempfile::tempdir().expect("create PR fixture worktree");
    let path = worktree.path();
    let path_arg = path.to_str().expect("UTF-8 worktree path");
    let bare_arg = bare_path.to_str().expect("UTF-8 bare repository path");

    git(&["init", "-q", "-b", "main", path_arg], None);
    git(
        &["config", "user.name", "Watch notification test"],
        Some(path),
    );
    git(
        &["config", "user.email", "watch-notification@example.invalid"],
        Some(path),
    );
    std::fs::write(path.join("README.md"), "base\n").expect("write base file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "base"], Some(path));
    git(&["remote", "add", "origin", bare_arg], Some(path));
    git(&["push", "origin", "main"], Some(path));

    git(&["checkout", "-q", "-b", "feature"], Some(path));
    std::fs::write(path.join("feature.txt"), "feature\n").expect("write feature file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "feature"], Some(path));
    git(&["push", "origin", "feature"], Some(path));
}

#[tokio::test]
async fn a_push_notifies_subscribed_watchers_but_not_the_pusher() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let pusher = user(&db, "pushowner").await;
    let watcher = user(&db, "pushwatcher").await;
    let ignorer = user(&db, "pushignorer").await;
    let unwatcher = user(&db, "pushunwatcher").await;

    let repo = rg_core::repo::service::create_repo(
        &db, pusher.id, "pushrepo", None, false, &repo_root, None,
    )
    .await
    .expect("create repo");

    // The pusher watches their own repo too — self-notification is the other
    // half of "the right people get it".
    watch(&db, pusher.id, repo.id, "watching").await;
    watch(&db, watcher.id, repo.id, "watching").await;
    watch(&db, ignorer.id, repo.id, "ignoring").await;
    watch(&db, unwatcher.id, repo.id, "not_watching").await;

    run_post_push_hooks(
        &db,
        &repo_root,
        "pushowner",
        "pushrepo",
        Some(pusher.id),
        &[accepted_push("refs/heads/main", &"a".repeat(40))],
    )
    .await;

    assert_eq!(
        notified_events(&db, watcher.id).await,
        vec!["push".to_string()],
        "a subscribed watcher must get exactly one push notification — the \
         producer used to have no caller at all"
    );
    assert!(
        notified_events(&db, pusher.id).await.is_empty(),
        "the pusher must not be notified about their own push"
    );
    assert!(
        notified_events(&db, ignorer.id).await.is_empty(),
        "watch_state=ignoring must not receive the fan-out"
    );
    assert!(
        notified_events(&db, unwatcher.id).await.is_empty(),
        "watch_state=not_watching (what DELETE /watch writes) must not receive \
         the fan-out"
    );
}

#[tokio::test]
async fn a_rejected_ref_update_notifies_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "rejectowner").await;
    let watcher = user(&db, "rejectwatcher").await;
    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "rejectrepo",
        None,
        false,
        &repo_root,
        None,
    )
    .await
    .expect("create repo");
    watch(&db, watcher.id, repo.id, "watching").await;

    let mut rejected = accepted_push("refs/heads/main", &"b".repeat(40));
    rejected.status = "ng".to_string();
    rejected.message = "protected branch".to_string();

    run_post_push_hooks(
        &db,
        &repo_root,
        "rejectowner",
        "rejectrepo",
        Some(owner.id),
        &[rejected],
    )
    .await;

    assert!(
        notified_events(&db, watcher.id).await.is_empty(),
        "a ref update git refused must not announce a push that never landed"
    );
}

#[tokio::test]
async fn a_watcher_without_read_access_is_not_notified_about_a_push() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "privateowner").await;
    let collaborator = user(&db, "privatecollab").await;
    let outsider = user(&db, "privateoutsider").await;

    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "privaterepo",
        None,
        true,
        &repo_root,
        None,
    )
    .await
    .expect("create private repo");

    rg_db::ops::repo_collaborator_ops::create(
        &db,
        rg_db::entities::repo_collaborator::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            user_id: Set(collaborator.id),
            permission: Set("read".to_string()),
            created_at: Set(chrono::Utc::now()),
        },
    )
    .await
    .expect("add collaborator");

    // Both subscriptions are live rows; only one of them still carries read
    // access to the repository whose branch names the body leaks.
    watch(&db, collaborator.id, repo.id, "watching").await;
    watch(&db, outsider.id, repo.id, "watching").await;
    rg_core::repo::service::invalidate_perm_cache_all(&db);

    run_post_push_hooks(
        &db,
        &repo_root,
        "privateowner",
        "privaterepo",
        Some(owner.id),
        &[accepted_push("refs/heads/secret-branch", &"c".repeat(40))],
    )
    .await;

    assert_eq!(
        notified_events(&db, collaborator.id).await,
        vec!["push".to_string()],
        "a collaborator who can read the private repo must still be notified — \
         otherwise the read gate has traded a leak for an outage"
    );
    assert!(
        notified_events(&db, outsider.id).await.is_empty(),
        "a watcher without read access must not learn a private repo's branch \
         names from a push notification"
    );
}

#[tokio::test]
async fn pull_request_transitions_notify_watchers_but_not_the_actor() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "prowner").await;
    let watcher = user(&db, "prwatcher").await;
    let repo =
        rg_core::repo::service::create_repo(&db, owner.id, "prrepo", None, false, &repo_root, None)
            .await
            .expect("create repo");
    let bare_path = repo_root.join(format!("{}/{}.git", owner.username, repo.name));
    seed_pr_branches(&bare_path);

    watch(&db, owner.id, repo.id, "watching").await;
    watch(&db, watcher.id, repo.id, "watching").await;

    let tracker = rg_core::task_tracker::TaskTracker::new();
    let pr = rg_core::pull_request::create_pr(
        &db,
        &repo_root,
        repo.id,
        owner.id,
        "wire the watch fan-out".to_string(),
        None,
        "feature".to_string(),
        "main".to_string(),
        None,
        false,
        Some(&tracker),
    )
    .await
    .expect("create PR");

    // Checked synchronously, with no `.await` between: this test runs on the
    // current-thread runtime, so a task that has been *queued* rather than run
    // cannot have started yet. That is the whole point of the change — the
    // fan-out is one read check and one insert per subscriber, and the client
    // opening the PR must not be paying for that walk.
    assert_eq!(
        tracker.len(),
        1,
        "create_pr must hand the watch fan-out to the tracker, not walk the \
         subscribers before it returns"
    );

    // Both transitions are announced on the tracker this test owns, so one
    // drain at the end covers them; the assertions in between would otherwise
    // be racing a task that is deliberately not awaited.
    rg_core::pull_request::update_pr(
        &db,
        "prowner",
        "prrepo",
        pr.number,
        None,
        None,
        Some("closed".to_string()),
        None,
        owner.id,
        Some(&tracker),
    )
    .await
    .expect("close PR");

    drain(&tracker).await;

    assert_eq!(
        notified_events(&db, watcher.id).await,
        vec!["pull_request".to_string(), "pull_request".to_string()],
        "opening and closing a PR must both reach the repository's watchers"
    );
    assert!(
        notified_events(&db, owner.id).await.is_empty(),
        "the account behind a PR transition must not be notified about it"
    );
}

/// The fan-out used to be one `limit = 1000` query with no loop behind it, so
/// subscriber #1001 was a row in `repo_watches`, counted as a watcher, and
/// silently unreachable — no log line said anybody had been dropped. The
/// population here is deliberately above that old ceiling.
#[tokio::test]
async fn every_subscriber_is_notified_past_the_old_page_limit() {
    const SUBSCRIBERS: usize = 1200;

    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "capowner").await;
    let repo = rg_core::repo::service::create_repo(
        &db, owner.id, "caprepo", None, false, &repo_root, None,
    )
    .await
    .expect("create repo");

    // Inserted in chunked bulk statements: one round-trip per row would make
    // the fixture, not the fan-out, the slow part of this test, and one
    // statement for all of them would run into SQLite's bound-parameter limit.
    let now = chrono::Utc::now();
    for chunk in (0..SUBSCRIBERS).collect::<Vec<_>>().chunks(40) {
        let users: Vec<rg_db::entities::user::ActiveModel> = chunk
            .iter()
            .map(|i| rg_db::entities::user::ActiveModel {
                id: NotSet,
                username: Set(format!("crowd{i}")),
                email: Set(format!("crowd{i}@example.invalid")),
                password_hash: Set(String::new()),
                display_name: Set(None),
                avatar_url: Set(None),
                bio: Set(None),
                is_admin: Set(false),
                is_active: Set(true),
                auth_provider: Set("local".to_string()),
                ldap_dn: Set(None),
                ldap_uid: Set(None),
                ldap_provider_id: Set(None),
                totp_secret: Set(None),
                mfa_enabled: Set(false),
                mfa_type: Set(None),
                backup_codes: Set(None),
                totp_last_step: Set(None),
                last_login_at: Set(None),
                login_attempts: Set(0),
                locked_until: Set(None),
                session_version: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
            })
            .collect();
        rg_db::entities::user::Entity::insert_many(users)
            .exec(&db)
            .await
            .expect("bulk insert users");
    }

    let subscriber_ids: Vec<i64> = rg_db::ops::user_ops::list_users(&db, 0, 10_000)
        .await
        .expect("list users")
        .0
        .into_iter()
        .filter(|user| user.username.starts_with("crowd"))
        .map(|user| user.id)
        .collect();
    assert_eq!(
        subscriber_ids.len(),
        SUBSCRIBERS,
        "fixture must have created every subscriber"
    );

    for chunk in subscriber_ids.chunks(100) {
        let watches: Vec<rg_db::entities::repo_watch::ActiveModel> = chunk
            .iter()
            .map(|user_id| rg_db::entities::repo_watch::ActiveModel {
                id: NotSet,
                user_id: Set(*user_id),
                repo_id: Set(repo.id),
                watch_state: Set("watching".to_string()),
                created_at: Set(now),
                updated_at: Set(now),
            })
            .collect();
        rg_db::entities::repo_watch::Entity::insert_many(watches)
            .exec(&db)
            .await
            .expect("bulk insert watches");
    }

    run_post_push_hooks(
        &db,
        &repo_root,
        "capowner",
        "caprepo",
        Some(owner.id),
        &[accepted_push("refs/heads/main", &"d".repeat(40))],
    )
    .await;

    let mut unreached = Vec::new();
    for user_id in &subscriber_ids {
        if notified_events(&db, *user_id).await.is_empty() {
            unreached.push(*user_id);
        }
    }
    assert!(
        unreached.is_empty(),
        "{} of {SUBSCRIBERS} subscribers were never notified — the fan-out is \
         capped again, and nothing in the logs would say so",
        unreached.len()
    );
}
