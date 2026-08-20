//! A repository holding a full history must never keep reading as empty.
//!
//! `create_repo` points `HEAD` at the branch the create request named, and
//! nothing moved it afterwards. Push a history that lives on `master` into a
//! repository created with `main` — every repository imported by hand from an
//! older tree — and `HEAD` stayed unborn next to a complete object store. Every
//! read that resolves the default ref then answered "empty repository": the
//! repository page rendered its "push an existing repository" setup screen,
//! `git clone` warned about a nonexistent remote HEAD and checked out nothing,
//! and no message anywhere said the branch was merely spelled differently.
//!
//! Pinned here: the adoption happens, it is bounded to a genuinely unborn
//! `HEAD` (so a feature-branch push can never rewrite a live default), and the
//! database row moves with Git rather than after it.

use std::path::Path;

use crate::common::{accepted_push, git, run_post_push_hooks};

async fn fresh_db(dir: &Path) -> sea_orm::DatabaseConnection {
    crate::common::migrated_sqlite(&dir.join("test.db"), 2).await
}

async fn user(db: &sea_orm::DatabaseConnection, name: &str) -> rg_db::entities::user::Model {
    rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.invalid"), "", name)
        .await
        .unwrap_or_else(|error| panic!("create user {name}: {error:#}"))
}

/// Commit once on `branch` in a worktree wired to `bare_path`, push it, and
/// return the SHA the transport would report to the hooks.
fn push_one_commit(bare_path: &Path, branch: &str, worktree: &Path) -> String {
    let path_arg = worktree.to_str().expect("UTF-8 worktree path");
    let bare_arg = bare_path.to_str().expect("UTF-8 bare repository path");

    git(&["init", "-q", "-b", branch, path_arg], None);
    git(&["config", "user.name", "Unborn head test"], Some(worktree));
    git(
        &["config", "user.email", "unborn-head@example.invalid"],
        Some(worktree),
    );
    std::fs::write(worktree.join("file.txt"), "contents\n").expect("write the seed file");
    git(&["add", "."], Some(worktree));
    git(&["commit", "-qm", "seed"], Some(worktree));
    git(&["remote", "add", "origin", bare_arg], Some(worktree));
    git(&["push", "-q", "origin", branch], Some(worktree));

    rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(&["rev-parse", "HEAD"], Some(worktree))
        .expect("run git rev-parse")
        .stdout_str()
        .trim()
        .to_string()
}

/// The symbolic target of the bare repository's `HEAD`, and whether it resolves
/// to a commit — the two facts every default-ref read depends on.
fn head_state(bare_path: &Path) -> (Option<String>, bool) {
    let advertisement =
        rg_git::ref_advertisement::collect(bare_path).expect("read the repository advertisement");
    (advertisement.head_target, advertisement.head_oid.is_some())
}

async fn default_branch(db: &sea_orm::DatabaseConnection, repo_id: i64) -> String {
    rg_db::ops::repo_ops::find_by_id(db, repo_id)
        .await
        .expect("read the repository row")
        .expect("the repository row still exists")
        .default_branch
}

/// The card itself: a first push onto a branch the repository was not created
/// with leaves both Git and the row naming that branch.
#[tokio::test]
async fn a_first_push_to_another_branch_becomes_the_repository_default() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "unbornowner").await;
    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "unbornrepo",
        None,
        false,
        &repo_root,
        None,
    )
    .await
    .expect("create repo");
    let bare_path = repo_root.join("unbornowner/unbornrepo.git");

    assert_eq!(
        head_state(&bare_path),
        (Some("refs/heads/main".to_string()), false),
        "a freshly created repository points HEAD at its unborn default branch"
    );

    let worktree = tempfile::tempdir().expect("create the pusher's worktree");
    let new_sha = push_one_commit(&bare_path, "master", worktree.path());

    // Before the hooks run, this is exactly the broken state users reported: a
    // full history behind a HEAD that resolves to nothing.
    assert_eq!(
        head_state(&bare_path),
        (Some("refs/heads/main".to_string()), false),
        "the push alone does not move HEAD"
    );

    run_post_push_hooks(
        &db,
        &repo_root,
        "unbornowner",
        "unbornrepo",
        Some(owner.id),
        &[accepted_push("refs/heads/master", &new_sha)],
    )
    .await;

    assert_eq!(
        head_state(&bare_path),
        (Some("refs/heads/master".to_string()), true),
        "HEAD must name the branch the push created, and resolve to its commit"
    );
    assert_eq!(
        default_branch(&db, repo.id).await,
        "master",
        "the repository row must name the same branch the page will resolve"
    );
}

/// The bound on the fix: once a default branch has commits, a push to any other
/// branch leaves it alone. Without this, the previous test's behaviour would
/// mean the last branch anyone pushed silently became the repository default.
#[tokio::test]
async fn a_push_to_a_side_branch_never_moves_a_live_default() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "liveowner").await;
    let repo = rg_core::repo::service::create_repo(
        &db, owner.id, "liverepo", None, false, &repo_root, None,
    )
    .await
    .expect("create repo");
    let bare_path = repo_root.join("liveowner/liverepo.git");

    let worktree = tempfile::tempdir().expect("create the pusher's worktree");
    let main_sha = push_one_commit(&bare_path, "main", worktree.path());
    run_post_push_hooks(
        &db,
        &repo_root,
        "liveowner",
        "liverepo",
        Some(owner.id),
        &[accepted_push("refs/heads/main", &main_sha)],
    )
    .await;

    git(&["checkout", "-q", "-b", "feature"], Some(worktree.path()));
    std::fs::write(worktree.path().join("feature.txt"), "feature\n").expect("write feature file");
    git(&["add", "."], Some(worktree.path()));
    git(&["commit", "-qm", "feature"], Some(worktree.path()));
    git(&["push", "-q", "origin", "feature"], Some(worktree.path()));
    let feature_sha = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(&["rev-parse", "HEAD"], Some(worktree.path()))
        .expect("run git rev-parse")
        .stdout_str()
        .trim()
        .to_string();

    run_post_push_hooks(
        &db,
        &repo_root,
        "liveowner",
        "liverepo",
        Some(owner.id),
        &[accepted_push("refs/heads/feature", &feature_sha)],
    )
    .await;

    assert_eq!(
        head_state(&bare_path),
        (Some("refs/heads/main".to_string()), true),
        "a repository whose default branch has commits keeps it"
    );
    assert_eq!(
        default_branch(&db, repo.id).await,
        "main",
        "a side-branch push must not rewrite the repository row"
    );
}
