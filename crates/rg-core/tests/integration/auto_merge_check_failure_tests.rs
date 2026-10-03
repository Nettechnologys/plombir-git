//! card_af2abe7904bd: an enabled auto-merge asks branch protection whether the
//! pull request may merge, and that one `Err` carries two unrelated answers.
//!
//! A rule that *refused* — too few approvals, a status check that has not
//! passed — is a condition the caller can wait out, and `try_auto_merge`
//! reports it as a `pending` outcome the way it always has. A rule the server
//! could not *read* refused nothing: reporting it as `pending` tells whoever
//! enabled auto-merge that the merge is waiting on a condition nobody
//! evaluated, and `PUT .../auto-merge` serialises that outcome into a `200`
//! carrying the failed read's `db: …` chain as the explanation (H-05).
//!
//! Exercised at the service rather than over HTTP because the outcome is the
//! subject: past the gate `try_auto_merge` goes on to real git work that a
//! fixture with no objects fails on its own terms.

use sea_orm::{ConnectionTrait, Set};

/// A migrated database with a repository and an open pull request onto `main`
/// whose auto-merge is enabled and authorized.
async fn setup(
    directory: &std::path::Path,
) -> (
    sea_orm::DatabaseConnection,
    rg_db::entities::repository::Model,
) {
    let db = crate::common::migrated_sqlite(&directory.join("test.db"), 2).await;

    let owner = rg_db::ops::user_ops::create_user(&db, "automerger", "a@example.invalid", "", "A")
        .await
        .expect("create the account the repository hangs off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            id: sea_orm::NotSet,
            owner_id: Set(owner.id),
            name: Set("auto".to_string()),
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
    .expect("create the repository the rule hangs off");

    rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("a pull request to merge automatically".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(true),
            auto_merge_strategy: Set(Some("merge".to_string())),
            auto_merge_enabled_by_id: Set(Some(owner.id)),
            auto_merge_enabled_at: Set(Some(now)),
            author_id: Set(owner.id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some("a".repeat(40))),
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
    .expect("create the pull request auto-merge is enabled on");

    (db, repo)
}

/// The defect: a failed read of the protection rule used to be answered as
/// "your merge is pending", with the read's own chain as the reason.
#[tokio::test]
async fn a_protection_rule_that_could_not_be_read_is_not_a_pending_merge() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, _repo) = setup(directory.path()).await;

    // Any failure of the read would do — an unreachable database, a dropped
    // pool. The table going away is the one whose message is unmistakably
    // internal, and it is where the rule-list lookup puts its context.
    db.execute_unprepared("DROP TABLE protected_branches")
        .await
        .expect("take branch-protection storage away");

    let error = rg_core::pull_request::try_auto_merge(
        &db,
        &directory.path().join("repos"),
        "automerger",
        "auto",
        1,
    )
    .await
    .expect_err("a check that never ran is not a merge that is pending");

    // The chain reaches the caller, which is what logs it: `AppError`'s `{:#}`
    // funnel on `PUT .../auto-merge`, a `tracing::warn!` on the push and
    // CI-completion hooks. Nothing of it was handed to the client on the way.
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("branch protection of 'main' could not be checked"),
        "auto-merge must name what it could not check: {rendered}"
    );
    assert!(
        rendered.contains("db: list protected branches by repo"),
        "the failed read must reach the operator whole: {rendered}"
    );
}

/// The other half, and the regression card_a997f30c142c left behind: a rule
/// that genuinely refuses is a condition to wait for, and it reaches the caller
/// in the words branch protection wrote.
#[tokio::test]
async fn a_rule_that_refused_is_still_a_pending_merge_in_its_own_words() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, _repo) = setup(directory.path()).await;

    rg_core::branch_protection::service::create_protection(
        &db,
        "automerger",
        "auto",
        "main".to_string(),
        false,
        false,
        None,
        true,
        Some(2),
        false,
        false,
        None,
    )
    .await
    .expect("protect the base branch");

    let outcome = rg_core::pull_request::try_auto_merge(
        &db,
        &directory.path().join("repos"),
        "automerger",
        "auto",
        1,
    )
    .await
    .expect("a rule that refused is a pending outcome, not a failure");

    assert_eq!(outcome.status, "pending");
    assert_eq!(
        outcome.reason.as_deref(),
        Some("merging into protected branch 'main' requires at least 2 approval(s), got 0"),
        "the refusal reaches the caller naming the rule and the count: {outcome:?}"
    );
}
