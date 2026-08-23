//! The code-search index must not outlive the tree it was taken from
//! (card_a1237efc85b7).
//!
//! `code_fts` is a snapshot of one commit. Until the post-push refresh existed,
//! nothing moved it after the single explicit `index-repo` / index-endpoint
//! call that created it: `ai_search_code` answered `200` forever out of a
//! snapshot that drifted with every push — deleted files still findable, added
//! files missing, and no signal anywhere that the answer was stale. The refusal
//! it *could* produce ("repository not indexed") happened at most once in a
//! repository's life.
//!
//! What is pinned here is the whole shape of the decision, not just the happy
//! path: a push to the default branch refreshes an existing snapshot; a push to
//! any other branch does not; and a repository nobody has indexed does not get
//! an index built for it behind their back, because indexing reads every blob
//! of the tree and stores its text a second time.

use std::{path::Path, sync::Arc};

use rg_core::blob_storage::LocalBlobStorage;
use rg_core::package_registry::oci::storage::OciStorage;
use rg_core::search::code_indexer::CodeIndexer;

use crate::common::{accepted_push, git, run_post_push_hooks};

/// A distinctive token in the file that exists in the *first* commit only.
const STALE_MARKER: &str = "zzstalemarker";
/// A distinctive token in the file that exists in the *second* commit only.
const FRESH_MARKER: &str = "zzfreshmarker";

async fn fresh_db(dir: &Path) -> sea_orm::DatabaseConnection {
    crate::common::migrated_sqlite(&dir.join("test.db"), 2).await
}

async fn user(db: &sea_orm::DatabaseConnection, name: &str) -> rg_db::entities::user::Model {
    rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.invalid"), "", name)
        .await
        .unwrap_or_else(|error| panic!("create user {name}: {error:#}"))
}

/// A worktree wired to `bare_path`, already carrying one commit on `main` whose
/// only indexable file contains [`STALE_MARKER`].
fn seed_worktree(bare_path: &Path) -> tempfile::TempDir {
    let worktree = tempfile::tempdir().expect("create index fixture worktree");
    let path = worktree.path();
    let path_arg = path.to_str().expect("UTF-8 worktree path");
    let bare_arg = bare_path.to_str().expect("UTF-8 bare repository path");

    git(&["init", "-q", "-b", "main", path_arg], None);
    git(&["config", "user.name", "Code index test"], Some(path));
    git(
        &["config", "user.email", "code-index@example.invalid"],
        Some(path),
    );
    std::fs::write(path.join("stale.rs"), format!("fn {STALE_MARKER}() {{}}\n"))
        .expect("write the stale file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "stale"], Some(path));
    git(&["remote", "add", "origin", bare_arg], Some(path));
    git(&["push", "-q", "origin", "main"], Some(path));

    worktree
}

/// Replace the seeded file with one carrying [`FRESH_MARKER`] and push the
/// result to `branch`. Returns the new commit SHA — what `receive-pack` reports
/// to the hooks.
fn push_replacement_commit(worktree: &Path, branch: &str) -> String {
    if branch != "main" {
        git(&["checkout", "-q", "-b", branch], Some(worktree));
    }
    std::fs::remove_file(worktree.join("stale.rs")).expect("remove the stale file");
    std::fs::write(
        worktree.join("fresh.rs"),
        format!("fn {FRESH_MARKER}() {{}}\n"),
    )
    .expect("write the fresh file");
    git(&["add", "-A"], Some(worktree));
    git(&["commit", "-qm", "fresh"], Some(worktree));
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

/// Every file path the index currently holds for `repo_id` that matches
/// `marker` — the reader's view, taken through the same search the AI endpoint
/// serves.
async fn indexed_paths(
    db: &sea_orm::DatabaseConnection,
    repo_id: i64,
    marker: &str,
) -> Vec<String> {
    let (results, _total) = CodeIndexer::new(db.clone())
        .search_code(marker, Some(repo_id), 20, 0)
        .await
        .unwrap_or_else(|error| panic!("search the code index for {marker}: {error:#}"));
    results.into_iter().map(|hit| hit.file_path).collect()
}

/// The core of the card: a repository that *has* an index gets it refreshed by
/// a push to its default branch, with no second manual indexing call.
#[tokio::test]
async fn a_push_to_the_default_branch_refreshes_an_existing_code_index() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "indexowner").await;
    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "indexrepo",
        None,
        false,
        &repo_root,
        None,
    )
    .await
    .expect("create repo");
    let bare_path = repo_root.join("indexowner/indexrepo.git");
    let worktree = seed_worktree(&bare_path);

    // The one explicit act that creates a snapshot — everything after this is
    // what used to be frozen forever.
    let indexed = CodeIndexer::new(db.clone())
        .index_repository(repo.id, &bare_path, "main")
        .await
        .expect("build the initial index");
    assert_eq!(indexed, 1, "the seed commit has exactly one indexable file");
    assert_eq!(
        indexed_paths(&db, repo.id, STALE_MARKER).await,
        vec!["stale.rs".to_string()],
        "the initial snapshot must contain the seeded file"
    );

    let new_sha = push_replacement_commit(worktree.path(), "main");
    run_post_push_hooks(
        &db,
        &repo_root,
        "indexowner",
        "indexrepo",
        Some(owner.id),
        &[accepted_push("refs/heads/main", &new_sha)],
    )
    .await;

    assert_eq!(
        indexed_paths(&db, repo.id, FRESH_MARKER).await,
        vec!["fresh.rs".to_string()],
        "a file added by the push must be findable without a manual re-index"
    );
    assert!(
        indexed_paths(&db, repo.id, STALE_MARKER).await.is_empty(),
        "a file deleted by the push must stop being findable — a stale hit is \
         the silently-wrong answer this card is about"
    );
}

/// Repository deletion is also the retention boundary for the opt-in source
/// snapshot. The Git tree is removed by the same operation, so leaving its text
/// in `code_fts` would keep a second, unbounded copy that no live repository can
/// reach and no later hard-delete can cascade.
#[tokio::test]
async fn deleting_a_repository_removes_its_code_index() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "deleteindexowner").await;
    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "deleteindexrepo",
        None,
        false,
        &repo_root,
        None,
    )
    .await
    .expect("create repo");
    let bare_path = repo_root.join("deleteindexowner/deleteindexrepo.git");
    let _worktree = seed_worktree(&bare_path);
    let indexer = CodeIndexer::new(db.clone());

    assert_eq!(
        indexer
            .index_repository(repo.id, &bare_path, "main")
            .await
            .expect("build the repository code index"),
        1,
        "the fixture must put source contents into code_fts before deletion"
    );
    assert_eq!(
        indexer
            .indexed_file_count(repo.id)
            .await
            .expect("count indexed files before deletion"),
        1
    );

    let blob_storage = LocalBlobStorage::new(&repo_root);
    let oci_storage = OciStorage::from_backend(
        Arc::new(LocalBlobStorage::new(&repo_root)),
        repo_root.join("_oci_uploads"),
        Some(repo_root.clone()),
    );
    rg_core::repo::service::delete_repo(&db, &repo_root, &blob_storage, &oci_storage, &repo)
        .await
        .expect("delete the indexed repository");

    assert_eq!(
        indexer
            .indexed_file_count(repo.id)
            .await
            .expect("count indexed files after deletion"),
        0,
        "repository deletion retained its source contents in code_fts"
    );
}

/// The cost gate. Indexing reads every blob of the tree, so a push must not
/// build a snapshot for a repository whose owner never asked for one.
#[tokio::test]
async fn a_push_does_not_build_an_index_for_a_repository_that_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "unindexedowner").await;
    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "unindexedrepo",
        None,
        false,
        &repo_root,
        None,
    )
    .await
    .expect("create repo");
    let bare_path = repo_root.join("unindexedowner/unindexedrepo.git");
    let worktree = seed_worktree(&bare_path);

    let new_sha = push_replacement_commit(worktree.path(), "main");
    run_post_push_hooks(
        &db,
        &repo_root,
        "unindexedowner",
        "unindexedrepo",
        Some(owner.id),
        &[accepted_push("refs/heads/main", &new_sha)],
    )
    .await;

    assert_eq!(
        CodeIndexer::new(db.clone())
            .indexed_file_count(repo.id)
            .await
            .expect("count index rows"),
        0,
        "the first snapshot stays an explicit act — a push must not index a \
         repository nobody asked to index"
    );
}

/// The snapshot follows the default branch, not whatever moved. A topic branch
/// push must leave the index describing the default branch it describes.
#[tokio::test]
async fn a_push_to_another_branch_leaves_the_code_index_alone() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");

    let owner = user(&db, "sidebranchowner").await;
    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "sidebranchrepo",
        None,
        false,
        &repo_root,
        None,
    )
    .await
    .expect("create repo");
    let bare_path = repo_root.join("sidebranchowner/sidebranchrepo.git");
    let worktree = seed_worktree(&bare_path);

    CodeIndexer::new(db.clone())
        .index_repository(repo.id, &bare_path, "main")
        .await
        .expect("build the initial index");

    let new_sha = push_replacement_commit(worktree.path(), "topic");
    run_post_push_hooks(
        &db,
        &repo_root,
        "sidebranchowner",
        "sidebranchrepo",
        Some(owner.id),
        &[accepted_push("refs/heads/topic", &new_sha)],
    )
    .await;

    assert_eq!(
        indexed_paths(&db, repo.id, STALE_MARKER).await,
        vec!["stale.rs".to_string()],
        "the index describes the default branch; a topic-branch push must not \
         redirect it"
    );
    assert!(
        indexed_paths(&db, repo.id, FRESH_MARKER).await.is_empty(),
        "a file that exists only on a topic branch must not enter the index"
    );
}
