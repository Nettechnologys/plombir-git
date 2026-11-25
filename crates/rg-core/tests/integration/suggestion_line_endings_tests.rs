//! card_98cad1f4246c: applying a review suggestion changes the lines it names
//! and nothing else.
//!
//! The target file used to be split with `str::lines()` and rejoined with
//! `"\n"`, so one suggestion on a CRLF file committed every line of it as LF,
//! and a suggestion that deleted a file's only line left `"\n"` behind. Both
//! are asserted on the blob the commit actually wrote, through a real bare
//! repository and the public `apply_suggestion` entry point.

use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use crate::common::git;

fn git_output(args: &[&str], cwd: &std::path::Path) -> String {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(args, Some(cwd))
        .expect("run git");
    output.ensure_success().expect("git command succeeds");
    output.stdout_str().to_string()
}

/// Commit `original` as `notes.txt`, leave a one-line suggestion on line 1
/// replacing it with `replacement`, apply it, and return the file as the new
/// commit has it.
async fn apply_replacement(original: &str, replacement: &str) -> String {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = crate::common::migrated_sqlite(&directory.path().join("test.db"), 2).await;
    let repo_root = directory.path().join("repos");
    let user = rg_db::ops::user_ops::create_user(
        &db,
        "reviewer",
        "reviewer@example.invalid",
        "",
        "Reviewer",
    )
    .await
    .expect("create the reviewer");
    let repo =
        rg_core::repo::service::create_repo(&db, user.id, "sample", None, false, &repo_root, None)
            .await
            .expect("create the repository");

    let worktree = tempfile::tempdir().expect("create the author's worktree");
    let path = worktree.path();
    let bare = repo_root.join("reviewer/sample.git");
    git(&["init", "-q", "-b", "main", path.to_str().unwrap()], None);
    git(&["config", "user.name", "Reviewer"], Some(path));
    git(
        &["config", "user.email", "reviewer@example.invalid"],
        Some(path),
    );
    // The bytes on disk are the bytes in the blob: no conversion either way.
    git(&["config", "core.autocrlf", "false"], Some(path));
    std::fs::write(path.join("notes.txt"), original).unwrap();
    git(&["add", "notes.txt"], Some(path));
    git(&["commit", "-qm", "seed"], Some(path));
    git(&["push", "-q", bare.to_str().unwrap(), "main"], Some(path));
    let head = git_output(&["rev-parse", "HEAD"], path).trim().to_owned();
    assert_eq!(
        git_output(&["show", &format!("{head}:notes.txt")], &bare),
        original,
        "the fixture must commit the file byte for byte"
    );

    let now = chrono::Utc::now();
    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("review".into()),
            state: Set("open".into()),
            author_id: Set(user.id),
            head_branch: Set("main".into()),
            base_branch: Set("release".into()),
            head_sha: Set(Some(head.clone())),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("create the pull request");
    let review = rg_db::entities::pr_review::ActiveModel {
        pr_id: Set(pr.id),
        repo_id: Set(repo.id),
        reviewer_id: Set(user.id),
        action: Set("comment".into()),
        commit_id: Set(Some(head.clone())),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create the review");
    let comment = rg_db::entities::review_comment::ActiveModel {
        review_id: Set(review.id),
        pr_id: Set(pr.id),
        author_id: Set(user.id),
        body: Set("suggestion".into()),
        path: Set("notes.txt".into()),
        line: Set(Some(1)),
        side: Set(Some("RIGHT".into())),
        commit_id: Set(Some(head)),
        suggestion: Set(Some(replacement.into())),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create the suggestion");

    let applied = rg_core::review::service::apply_suggestion(
        &db, &repo_root, &repo, "reviewer", &pr, comment.id, &user,
    )
    .await
    .expect("apply the suggestion");
    let stored = rg_db::entities::review_comment::Entity::find_by_id(comment.id)
        .one(&db)
        .await
        .expect("read the suggestion back")
        .expect("the suggestion row still exists");
    assert!(stored.suggestion_applied_at.is_some());

    git_output(
        &["show", &format!("{}:notes.txt", applied.commit_sha)],
        &bare,
    )
}

#[tokio::test]
async fn a_suggestion_on_a_crlf_file_leaves_the_other_lines_crlf() {
    assert_eq!(
        apply_replacement("old\r\nuntouched\r\n", "new").await,
        "new\r\nuntouched\r\n"
    );
}

#[tokio::test]
async fn deleting_a_files_only_line_leaves_it_empty() {
    assert_eq!(apply_replacement("old\n", "").await, "");
}

/// The control: an LF file was always handled right and still is.
#[tokio::test]
async fn a_suggestion_on_an_lf_file_changes_only_its_line() {
    assert_eq!(
        apply_replacement("old\nuntouched\n", "new").await,
        "new\nuntouched\n"
    );
}
