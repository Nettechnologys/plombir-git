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
async fn setup(directory: &std::path::Path) -> (sea_orm::DatabaseConnection, i64, i64, i64) {
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
            base_sha: Set(None),
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
            original_author: sea_orm::NotSet,
            original_author_id: sea_orm::NotSet,
            original_service: sea_orm::NotSet,
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

    (db, repo.id, pr.id, owner.id)
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
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

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
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

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
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

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
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

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
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

    rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "develop", pr_id)
        .await
        .expect("a branch with no protection row has nothing to check");
}

/// card_af64c0807560: the public core merge service is the security boundary,
/// not merely a Git helper behind the REST extractor. A library caller that
/// names an outsider must be refused before branch-policy details or Git state
/// become observable.
#[tokio::test]
async fn merge_service_requires_current_repository_write_access() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, _, _, _) = setup(directory.path()).await;
    let outsider = rg_db::ops::user_ops::create_user(
        &db,
        "merge-outsider",
        "merge-outsider@example.invalid",
        "",
        "Outsider",
    )
    .await
    .expect("create outsider");

    let error = rg_core::pull_request::merge_pr(
        &db,
        directory.path(),
        "gatekeeper",
        "gated",
        1,
        outsider.id,
        rg_core::pull_request::MergeStrategy::Merge,
        None,
        None,
        &Default::default(),
    )
    .await
    .expect_err("a library caller cannot merge on behalf of an outsider");

    assert!(
        error.downcast_ref::<rg_core::error::Forbidden>().is_some(),
        "missing write access is a policy refusal: {error:#}"
    );
    assert!(
        format!("{error}").contains("write access denied"),
        "the refusal must name the missing repository permission: {error:#}"
    );
}

/// The actor id is durable provenance for auto-merge and merge-queue entries,
/// not a permanent grant. If that account has since been deactivated, its old
/// owner/collaborator identity must not authorize a delayed merge.
#[tokio::test]
async fn merge_service_requires_an_active_actor() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, _, _, owner_id) = setup(directory.path()).await;
    db.execute_unprepared(&format!(
        "UPDATE users SET is_active = 0 WHERE id = {owner_id};"
    ))
    .await
    .expect("deactivate the merge actor");

    let error = rg_core::pull_request::merge_pr(
        &db,
        directory.path(),
        "gatekeeper",
        "gated",
        1,
        owner_id,
        rg_core::pull_request::MergeStrategy::Merge,
        None,
        None,
        &Default::default(),
    )
    .await
    .expect_err("a durable actor id must not outlive account standing");

    assert!(
        error.downcast_ref::<rg_core::error::Forbidden>().is_some(),
        "an inactive actor is a policy refusal: {error:#}"
    );
    assert!(
        format!("{error}").contains("active account"),
        "the refusal must name the actor-standing failure: {error:#}"
    );
}

/// A write-capable actor still cannot bypass the target branch's merge rules by
/// calling `rg-core` directly. The fixture deliberately has no Git repository:
/// reaching Git would prove that the policy layer was skipped.
#[tokio::test]
async fn merge_service_rechecks_branch_protection_before_git() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, _, _, owner_id) = setup(directory.path()).await;

    let error = rg_core::pull_request::merge_pr(
        &db,
        directory.path(),
        "gatekeeper",
        "gated",
        1,
        owner_id,
        rg_core::pull_request::MergeStrategy::Merge,
        None,
        None,
        &Default::default(),
    )
    .await
    .expect_err("the protected branch has no successful pipeline");

    assert!(
        error.downcast_ref::<rg_core::error::Forbidden>().is_some(),
        "an unmet readable rule is a policy refusal: {error:#}"
    );
    assert!(
        format!("{error}").contains("no CI pipeline has run"),
        "the core service must stop at branch protection, before Git: {error:#}"
    );
}

/// A finished pipeline on the fixture's head commit, with one job per entry of
/// `jobs` (`(name, status)`), recorded under `ref_name` by `trigger_type`.
async fn finished_pipeline(
    db: &sea_orm::DatabaseConnection,
    repo_id: i64,
    ref_name: &str,
    trigger_type: &str,
    status: &str,
    jobs: &[(&str, &str)],
) -> i64 {
    use rg_db::ops::pipeline_ops;

    let head = "a".repeat(40);
    let pipeline = pipeline_ops::create_pipeline_row(
        db,
        pipeline_ops::NewPipeline {
            repo_id,
            commit_sha: &head,
            ref_name,
            trigger_type,
            triggered_by: None,
            concurrency_group: None,
            dispatch_inputs: None,
            base_branch: None,
            head_branch: None,
            previous_sha: None,
            schedule: None,
            run_name: None,
        },
    )
    .await
    .expect("create a pipeline row");
    let stage = pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create a stage");
    let now = chrono::Utc::now().naive_utc();
    for (name, job_status) in jobs {
        let job = pipeline_ops::create_job(
            db, stage.id, name, "", None, None, None, None, None, None, false, None, None, None,
        )
        .await
        .expect("create a job");
        pipeline_ops::update_job_result(
            db,
            job.id,
            job_status,
            Some(0),
            None,
            Some(now),
            Some(now),
        )
        .await
        .expect("finish the job");
    }
    pipeline_ops::update_pipeline_status(db, pipeline.id, status, Some(now), Some(now))
        .await
        .expect("finish the pipeline");
    pipeline.id
}

const PULL_REQUEST_REF: &str = "refs/pull/1/head";
const BRANCH_REF: &str = "refs/heads/feature";
const GREEN: &[(&str, &str)] = &[("build", "success"), ("test", "success")];

/// card_1d3af3ad4f55, the defect as it met a live pull request: the pull
/// request's own run is green, and a push to its branch then published a red
/// pipeline on the same head — after it. The gate took "the latest pipeline on
/// the commit" and refused the merge.
#[tokio::test]
async fn a_later_red_branch_run_does_not_overrule_the_pull_requests_green_run() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

    finished_pipeline(
        &db,
        repo_id,
        PULL_REQUEST_REF,
        "pull_request",
        "success",
        GREEN,
    )
    .await;
    finished_pipeline(
        &db,
        repo_id,
        BRANCH_REF,
        "push",
        "failed",
        &[("CI configuration", "failed")],
    )
    .await;

    rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
        .await
        .expect("the pull request's own green run is the verdict");
}

/// The other side: the pull request's run is the verdict, so a red one is not
/// rescued by a green branch run that happens to be newer.
#[tokio::test]
async fn a_later_green_branch_run_does_not_rescue_the_pull_requests_red_run() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

    let red = finished_pipeline(
        &db,
        repo_id,
        PULL_REQUEST_REF,
        "pull_request",
        "failed",
        &[("build", "failed"), ("test", "success")],
    )
    .await;
    finished_pipeline(&db, repo_id, BRANCH_REF, "push", "success", GREEN).await;

    let error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
            .await
            .expect_err("the pull request's red run holds the gate");
    assert!(
        format!("{error}").contains(&format!("pipeline #{red} is failed")),
        "the refusal must name the pull request's run: {error:#}"
    );
}

/// A repository with only `on: push` workflows has no pull request runs at all,
/// and its pull requests are gated on the branch's run — as before.
#[tokio::test]
async fn without_a_pull_request_run_the_branch_run_is_the_verdict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

    let red = finished_pipeline(
        &db,
        repo_id,
        BRANCH_REF,
        "push",
        "failed",
        &[("build", "failed")],
    )
    .await;
    let error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
            .await
            .expect_err("a red branch run holds the gate when it is the only run");
    assert!(
        format!("{error}").contains(&format!("pipeline #{red} is failed")),
        "{error:#}"
    );

    finished_pipeline(&db, repo_id, BRANCH_REF, "push", "success", GREEN).await;
    rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
        .await
        .expect("the newest branch run is green");
}

/// Another pull request on the same head commit — the same branch proposed
/// into a second base — has runs of its own, and they are not this one's.
#[tokio::test]
async fn another_pull_requests_run_is_not_this_ones_verdict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

    finished_pipeline(
        &db,
        repo_id,
        "refs/pull/2/head",
        "pull_request",
        "success",
        GREEN,
    )
    .await;

    let error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
            .await
            .expect_err("pull request #2's green run says nothing about #1");
    assert!(
        format!("{error}").contains("no CI pipeline has run"),
        "{error:#}"
    );
}

/// A required check only an `on: push` workflow declares is absent from the
/// pull request's run, not failed in it: it is judged by the branch's run, the
/// way it was before the pull request's run became the verdict. A check the
/// pull request's run *did* have is judged there and nowhere else.
#[tokio::test]
async fn a_required_check_only_the_branch_run_has_is_judged_there() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id, pr_id, _) = setup(directory.path()).await;

    finished_pipeline(
        &db,
        repo_id,
        PULL_REQUEST_REF,
        "pull_request",
        "success",
        &[("test", "success")],
    )
    .await;
    finished_pipeline(
        &db,
        repo_id,
        BRANCH_REF,
        "push",
        "failed",
        &[("build", "failed"), ("test", "success")],
    )
    .await;
    let error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
            .await
            .expect_err("`build` failed on the branch, and only the branch runs it");
    assert!(format!("{error}").contains("[\"build\"]"), "{error:#}");

    finished_pipeline(
        &db,
        repo_id,
        BRANCH_REF,
        "push",
        "success",
        &[("build", "success"), ("test", "failed")],
    )
    .await;
    rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id)
        .await
        .expect(
            "`build` passed on the branch; `test` is the pull request's own check, and it passed",
        );
}
