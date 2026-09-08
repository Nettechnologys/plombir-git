//! card_1cf8a3004b6d: a merge that has happened must be handed back, whatever
//! the bookkeeping after it does.
//!
//! `update_pr_merged` runs only once the merge commit is written and
//! `refs/heads/<base>` already points at it — an effect no `?` can take back.
//! Two database writes used to sit between that point and the `Ok`: marking the
//! pull request `merged` and recording its timeline event. Either one failing
//! (SQLite `database is locked` under load, a dropped connection) threw the
//! whole `MergeResult` away, and with it `base_ref_update` — the ref move every
//! caller feeds to `push_hooks::post_push_hooks`. The base branch moved and no
//! pipeline, webhook or watcher ever heard about it, while `POST .../merge`
//! answered 5xx for a merge that did happen.
//!
//! Exercised at the service rather than over HTTP because the `MergeResult` is
//! the subject: the ref move lives in a `#[serde(skip)]` field precisely
//! because it is plumbing for the caller, so the HTTP body cannot show whether
//! it survived.

use sea_orm::{ConnectionTrait, Set};

use crate::common::git;

/// A repository whose bare tree has `main` and a `feature` branch one commit
/// ahead of it, plus the open pull request between them.
struct Fixture {
    db: sea_orm::DatabaseConnection,
    repo_root: std::path::PathBuf,
    repo_id: i64,
    actor_id: i64,
    /// What `main` pointed at before anything merged — the `before` half of the
    /// ref move under test.
    base_before: String,
}

async fn setup(directory: &std::path::Path) -> Fixture {
    let db = crate::common::migrated_sqlite(&directory.join("test.db"), 2).await;
    let repo_root = directory.join("repos");

    let owner = rg_db::ops::user_ops::create_user(&db, "merger", "m@example.invalid", "", "M")
        .await
        .expect("create the account the repository hangs off");
    let repo =
        rg_core::repo::service::create_repo(&db, owner.id, "merged", None, false, &repo_root, None)
            .await
            .expect("create the repository the pull request lives in");

    let bare_path = repo_root.join("merger/merged.git");
    let worktree = tempfile::tempdir().expect("create the seeding worktree");
    let tree = worktree.path();
    let tree_arg = tree.to_str().expect("UTF-8 worktree path");
    let bare_arg = bare_path.to_str().expect("UTF-8 bare repository path");

    git(&["init", "-q", "-b", "main", tree_arg], None);
    git(&["config", "user.name", "Merge test"], Some(tree));
    git(
        &["config", "user.email", "merge@example.invalid"],
        Some(tree),
    );
    std::fs::write(tree.join("base.txt"), "base\n").expect("write the base file");
    git(&["add", "."], Some(tree));
    git(&["commit", "-qm", "base"], Some(tree));
    git(&["remote", "add", "origin", bare_arg], Some(tree));
    git(&["push", "-q", "origin", "main"], Some(tree));

    git(&["checkout", "-q", "-b", "feature"], Some(tree));
    std::fs::write(tree.join("feature.txt"), "feature\n").expect("write the feature file");
    git(&["add", "."], Some(tree));
    git(&["commit", "-qm", "feature"], Some(tree));
    git(&["push", "-q", "origin", "feature"], Some(tree));
    let head_sha = rev_parse(tree, "HEAD");

    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("a pull request whose merge must survive".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(owner.id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some(head_sha)),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            merged_at: Set(None),
            closed_at: Set(None),
        },
    )
    .await
    .expect("create the pull request to merge");

    let base_before = rev_parse(&bare_path, "refs/heads/main");
    Fixture {
        db,
        repo_root,
        repo_id: repo.id,
        actor_id: owner.id,
        base_before,
    }
}

fn rev_parse(repo: &std::path::Path, revision: &str) -> String {
    rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(&["rev-parse", revision], Some(repo))
        .expect("run git rev-parse")
        .stdout_str()
        .trim()
        .to_string()
}

/// The whole point of the `MergeResult`: the caller must be handed the base
/// branch's ref move, and that move must describe the repository as it now is.
fn assert_hooks_can_run(
    merge: &rg_core::pull_request::MergeResult,
    repo_root: &std::path::Path,
    base_before: &str,
) {
    let merged_ref = merge
        .base_ref_update
        .as_ref()
        .expect("the merge moved the base branch, so its ref update must reach the caller");
    assert_eq!(
        merged_ref.update.refname, "refs/heads/main",
        "the hooks are owed the base branch's own ref"
    );
    assert_eq!(
        merged_ref.update.old_sha, base_before,
        "the `before` half must be the tip read before the merge"
    );
    assert_eq!(
        merged_ref.update.new_sha, merge.merge_commit_sha,
        "the `after` half must be the merge commit"
    );
    assert_eq!(
        rev_parse(&repo_root.join("merger/merged.git"), "refs/heads/main"),
        merge.merge_commit_sha,
        "the ref update must describe a move the repository really made"
    );
}

async fn pr_state(db: &sea_orm::DatabaseConnection, repo_id: i64) -> String {
    rg_db::ops::pull_request_ops::find_by_repo_and_number(db, repo_id, 1)
        .await
        .expect("read the pull request back")
        .expect("the pull request row is still there")
        .state
}

/// The timeline write is the last thing between the merge and the `Ok`. A
/// missing `pull_request_merged` event costs the timeline a row; it must not
/// cost the branch its post-push hooks.
#[tokio::test]
async fn a_broken_timeline_write_still_hands_back_the_merge() {
    let directory = tempfile::tempdir().expect("temp dir");
    let fixture = setup(directory.path()).await;
    let Fixture {
        db,
        repo_root,
        repo_id,
        actor_id,
        base_before,
    } = &fixture;

    db.execute_unprepared("DROP TABLE pr_events;")
        .await
        .expect("break the timeline table");

    let merge = rg_core::pull_request::merge_pr(
        db,
        repo_root,
        "merger",
        "merged",
        1,
        *actor_id,
        rg_core::pull_request::MergeStrategy::Merge,
        None,
        None,
    )
    .await
    .expect("a merge that happened must not be reported as a failed merge");

    assert_hooks_can_run(&merge, repo_root, base_before);
    assert_eq!(
        pr_state(db, *repo_id).await,
        "merged",
        "only the timeline write failed, so the pull request itself is merged"
    );
}

/// The state write is the other one. When it fails the row cannot say `merged`
/// — but the commit is on `main` either way, so the hooks are still owed their
/// ref move, and the caller must not be told the merge did not happen.
#[tokio::test]
async fn a_refused_state_write_still_hands_back_the_merge() {
    let directory = tempfile::tempdir().expect("temp dir");
    let fixture = setup(directory.path()).await;
    let Fixture {
        db,
        repo_root,
        repo_id,
        actor_id,
        base_before,
    } = &fixture;

    // Fails only the write that marks the pull request merged: the claim taken
    // on the way in writes `merging` and must still go through, or the merge
    // would never be reached.
    db.execute_unprepared(
        "CREATE TRIGGER pull_requests_merge_write_outage BEFORE UPDATE ON pull_requests \
         WHEN new.state = 'merged' \
         BEGIN SELECT RAISE(ABORT, 'storage is unavailable'); END;",
    )
    .await
    .expect("install the write fault");

    let merge = rg_core::pull_request::merge_pr(
        db,
        repo_root,
        "merger",
        "merged",
        1,
        *actor_id,
        rg_core::pull_request::MergeStrategy::Merge,
        None,
        None,
    )
    .await
    .expect("a merge that happened must not be reported as a failed merge");

    assert_hooks_can_run(&merge, repo_root, base_before);
    // The claim is deliberately left standing. Restoring it would offer a second
    // merge of a branch that is already merged; the 30-minute lease recovery is
    // what settles the row.
    assert_eq!(
        pr_state(db, *repo_id).await,
        "merging",
        "the row could not be marked merged, and it must not be reopened for a \
         second merge of a branch that has already moved"
    );
}
