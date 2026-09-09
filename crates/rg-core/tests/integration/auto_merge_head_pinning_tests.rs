//! card_9ff26bb95dc9: the gate judges `pr.head_sha`, the merge takes the branch.
//!
//! `pr.head_sha` is moved by the post-push hook, which is a detached task, so
//! between "`refs/heads/feature` moved to H2" and "the pull request row says
//! H2" there is a window. Inside it, branch protection counts approvals and
//! looks up status checks for H — and the merge that follows used to take
//! whatever the branch pointed at, i.e. H2: a commit no rule above ever looked
//! at, merged into a protected branch on the strength of another commit's
//! green pipeline.
//!
//! Auto-merge widens the same window on its own: it is woken *by* a green
//! pipeline for one specific commit (`try_auto_merges_for_head_commit` selects
//! pull requests by `head_sha == commit_sha`), so the commit it is about is in
//! the caller's hands and simply never reached the merge.
//!
//! Both halves are pinned here, and separately, because they are two different
//! mechanisms: the caller's own pin (`try_auto_merge` passing `pr.head_sha`)
//! and the gate's verdict (`check_merge_allowed` returning the head it judged,
//! which pins the REST path too). Either one alone would keep a test using
//! both of them green, so each has a fixture the other cannot rescue.

use sea_orm::Set;

use crate::common::git;

struct Fixture {
    _directory: tempfile::TempDir,
    db: sea_orm::DatabaseConnection,
    repo_root: std::path::PathBuf,
    owner: rg_db::entities::user::Model,
    repo: rg_db::entities::repository::Model,
    /// The worktree the fixture pushes from — the author's clone.
    worktree: tempfile::TempDir,
    /// The head commit the pull request row names.
    head_sha: String,
}

/// A repository with `main` and `feature`, and an open pull request from one to
/// the other whose row names the current tip of `feature`.
async fn fixture(name: &str) -> Fixture {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = crate::common::migrated_sqlite(&directory.path().join("test.db"), 2).await;
    let repo_root = directory.path().join("repos");

    let owner = rg_db::ops::user_ops::create_user(
        &db,
        name,
        &format!("{name}@example.invalid"),
        "",
        "Pin Owner",
    )
    .await
    .expect("create the account the repository hangs off");

    let repo =
        rg_core::repo::service::create_repo(&db, owner.id, name, None, false, &repo_root, None)
            .await
            .expect("create the repository");

    let worktree = tempfile::tempdir().expect("create the author's worktree");
    let path = worktree.path();
    let path_arg = path.to_str().expect("UTF-8 worktree path");
    let bare = repo_root.join(format!("{name}/{name}.git"));
    let bare_arg = bare.to_str().expect("UTF-8 bare repository path");

    git(&["init", "-q", "-b", "main", path_arg], None);
    git(&["config", "user.name", "Pin Author"], Some(path));
    git(&["config", "user.email", "pin@example.invalid"], Some(path));
    std::fs::write(path.join("base.txt"), "base\n").expect("write the base file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "base"], Some(path));
    git(&["remote", "add", "origin", bare_arg], Some(path));
    git(&["push", "-q", "origin", "main"], Some(path));

    git(&["checkout", "-q", "-b", "feature"], Some(path));
    std::fs::write(path.join("feature.txt"), "reviewed\n").expect("write the reviewed file");
    git(&["add", "."], Some(path));
    git(
        &["commit", "-qm", "the commit CI went green on"],
        Some(path),
    );
    git(&["push", "-q", "origin", "feature"], Some(path));
    let head_sha = rev_parse(path, "HEAD");

    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("a pull request whose head can move".to_string()),
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
            head_sha: Set(Some(head_sha.clone())),
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
    .expect("create the pull request");

    Fixture {
        _directory: directory,
        db,
        repo_root,
        owner,
        repo,
        worktree,
        head_sha,
    }
}

fn rev_parse(cwd: &std::path::Path, rev: &str) -> String {
    rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(&["rev-parse", rev], Some(cwd))
        .expect("run git rev-parse")
        .stdout_str()
        .trim()
        .to_string()
}

fn bare_path(fixture: &Fixture) -> std::path::PathBuf {
    fixture.repo_root.join(format!(
        "{}/{}.git",
        fixture.owner.username, fixture.repo.name
    ))
}

/// The push an author makes after the review and after CI went green, while the
/// pull request row still names the commit both were about.
fn move_the_head_branch(fixture: &Fixture) -> String {
    let path = fixture.worktree.path();
    std::fs::write(path.join("feature.txt"), "pushed after the verdict\n")
        .expect("rewrite the reviewed file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "pushed after the verdict"], Some(path));
    git(&["push", "-q", "origin", "feature"], Some(path));
    let moved = rev_parse(path, "HEAD");
    assert_ne!(
        moved, fixture.head_sha,
        "the head branch has to actually move"
    );
    assert_eq!(
        rev_parse(&bare_path(fixture), "refs/heads/feature"),
        moved,
        "the served repository is the one whose branch moved"
    );
    moved
}

/// Enable auto-merge on the pull request, the way `PUT .../auto-merge` does.
async fn enable_auto_merge(fixture: &Fixture) {
    rg_core::pull_request::enable_auto_merge(
        &fixture.db,
        &fixture.owner.username,
        &fixture.repo.name,
        1,
        rg_core::pull_request::MergeStrategy::Merge,
        fixture.owner.id,
    )
    .await
    .expect("enable auto-merge");
}

/// Protect `main` with `require_status_check` and give the pull request's head
/// commit the green pipeline that rule looks for.
async fn protect_main_with_a_green_pipeline(fixture: &Fixture) {
    rg_core::branch_protection::service::create_protection(
        &fixture.db,
        &fixture.owner.username,
        &fixture.repo.name,
        "main".to_string(),
        false,
        true,
        None,
        false,
        None,
        true,
        false,
        None,
    )
    .await
    .expect("protect main");

    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &fixture.db,
        fixture.repo.id,
        &fixture.head_sha,
        "refs/heads/feature",
        "push",
        Some(fixture.owner.id),
    )
    .await
    .expect("record the pipeline the rule looks up");
    rg_db::ops::pipeline_ops::update_pipeline_status(
        &fixture.db,
        pipeline.id,
        "success",
        None,
        None,
    )
    .await
    .expect("the pipeline is the green one");
}

async fn try_auto_merge(
    fixture: &Fixture,
) -> anyhow::Result<rg_core::pull_request::AutoMergeOutcome> {
    rg_core::pull_request::try_auto_merge(
        &fixture.db,
        &fixture.repo_root,
        &fixture.owner.username,
        &fixture.repo.name,
        1,
    )
    .await
}

/// The unpinned REST path: `POST .../pulls/{n}/merge` passes no head of its own.
async fn merge_over_rest(fixture: &Fixture) -> anyhow::Result<rg_core::pull_request::MergeResult> {
    rg_core::pull_request::merge_pr(
        &fixture.db,
        &fixture.repo_root,
        &fixture.owner.username,
        &fixture.repo.name,
        1,
        fixture.owner.id,
        rg_core::pull_request::MergeStrategy::Merge,
        None,
        None,
    )
    .await
}

fn assert_head_moved_refusal(error: &anyhow::Error, what: &str) {
    let message = rg_core::error::client_facing_message(error).unwrap_or_else(|| {
        panic!("{what} must be a refusal the caller can read, not a server failure: {error:#}")
    });
    assert_eq!(
        message, "the pull request head moved after it was verified; retry the merge",
        "{what} refused for the wrong reason"
    );
}

/// The card's acceptance case. Auto-merge is woken by the green pipeline of one
/// commit, branch protection counts that commit's checks — and the branch has
/// since moved on. Before the pin this merged the moved head.
#[tokio::test]
async fn a_protected_branch_does_not_auto_merge_a_head_the_rules_never_saw() {
    let fixture = fixture("pinprotected").await;
    protect_main_with_a_green_pipeline(&fixture).await;
    enable_auto_merge(&fixture).await;
    let base_before = rev_parse(&bare_path(&fixture), "refs/heads/main");
    let moved = move_the_head_branch(&fixture);

    let error = try_auto_merge(&fixture)
        .await
        .err()
        .unwrap_or_else(|| panic!("the moved head {moved} must not be merged"));
    assert_head_moved_refusal(&error, "an auto-merge onto a protected branch");

    assert_eq!(
        rev_parse(&bare_path(&fixture), "refs/heads/main"),
        base_before,
        "the protected branch must not have moved: the only commit with approvals \
         and a green pipeline is no longer what the head branch names"
    );
}

/// The caller's own pin, with the gate's verdict removed from the picture: on an
/// unprotected base branch `check_merge_allowed` judges no head at all, so the
/// only thing standing between the green pipeline's commit and the branch tip is
/// `try_auto_merge` passing `pr.head_sha` down.
#[tokio::test]
async fn auto_merge_does_not_merge_a_head_it_was_not_woken_for() {
    let fixture = fixture("pinunprotected").await;
    enable_auto_merge(&fixture).await;
    let base_before = rev_parse(&bare_path(&fixture), "refs/heads/main");
    let moved = move_the_head_branch(&fixture);

    let error = try_auto_merge(&fixture)
        .await
        .err()
        .unwrap_or_else(|| panic!("the moved head {moved} must not be merged"));
    assert_head_moved_refusal(&error, "an auto-merge of a head that moved");

    assert_eq!(
        rev_parse(&bare_path(&fixture), "refs/heads/main"),
        base_before,
        "auto-merge merged a commit no pipeline of this pull request ever ran on"
    );
}

/// The gate's verdict, with the caller's pin removed from the picture: the REST
/// path pins nothing itself, so what refuses here is `check_merge_allowed`
/// carrying the head it judged. Without it, a protected branch merges a commit
/// whose approvals and status checks belong to an earlier one.
#[tokio::test]
async fn the_rest_path_merges_the_commit_branch_protection_judged() {
    let fixture = fixture("pinrest").await;
    protect_main_with_a_green_pipeline(&fixture).await;
    let base_before = rev_parse(&bare_path(&fixture), "refs/heads/main");
    let moved = move_the_head_branch(&fixture);

    let error = merge_over_rest(&fixture)
        .await
        .err()
        .unwrap_or_else(|| panic!("the moved head {moved} must not be merged"));
    assert_head_moved_refusal(&error, "a REST merge onto a protected branch");

    assert_eq!(
        rev_parse(&bare_path(&fixture), "refs/heads/main"),
        base_before,
        "the protected branch moved onto a commit its own rules never judged"
    );
}

/// The other half, and the one a pin that refuses everything would break: a head
/// that stayed put is the head both the rule and the caller are about, so both
/// paths still merge it.
#[tokio::test]
async fn a_head_that_stayed_put_still_merges_on_both_paths() {
    let auto = fixture("pinstillauto").await;
    protect_main_with_a_green_pipeline(&auto).await;
    enable_auto_merge(&auto).await;
    let outcome = try_auto_merge(&auto)
        .await
        .expect("auto-merge the verified head");
    assert_eq!(
        outcome.status, "merged",
        "the head never moved, so nothing was there to refuse: {:?}",
        outcome.reason
    );
    assert_ne!(
        rev_parse(&bare_path(&auto), "refs/heads/main"),
        rev_parse(&bare_path(&auto), "refs/heads/feature"),
        "a merge commit, not a fast-forward, is what this strategy makes"
    );

    let rest = fixture("pinstillrest").await;
    protect_main_with_a_green_pipeline(&rest).await;
    let base_before = rev_parse(&bare_path(&rest), "refs/heads/main");
    merge_over_rest(&rest)
        .await
        .expect("merge the verified head");
    assert_ne!(
        rev_parse(&bare_path(&rest), "refs/heads/main"),
        base_before,
        "the REST path refused a head that had not moved"
    );
}

/// The third site of the same root, one step upstream of the merge: the diff a
/// reviewer approves.
///
/// It used to be computed from `refs/heads/<head>` — the branch tip — while the
/// approval the reviewer then submits is recorded against `pr.head_sha`, and the
/// merge is now pinned to that same commit. Left as it was, the fix above would
/// have moved the window rather than closed it: the reviewer would read the
/// content of H2 and hand a green light to H.
#[tokio::test]
async fn the_diff_shows_the_commit_the_review_will_be_recorded_against() {
    let fixture = fixture("pindiff").await;
    let moved = move_the_head_branch(&fixture);

    let diff = rg_core::pull_request::compute_diff(
        &fixture.db,
        &fixture.repo_root,
        &fixture.owner.username,
        &fixture.repo.name,
        1,
    )
    .await
    .expect("compute the pull request diff");

    let patch = diff
        .files_changed
        .iter()
        .find(|file| file.path == "feature.txt")
        .and_then(|file| file.patch.clone())
        .expect("the changed file carries its patch");
    assert!(
        patch.contains("reviewed"),
        "the diff must show the head the pull request row names ({}), \
         which is what an approval submitted now is recorded against: {patch}",
        fixture.head_sha
    );
    assert!(
        !patch.contains("pushed after the verdict"),
        "the diff shows the branch tip {moved}, which no approval will cover: {patch}"
    );
}
