//! card_7ad8276168a0: `check_merge_allowed` must not read an unparseable
//! `required_status_checks` as "no checks are required".
//!
//! The whole status-check block used to sit inside
//! `if let Ok(required_checks) = serde_json::from_str::<Vec<String>>(..)`, so a
//! column that failed to decode skipped the head-sha lookup, the pipeline
//! lookup and the job comparison in one go, and the function returned `Ok(())`.
//! `require_status_check` stayed `true` in the database and lit up in the UI
//! while the branch merged with nothing checked.
//!
//! The gate is exercised here rather than only through `POST .../merge` because
//! the three cases are not distinguishable at the HTTP layer: past the gate the
//! handler goes on to do real git work, which fails in a fixture with no
//! objects and produces its own 5xx. The service call answers the question the
//! card actually asks — did the gate let this through — with `Ok` and `Err`.
//!
//! `super::super` note: the two cases that *are* observable over HTTP (a corrupt
//! list must not merge the PR; a readable list still refuses with 403) are
//! pinned in `rg-http`'s `undecodable_status_check_tests`.

use sea_orm::{ConnectionTrait, Set};

/// A migrated database with a repository, an open PR onto `main`, and `main`
/// protected by `require_status_check`.
async fn setup(directory: &std::path::Path) -> (sea_orm::DatabaseConnection, i64, i64) {
    let db = crate::common::migrated_sqlite(&directory.join("test.db"), 2).await;

    let owner = rg_db::ops::user_ops::create_user(&db, "gatekeeper", "g@example.invalid", "", "G")
        .await
        .expect("create the account the repository hangs off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            id: sea_orm::NotSet,
            owner_id: Set(owner.id),
            name: Set("gated".to_string()),
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

    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("a pull request to gate".to_string()),
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
    .expect("create the pull request the gate is asked about");

    rg_core::branch_protection::service::create_protection(
        &db,
        "gatekeeper",
        "gated",
        "main".to_string(),
        false,
        true,
        Some(vec!["build".to_string(), "test".to_string()]),
        false,
        None,
        true,
        false,
        None,
    )
    .await
    .expect("protect main");

    (db, repo.id, pr.id)
}

async fn set_checks(db: &sea_orm::DatabaseConnection, repo_id: i64, value: &str) {
    db.execute_unprepared(&format!(
        "UPDATE protected_branches SET required_status_checks = {value} \
         WHERE repo_id = {repo_id} AND branch_name = 'main';"
    ))
    .await
    .expect("rewrite the stored check list");
}

/// The defect: an unreadable rule used to be no rule at all.
#[tokio::test]
async fn an_undecodable_check_list_does_not_open_the_gate() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id) = setup(directory.path()).await;

    // Valid UTF-8 that is not a JSON array of strings — the shape a half-written
    // migration or a hand-edited row leaves behind.
    set_checks(&db, repo_id, "'{\"build\": true}'").await;

    let error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
            .await
            .expect_err("a rule the server cannot read must not be treated as no rule");

    // Not a `Forbidden`: a broken row of ours is not the caller being refused.
    // That distinction is what makes it a 5xx at the HTTP layer instead of a
    // 403 that blames the merger for our storage.
    assert!(
        error.downcast_ref::<rg_core::error::Forbidden>().is_none(),
        "an unreadable rule is our failure, not a policy refusal: {error:#}"
    );
    let chain = format!("{error:#}");
    assert!(
        chain.contains("required_status_checks"),
        "the operator log must name the column that could not be read: {chain}"
    );
}

/// The control. The same rule stored readably reaches the pipeline lookup and
/// refuses there — so the assertion above is comparing against a live gate, not
/// against a call that errors on anything.
#[tokio::test]
async fn a_readable_check_list_reaches_the_pipeline_check_and_refuses() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id) = setup(directory.path()).await;

    let error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
            .await
            .expect_err("no pipeline has run for the head commit, so the gate holds");

    assert!(
        error.downcast_ref::<rg_core::error::Forbidden>().is_some(),
        "a rule the server can read and the PR fails is a policy refusal: {error:#}"
    );
    assert!(
        format!("{error}").contains("no CI pipeline has run"),
        "the refusal must name the rule that refused: {error:#}"
    );
}

/// card_58d3af041513: `NULL` is not a broken row, but it is not an open gate
/// either — it means "no *names* were configured", and the flag beside it still
/// says a status check is required.
///
/// This case used to be pre-registered the other way round ("an absent list
/// must not block the merge"), which is the assertion the defect was hiding
/// behind: `NULL` skipped the head-sha lookup, the pipeline lookup and the job
/// comparison exactly as an undecodable list once did, one `if let Some(..)`
/// further out. The distinction that survives is the *kind* of answer — a
/// broken row is our `Err`, an unmet rule is a `Forbidden` — not whether the
/// gate runs at all.
#[tokio::test]
async fn a_null_check_list_still_requires_a_pipeline() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id) = setup(directory.path()).await;

    set_checks(&db, repo_id, "NULL").await;

    let error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
            .await
            .expect_err("no pipeline has run for the head commit, so the gate holds");

    assert!(
        error.downcast_ref::<rg_core::error::Forbidden>().is_some(),
        "an absent name list is a rule with no names, not a row we failed to \
         read: {error:#}"
    );
    assert!(
        format!("{error}").contains("no CI pipeline has run"),
        "the refusal must name the rule that refused: {error:#}"
    );
}

/// And the empty list stored explicitly answers the same way. `[]` already
/// reached the pipeline lookup before the fix (it decodes to an empty vec and
/// falls through), while `NULL` skipped it — two spellings of one operator
/// instruction with opposite effects. Pinning them together is what keeps the
/// gate from re-acquiring a second dialect.
#[tokio::test]
async fn an_empty_check_list_answers_the_same_as_an_absent_one() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id) = setup(directory.path()).await;

    set_checks(&db, repo_id, "'[]'").await;

    let error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
            .await
            .expect_err("an empty list is still a required status check");

    assert!(
        error.downcast_ref::<rg_core::error::Forbidden>().is_some(),
        "an empty name list is a rule with no names: {error:#}"
    );
    assert!(
        format!("{error}").contains("no CI pipeline has run"),
        "the refusal must name the rule that refused: {error:#}"
    );
}

/// The remaining rules of the same protection row keep working: the gate is not
/// short-circuited by the status-check branch either way.
#[tokio::test]
async fn an_unprotected_branch_is_still_allowed() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id) = setup(directory.path()).await;

    rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "develop", pr_id)
        .await
        .expect("a branch with no protection row has nothing to check");
}
