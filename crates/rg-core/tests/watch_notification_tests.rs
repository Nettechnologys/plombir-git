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

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use sea_orm::{ActiveValue::NotSet, Set};

/// A `CiTrigger` that reports no CI config, so the hook run under test is just
/// the DB-visible half: PR head-SHA refresh, webhooks (none registered) and the
/// watch fan-out. Triggering a pipeline would drag `rg-ci` in for nothing.
struct NoCi;

impl rg_core::ci::CiTrigger for NoCi {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        false
    }

    fn trigger_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        Box::pin(async { anyhow::bail!("no CI config in this test") })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

async fn fresh_db(dir: &Path) -> sea_orm::DatabaseConnection {
    let db_path = dir.join("test.db");
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        5,
        60,
        2,
    )
    .await
    .expect("connect sqlite");
    rg_db::run_migrations(&db).await.expect("run migrations");
    db
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

fn accepted_push(refname: &str, new_sha: &str) -> rg_git::protocol::receive_pack::RefUpdate {
    rg_git::protocol::receive_pack::RefUpdate {
        old_sha: "0".repeat(40),
        new_sha: new_sha.to_string(),
        refname: refname.to_string(),
        status: "ok".to_string(),
        message: String::new(),
    }
}

async fn run_post_push_hooks(
    db: &sea_orm::DatabaseConnection,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
    pusher_id: Option<i64>,
    ref_updates: &[rg_git::protocol::receive_pack::RefUpdate],
) {
    let ci = NoCi;
    rg_core::push_hooks::post_push_hooks(
        &rg_core::push_hooks::PostPushParams {
            db,
            repo_path: &repo_root.join(format!("{owner}/{repo_name}.git")),
            repo_root,
            owner,
            repo_name,
            pusher_id,
            docker_enabled: false,
            external_runners: false,
            allow_host_runner: false,
            jwt_secret: Some("test-secret"),
            notifier: None,
            smtp_config: &None,
            ci_engine: &ci,
            external_url: None,
        },
        ref_updates,
    )
    .await;
}

#[tokio::test]
async fn a_push_notifies_subscribed_watchers_but_not_the_pusher() {
    rg_core::repo::service::invalidate_perm_cache_all();
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let pusher = user(&db, "pushowner").await;
    let watcher = user(&db, "pushwatcher").await;
    let ignorer = user(&db, "pushignorer").await;
    let unwatcher = user(&db, "pushunwatcher").await;

    let repo = rg_core::repo::service::create_repo(
        &db,
        pusher.id,
        "pushrepo",
        None,
        false,
        &repo_root,
        None,
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
    rg_core::repo::service::invalidate_perm_cache_all();
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
    rg_core::repo::service::invalidate_perm_cache_all();
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
    rg_core::repo::service::invalidate_perm_cache_all();

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
    rg_core::repo::service::invalidate_perm_cache_all();
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "prowner").await;
    let watcher = user(&db, "prwatcher").await;
    let repo =
        rg_core::repo::service::create_repo(&db, owner.id, "prrepo", None, false, &repo_root, None)
            .await
            .expect("create repo");

    watch(&db, owner.id, repo.id, "watching").await;
    watch(&db, watcher.id, repo.id, "watching").await;

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
    )
    .await
    .expect("create PR");

    assert_eq!(
        notified_events(&db, watcher.id).await,
        vec!["pull_request".to_string()],
        "opening a PR must reach the repository's watchers"
    );
    assert!(
        notified_events(&db, owner.id).await.is_empty(),
        "the PR author must not be notified about their own PR"
    );

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
    )
    .await
    .expect("close PR");

    assert_eq!(
        notified_events(&db, watcher.id).await,
        vec!["pull_request".to_string(), "pull_request".to_string()],
        "closing a PR must reach the watchers too"
    );
    assert!(
        notified_events(&db, owner.id).await.is_empty(),
        "the account that closed the PR must not be notified about it"
    );
}
