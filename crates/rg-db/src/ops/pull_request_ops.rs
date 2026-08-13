//! Database operations for pull requests.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::pull_request::{self, ActiveModel, Entity as PrEntity, Model as PullRequest};

/// Find a PR by database ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<PullRequest>> {
    PrEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find PR by id")
}

/// Find a PR by (repo_id, number).
pub async fn find_by_repo_and_number(
    db: &DatabaseConnection,
    repo_id: i64,
    number: i64,
) -> Result<Option<PullRequest>> {
    PrEntity::find()
        .filter(pull_request::Column::RepoId.eq(repo_id))
        .filter(pull_request::Column::Number.eq(number))
        .one(db)
        .await
        .context("db: find PR by repo and number")
}

/// Paginated list of PRs for a repo. Returns (data, total).
///
/// Ordered by `created_at` **and** `id`. Imported pull requests carry the
/// upstream timestamp at second precision, so equal keys are ordinary rather
/// than exotic, and a page cut out of an order the database is free to change
/// between requests repeats one PR and hides another.
pub async fn list_by_repo_paginated(
    db: &DatabaseConnection,
    repo_id: i64,
    state: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<PullRequest>, i64)> {
    let mut base = PrEntity::find().filter(pull_request::Column::RepoId.eq(repo_id));
    if let Some(s) = state {
        base = base.filter(pull_request::Column::State.eq(s));
    }
    let query = base
        .order_by_desc(pull_request::Column::CreatedAt)
        .order_by_desc(pull_request::Column::Id);

    let total = query
        .clone()
        .count(db)
        .await
        .context("db: count PRs by repo")? as i64;
    let prs = query
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list PRs by repo (paginated)")?;

    Ok((prs, total))
}

/// Get the next PR number for a repo (max + 1, or 1 if no PRs).
///
/// The answer stops being true the moment anyone else inserts: this read and
/// the write that uses it are separate statements, and `(repo_id, number)` is
/// UNIQUE (`idx_pr_repo_number`). A caller that inserts under this number must
/// therefore treat a backend-confirmed UNIQUE violation as "someone took it,
/// re-read and take the next one" rather than as a failed create — see
/// `rg_core::pull_request::service::insert_with_repo_number`.
///
/// Generic over the connection so the read can be made inside the same
/// transaction as the insert it feeds.
pub async fn next_number<C>(db: &C, repo_id: i64) -> Result<i64>
where
    C: ConnectionTrait,
{
    let max = PrEntity::find()
        .filter(pull_request::Column::RepoId.eq(repo_id))
        .order_by_desc(pull_request::Column::Number)
        .one(db)
        .await
        .context("db: get max PR number")?;
    Ok(max.map(|m| m.number + 1).unwrap_or(1))
}

/// Create a new PR.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<PullRequest> {
    model.insert(db).await.context("db: create PR")
}

/// Update a PR.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<PullRequest> {
    model.update(db).await.context("db: update PR")
}

/// Delete a PR by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    PrEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete PR")?;
    Ok(())
}

fn head_repository_condition(source_repo_id: i64) -> Condition {
    Condition::any()
        .add(
            Condition::all()
                .add(pull_request::Column::RepoId.eq(source_repo_id))
                .add(pull_request::Column::HeadRepoId.is_null()),
        )
        .add(pull_request::Column::HeadRepoId.eq(source_repo_id))
}

/// Result of reconciling the open PRs for one source branch.
#[derive(Debug)]
pub struct OpenHeadShaRefresh {
    /// The current open PR rows after every attempted write.
    pub open_prs: Vec<PullRequest>,
    /// Rows whose snapshot changed before its conditional update could land.
    ///
    /// This is not a database failure: another writer won the compare-and-swap.
    /// It is still observable so the caller can re-read its authoritative ref
    /// and retry instead of reporting a silently stale refresh as successful.
    pub stale_rows: u64,
}

/// Replace one open PR head only while it still has the observed value.
///
/// Returning `false` makes a lost compare-and-swap a first-class outcome. The
/// caller can re-read its authoritative branch ref instead of overwriting the
/// concurrent winner or disguising the race as a database error.
pub async fn compare_and_swap_open_head_sha(
    db: &DatabaseConnection,
    pr_id: i64,
    expected_head: Option<&str>,
    head_sha: Option<&str>,
) -> Result<bool> {
    let expected_head = match expected_head {
        Some(expected) => pull_request::Column::HeadSha.eq(expected),
        None => pull_request::Column::HeadSha.is_null(),
    };
    let result = PrEntity::update_many()
        .col_expr(
            pull_request::Column::HeadSha,
            Expr::value(head_sha.map(str::to_string)),
        )
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("open"))
        .filter(expected_head)
        .exec(db)
        .await
        .context("db: compare-and-swap PR head SHA")?;
    Ok(result.rows_affected == 1)
}

/// Refresh open PR head SHAs after a branch move, including fork PRs whose
/// source repository is the moved repository.
///
/// Each row is updated only while `head_sha` still equals the value selected
/// above. A delayed hook must not turn its stale model into an unconditional
/// `UPDATE` after a newer hook has already published a later commit.
pub async fn update_open_head_sha(
    db: &DatabaseConnection,
    source_repo_id: i64,
    head_branch: &str,
    head_sha: Option<&str>,
) -> Result<OpenHeadShaRefresh> {
    update_open_head_sha_after_read(db, source_repo_id, head_branch, head_sha, || async {
        Ok(())
    })
    .await
}

async fn update_open_head_sha_after_read<F, Fut>(
    db: &DatabaseConnection,
    source_repo_id: i64,
    head_branch: &str,
    head_sha: Option<&str>,
    after_read: F,
) -> Result<OpenHeadShaRefresh>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let prs = PrEntity::find()
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadBranch.eq(head_branch))
        .filter(pull_request::Column::State.eq("open"))
        .all(db)
        .await
        .context("db: find PRs for pushed branch")?;

    // Test seam for a deterministic stale-snapshot interleaving. Production's
    // callback is a no-op; keeping the pause here avoids timing-based race tests.
    after_read().await?;

    let mut stale_rows = 0;
    for pr in prs {
        if pr.head_sha.as_deref() == head_sha {
            continue;
        }

        if !compare_and_swap_open_head_sha(db, pr.id, pr.head_sha.as_deref(), head_sha).await? {
            stale_rows += 1;
        }
    }

    // Return a fresh snapshot for pull_request CI. In particular, a writer
    // that lost its CAS must hand the caller the winner's SHA, never the stale
    // model it selected before the race.
    let open_prs = PrEntity::find()
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadBranch.eq(head_branch))
        .filter(pull_request::Column::State.eq("open"))
        .all(db)
        .await
        .context("db: reload PRs after head SHA refresh")?;

    Ok(OpenHeadShaRefresh {
        open_prs,
        stale_rows,
    })
}

/// Advance PR heads only while they still point at the commit that was used
/// to prepare a server-side update. This prevents a later DB write from
/// overwriting a newer concurrent push notification.
pub async fn advance_open_head_sha(
    db: &DatabaseConnection,
    source_repo_id: i64,
    head_branch: &str,
    expected_head_sha: &str,
    new_head_sha: &str,
) -> Result<u64> {
    let result = PrEntity::update_many()
        .col_expr(
            pull_request::Column::HeadSha,
            Expr::value(Some(new_head_sha.to_string())),
        )
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadBranch.eq(head_branch))
        .filter(pull_request::Column::State.eq("open"))
        .filter(pull_request::Column::HeadSha.eq(expected_head_sha))
        .exec(db)
        .await
        .context("db: advance PR head SHA")?;
    Ok(result.rows_affected)
}

/// Find enabled auto-merge PRs for a source repository commit.
pub async fn list_auto_merge_for_head_commit(
    db: &DatabaseConnection,
    source_repo_id: i64,
    commit_sha: &str,
) -> Result<Vec<PullRequest>> {
    PrEntity::find()
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadSha.eq(commit_sha))
        .filter(pull_request::Column::State.eq("open"))
        .filter(pull_request::Column::AutoMergeEnabled.eq(true))
        .all(db)
        .await
        .context("db: list auto-merge PRs for head commit")
}

pub async fn list_open_for_head_commit(
    db: &DatabaseConnection,
    source_repo_id: i64,
    commit_sha: &str,
) -> Result<Vec<PullRequest>> {
    PrEntity::find()
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadSha.eq(commit_sha))
        .filter(pull_request::Column::State.eq("open"))
        .all(db)
        .await
        .context("db: list open PRs for head commit")
}

/// Atomically claim an auto-merge so concurrent approval/CI/push events cannot
/// merge the same PR twice.
pub async fn claim_auto_merge(db: &DatabaseConnection, pr_id: i64) -> Result<bool> {
    let result = PrEntity::update_many()
        .col_expr(pull_request::Column::AutoMergeEnabled, Expr::value(false))
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("open"))
        .filter(pull_request::Column::AutoMergeEnabled.eq(true))
        .exec(db)
        .await
        .context("db: claim auto-merge")?;
    Ok(result.rows_affected == 1)
}

pub async fn restore_auto_merge(db: &DatabaseConnection, pr_id: i64) -> Result<()> {
    PrEntity::update_many()
        .col_expr(pull_request::Column::AutoMergeEnabled, Expr::value(true))
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("open"))
        .exec(db)
        .await
        .context("db: restore auto-merge")?;
    Ok(())
}

/// Atomically move an open PR into the short-lived `merging` state.
pub async fn claim_merge(db: &DatabaseConnection, pr_id: i64) -> Result<bool> {
    let result = PrEntity::update_many()
        .col_expr(pull_request::Column::State, Expr::value("merging"))
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("open"))
        .exec(db)
        .await
        .context("db: claim PR merge")?;
    Ok(result.rows_affected == 1)
}

pub async fn restore_merge_claim(db: &DatabaseConnection, pr_id: i64) -> Result<()> {
    PrEntity::update_many()
        .col_expr(pull_request::Column::State, Expr::value("open"))
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("merging"))
        .exec(db)
        .await
        .context("db: restore PR merge claim")?;
    Ok(())
}

/// Recover a merge claim left behind by a crashed process after its lease has
/// expired. Returns true only when this call restored the PR.
pub async fn recover_stale_merge_claim(
    db: &DatabaseConnection,
    pr_id: i64,
    cutoff: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let result = PrEntity::update_many()
        .col_expr(pull_request::Column::State, Expr::value("open"))
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("merging"))
        .filter(pull_request::Column::UpdatedAt.lt(cutoff))
        .exec(db)
        .await
        .context("db: recover stale PR merge claim")?;
    Ok(result.rows_affected == 1)
}

/// Record a maintainer's permission for this PR's CI to run, against the head
/// commit they were looking at.
///
/// Conditional on `head_sha` for the same reason the auto-merge claim above is
/// conditional on `state`: the row can move between the caller reading it and
/// this statement running, and an approval written for a head that has since
/// been replaced is an approval of code nobody reviewed. `false` means the head
/// moved under the approver — the caller re-reads and tells them so, rather than
/// stamping the new commit as approved (card_94834ecee708).
pub async fn approve_ci_for_head(
    db: &DatabaseConnection,
    pr_id: i64,
    head_sha: &str,
    approved_by: i64,
) -> Result<bool> {
    let result = PrEntity::update_many()
        .col_expr(
            pull_request::Column::CiApprovedSha,
            Expr::value(head_sha.to_string()),
        )
        .col_expr(pull_request::Column::CiApprovedBy, Expr::value(approved_by))
        .col_expr(
            pull_request::Column::CiApprovedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("open"))
        .filter(pull_request::Column::HeadSha.eq(head_sha))
        .exec(db)
        .await
        .context("db: approve pull-request CI")?;
    Ok(result.rows_affected == 1)
}

#[cfg(test)]
mod head_sha_refresh_tests {
    //! card_9d3b68368396: post-push tasks may finish out of order. A refresh
    //! selected before a newer task committed used to retain a stale full model
    //! and overwrite the newer `head_sha` after the newer task returned success.

    use super::*;
    use crate::entities::repository;

    struct TempDb {
        path: std::path::PathBuf,
    }

    impl TempDb {
        fn new(label: &str) -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "forgekeep-pr-head-refresh-{label}-{}.db",
                    uuid::Uuid::new_v4().simple()
                )),
            }
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

    async fn fixture(label: &str, initial_head: &str) -> (TempDb, DatabaseConnection, i64, i64) {
        let temp = TempDb::new(label);
        let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to throwaway database");
        crate::run_migrations(&db).await.expect("run migrations");

        let owner = crate::ops::user_ops::create_user(
            &db,
            label,
            &format!("{label}@example.invalid"),
            "",
            label,
        )
        .await
        .expect("create PR owner");
        let now = chrono::Utc::now();
        let repo = crate::ops::repo_ops::create(
            &db,
            repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("repo".to_string()),
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
        .expect("create PR repository");
        let pr = create(
            &db,
            ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                number: Set(1),
                title: Set("racing head".to_string()),
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
                head_sha: Set(Some(initial_head.to_string())),
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
                closed_at: Set(None),
                merged_at: Set(None),
            },
        )
        .await
        .expect("create open PR");

        (temp, db, repo.id, pr.id)
    }

    async fn stored_head(db: &DatabaseConnection, pr_id: i64) -> String {
        find_by_id(db, pr_id)
            .await
            .expect("read PR")
            .expect("PR still exists")
            .head_sha
            .expect("open PR has a head")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_newer_head_survives_both_write_orders_and_the_loser_is_visible() {
        const INITIAL: &str = "1111111111111111111111111111111111111111";
        const OLDER: &str = "2222222222222222222222222222222222222222";
        const NEWER: &str = "3333333333333333333333333333333333333333";

        // Ordinary order: the older hook commits, then the newer one advances it.
        let (_temp, db, repo_id, pr_id) = fixture("old-first", INITIAL).await;
        let old = update_open_head_sha(&db, repo_id, "feature", Some(OLDER))
            .await
            .expect("store the older head");
        let new = update_open_head_sha(&db, repo_id, "feature", Some(NEWER))
            .await
            .expect("store the newer head");
        assert_eq!(old.stale_rows, 0);
        assert_eq!(new.stale_rows, 0);
        assert_eq!(stored_head(&db, pr_id).await, NEWER);

        // Overtaken order: the older hook retains INITIAL in memory, the newer
        // hook commits NEWER, and only then is the stale writer released.
        let (_temp, db, repo_id, pr_id) = fixture("new-first", INITIAL).await;
        let older_selected = std::sync::Arc::new(tokio::sync::Notify::new());
        let release_older = std::sync::Arc::new(tokio::sync::Notify::new());
        let selected = older_selected.clone();
        let release = release_older.clone();
        let stale_writer = update_open_head_sha_after_read(
            &db,
            repo_id,
            "feature",
            Some(OLDER),
            move || async move {
                selected.notify_one();
                release.notified().await;
                Ok(())
            },
        );
        let winning_writer = async {
            older_selected.notified().await;
            let result = update_open_head_sha(&db, repo_id, "feature", Some(NEWER)).await;
            release_older.notify_one();
            result
        };

        let (stale, winner) = tokio::join!(stale_writer, winning_writer);
        let winner = winner.expect("the newer writer succeeds");
        let stale = stale.expect("a lost CAS is an outcome, not a database failure");
        assert_eq!(winner.stale_rows, 0);
        assert_eq!(
            stale.stale_rows, 1,
            "the overtaken writer must report its lost compare-and-swap"
        );
        assert_eq!(stored_head(&db, pr_id).await, NEWER);
        assert_eq!(
            stale.open_prs[0].head_sha.as_deref(),
            Some(NEWER),
            "the loser must return the winner's fresh row for pull_request CI"
        );
    }
}
