//! card_95b947a371fc: the diff of a pull request that is no longer open.
//!
//! "Files changed" is what a pull request changed, and that does not stop being
//! true when it merges or its branch is deleted. Most pull requests a project
//! brings along when it moves here are exactly that — merged, branch gone —
//! and so is most of any repository's own history.

use sea_orm::Set;

use crate::common::git;

struct Fixture {
    _directory: tempfile::TempDir,
    db: sea_orm::DatabaseConnection,
    repo_root: std::path::PathBuf,
    owner: rg_db::entities::user::Model,
    repo: rg_db::entities::repository::Model,
    bare: std::path::PathBuf,
    head_sha: String,
}

/// `main` with one commit, `feature` adding `feature.txt` on top, and an open
/// pull request from `feature` into `main` whose row names that commit.
async fn fixture(name: &str) -> Fixture {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = crate::common::migrated_sqlite(&directory.path().join("test.db"), 2).await;
    let repo_root = directory.path().join("repos");

    let owner = rg_db::ops::user_ops::create_user(
        &db,
        name,
        &format!("{name}@example.invalid"),
        "",
        "Diff Owner",
    )
    .await
    .expect("create the account the repository hangs off");
    let repo =
        rg_core::repo::service::create_repo(&db, owner.id, name, None, false, &repo_root, None)
            .await
            .expect("create the repository");

    let worktree = tempfile::tempdir().expect("create the author's worktree");
    let path = worktree.path();
    let bare = repo_root.join(format!("{name}/{name}.git"));
    let bare_arg = bare.to_str().expect("UTF-8 bare repository path");
    git(&["init", "-q", "-b", "main", path.to_str().unwrap()], None);
    git(&["config", "user.name", "Diff Author"], Some(path));
    git(
        &["config", "user.email", "diff@example.invalid"],
        Some(path),
    );
    std::fs::write(path.join("base.txt"), "base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "base"], Some(path));
    git(&["remote", "add", "origin", bare_arg], Some(path));
    git(&["push", "-q", "origin", "main"], Some(path));
    git(&["checkout", "-q", "-b", "feature"], Some(path));
    std::fs::write(path.join("feature.txt"), "what the pull request adds\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "the pull request's change"], Some(path));
    git(&["push", "-q", "origin", "feature"], Some(path));
    let head_sha = rev_parse(&bare, "refs/heads/feature");

    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("a pull request that will merge".to_string()),
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
        bare,
        head_sha,
    }
}

fn rev_parse(repo: &std::path::Path, rev: &str) -> String {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(&["rev-parse", rev], Some(repo))
        .expect("run git rev-parse");
    output.ensure_success().expect("rev-parse succeeds");
    output.stdout_str().trim().to_string()
}

async fn merge(fixture: &Fixture, strategy: rg_core::pull_request::MergeStrategy) {
    rg_core::pull_request::merge_pr(
        &fixture.db,
        &fixture.repo_root,
        &fixture.owner.username,
        &fixture.repo.name,
        1,
        fixture.owner.id,
        strategy,
        None,
        None,
        &Default::default(),
    )
    .await
    .expect("merge the pull request");
}

async fn changed_paths(fixture: &Fixture) -> anyhow::Result<Vec<String>> {
    let diff = rg_core::pull_request::compute_diff(
        &fixture.db,
        &fixture.repo_root,
        &fixture.owner.username,
        &fixture.repo.name,
        1,
    )
    .await?;
    let mut paths: Vec<String> = diff
        .files_changed
        .into_iter()
        .map(|file| file.path)
        .collect();
    paths.sort();
    Ok(paths)
}

fn delete_head_branch(fixture: &Fixture) {
    git(
        &["update-ref", "-d", "refs/heads/feature"],
        Some(&fixture.bare),
    );
}

/// The open pull request is the baseline every other case is held to.
#[tokio::test]
async fn an_open_pull_request_shows_its_change() {
    let fixture = fixture("diffopen").await;
    assert_eq!(changed_paths(&fixture).await.unwrap(), ["feature.txt"]);
}

/// A merge commit makes the head an ancestor of the base, so a diff measured
/// from the base's tip *now* finds nothing between them.
#[tokio::test]
async fn a_merged_pull_request_still_shows_what_it_changed() {
    for strategy in [
        rg_core::pull_request::MergeStrategy::Merge,
        rg_core::pull_request::MergeStrategy::Squash,
        rg_core::pull_request::MergeStrategy::Rebase,
    ] {
        let fixture = fixture("diffmerged").await;
        merge(&fixture, strategy).await;
        assert_eq!(
            changed_paths(&fixture).await.unwrap(),
            ["feature.txt"],
            "{strategy:?}"
        );
    }
}

/// Deleting the branch after the merge is the ordinary end of a pull request;
/// its commit is still in the repository, and so is what it changed.
#[tokio::test]
async fn a_merged_pull_request_whose_branch_was_deleted_still_shows_what_it_changed() {
    let fixture = fixture("diffgone").await;
    merge(&fixture, rg_core::pull_request::MergeStrategy::Merge).await;
    delete_head_branch(&fixture);

    assert_eq!(
        changed_paths(&fixture)
            .await
            .expect("a merged pull request's diff does not need its branch"),
        ["feature.txt"]
    );
    assert!(fixture.head_sha.len() == 40);
}

/// What an import leaves: the pull request merged on its source, its branch
/// never arrived, its head kept only under `refs/imported-pulls/`, and the row
/// naming the base the source recorded — no merge commit made here.
#[tokio::test]
async fn an_imported_merged_pull_request_shows_what_it_changed() {
    use sea_orm::{ActiveModelTrait, EntityTrait};

    let fixture = fixture("diffimported").await;
    let base_before = rev_parse(&fixture.bare, "refs/heads/main");
    // The source merged it with a merge commit, which the clone brought along.
    let work = tempfile::tempdir().expect("merge worktree");
    git(
        &[
            "clone",
            "-q",
            fixture.bare.to_str().unwrap(),
            work.path().to_str().unwrap(),
        ],
        None,
    );
    git(&["config", "user.name", "Source"], Some(work.path()));
    git(
        &["config", "user.email", "source@example.invalid"],
        Some(work.path()),
    );
    git(
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "merged on the source",
            "origin/feature",
        ],
        Some(work.path()),
    );
    git(&["push", "-q", "origin", "main"], Some(work.path()));
    git(
        &[
            "update-ref",
            "refs/imported-pulls/1/head",
            &fixture.head_sha,
        ],
        Some(&fixture.bare),
    );
    delete_head_branch(&fixture);

    let row = rg_db::entities::pull_request::Entity::find()
        .one(&fixture.db)
        .await
        .unwrap()
        .expect("the pull request row");
    let mut active: rg_db::entities::pull_request::ActiveModel = row.into();
    active.state = Set("merged".to_string());
    active.base_sha = Set(Some(base_before));
    active.update(&fixture.db).await.expect("mark it merged");

    assert_eq!(
        changed_paths(&fixture)
            .await
            .expect("the imported pull request's diff needs neither branch nor merge commit"),
        ["feature.txt"]
    );
}

/// A pull request closed without merging keeps reading the base branch's tip,
/// which does not hold its head — and needs no branch of its own for that.
#[tokio::test]
async fn a_pull_request_closed_unmerged_shows_its_change_after_the_branch_is_gone() {
    use sea_orm::{ActiveModelTrait, EntityTrait};

    let fixture = fixture("diffclosed").await;
    let row = rg_db::entities::pull_request::Entity::find()
        .one(&fixture.db)
        .await
        .unwrap()
        .expect("the pull request row");
    let mut active: rg_db::entities::pull_request::ActiveModel = row.into();
    active.state = Set("closed".to_string());
    active.update(&fixture.db).await.expect("close it");
    // Kept so the closed head is still in the repository, as a merge or an
    // import would have kept it.
    git(
        &[
            "update-ref",
            "refs/imported-pulls/1/head",
            &fixture.head_sha,
        ],
        Some(&fixture.bare),
    );
    delete_head_branch(&fixture);

    assert_eq!(changed_paths(&fixture).await.unwrap(), ["feature.txt"]);
}
