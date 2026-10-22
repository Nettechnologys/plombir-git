//! card_64ae115d5bd6: an import from a real Gitea server.
//!
//! Ignored because it needs one: `scripts/gitea-import-live.sh up` starts a
//! throwaway Gitea in Docker, fills `importer/moved` through Gitea's own API and
//! prints the two variables this test reads. The fixture there is what the
//! assertions below expect: issue #1 with a comment by a second account, pull
//! request #2 merged with a merge commit and its branch deleted, an approving
//! review, labels, a milestone, a release and a wiki page.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

use rg_db::entities::{
    issue, issue_comment, label, milestone, pr_review, pull_request, release, review_comment,
};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set — run `scripts/gitea-import-live.sh up` and export its output")
    })
}

#[tokio::test]
#[ignore = "needs a live Gitea: scripts/gitea-import-live.sh up"]
async fn a_gitea_repository_imports_with_its_history() {
    let base = env("PLOMBIR_GIT_TEST_GITEA_URL");
    let token = env("PLOMBIR_GIT_TEST_GITEA_TOKEN");
    let directory = tempfile::tempdir().expect("temp dir");
    let db = crate::common::migrated_sqlite(&directory.path().join("import.db"), 2).await;
    let repo_root = directory.path().join("repos");
    let importer = rg_db::ops::user_ops::create_user(
        &db,
        "local-importer",
        "local-importer@example.invalid",
        "",
        "Local Importer",
    )
    .await
    .expect("the account that runs the import");

    let now = chrono::Utc::now();
    let task = rg_db::ops::import_task_ops::create(
        &db,
        rg_db::entities::import_task::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(importer.id),
            repo_id: Set(None),
            platform: Set("gitea".to_string()),
            source_url: Set(format!("{base}/importer/moved")),
            target_owner: Set(importer.username.clone()),
            target_name: Set("moved".to_string()),
            status: Set("running".to_string()),
            progress: Set(0),
            stage: Set(None),
            error: Set(None),
            import_repo: Set(true),
            import_issues: Set(true),
            import_pull_requests: Set(true),
            import_wiki: Set(true),
            import_releases: Set(true),
            import_labels: Set(true),
            import_milestones: Set(true),
            stats: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("the import task row");

    // The live server is on loopback over plain HTTP, which an import refuses
    // unless the operator names that origin — so the test names it.
    let trusted = rg_core::import::trust::TrustedImportOrigins::parse(std::slice::from_ref(&base))
        .expect("trust the live Gitea");
    let transport =
        rg_core::import::trust::ImportTransportPolicy::parse(std::slice::from_ref(&base))
            .expect("allow its plain HTTP");
    let stats = rg_core::import::service::run_import(
        &db,
        &task,
        &repo_root,
        Some(&token),
        &trusted,
        &transport,
        Default::default(),
    )
    .await
    .expect("the Gitea import succeeds");

    let repo = rg_core::repo::service::find_repo_by_owner_name(&db, &importer.username, "moved")
        .await
        .unwrap()
        .expect("the imported repository");
    assert!(stats.repo_cloned, "{stats:?}");
    assert_eq!(stats.wiki_pages_imported, 1, "{stats:?}");
    assert_eq!(stats.pull_request_heads_imported, 1, "{stats:?}");
    assert_eq!(stats.completion_stage(), "Import completed", "{stats:?}");

    let labels: Vec<String> = label::Entity::find()
        .filter(label::Column::RepoId.eq(repo.id))
        .all(&db)
        .await
        .unwrap()
        .into_iter()
        .map(|label| label.name)
        .collect();
    assert!(labels.contains(&"bug".to_string()), "{labels:?}");
    assert_eq!(
        milestone::Entity::find()
            .filter(milestone::Column::RepoId.eq(repo.id))
            .all(&db)
            .await
            .unwrap()
            .len(),
        1
    );

    let issues = issue::Entity::find()
        .filter(issue::Column::RepoId.eq(repo.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(
        issues.len(),
        1,
        "the pull request is not listed as an issue"
    );
    let filed = &issues[0];
    assert_eq!(filed.number, 1);
    assert_eq!(filed.author_id, importer.id);
    assert_eq!(filed.original_author.as_deref(), Some("importer"));
    // The host is spelled out: no Gitea's sign-in vouches for its ids.
    let host = base
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    assert_eq!(
        filed.original_service.as_deref(),
        Some(format!("gitea:{host}").as_str())
    );
    let comments = issue_comment::Entity::find()
        .filter(issue_comment::Column::IssueId.eq(filed.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].original_author.as_deref(), Some("reviewer"));

    let prs = pull_request::Entity::find()
        .filter(pull_request::Column::RepoId.eq(repo.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(prs.len(), 1);
    let merged = &prs[0];
    assert_eq!(
        merged.number, 2,
        "Gitea numbers issues and pull requests together"
    );
    assert_eq!(merged.state, "merged");
    assert!(merged.merge_commit_sha.is_some());
    assert!(merged.base_sha.is_some());
    let reviews = pr_review::Entity::find()
        .filter(pr_review::Column::PrId.eq(merged.id))
        .all(&db)
        .await
        .unwrap();
    assert!(
        reviews.iter().any(|review| review.action == "approve"
            && review.original_author.as_deref() == Some("reviewer")),
        "{reviews:?}"
    );
    // card_f6c67a05c6b0: the approving review's comment on the added line,
    // read from Gitea's per-review route, sits on that line of the diff.
    let inline = review_comment::Entity::find()
        .filter(review_comment::Column::PrId.eq(merged.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(inline.len(), 1, "{inline:?}");
    assert_eq!(
        (
            inline[0].path.as_str(),
            inline[0].line,
            inline[0].side.as_deref(),
            inline[0].body.as_str()
        ),
        ("feature.txt", Some(1), Some("RIGHT"), "on the added line")
    );
    assert!(inline[0].commit_id.is_some(), "{inline:?}");
    assert!(
        reviews
            .iter()
            .any(|review| review.id == inline[0].review_id && review.action == "approve"),
        "the comment sits under the review it was written in: {reviews:?}"
    );

    // The merged pull request's branch is gone; its head is kept and its
    // diff reads from the recorded commits.
    let diff = rg_core::pull_request::compute_diff(&db, &repo_root, &importer.username, "moved", 2)
        .await
        .expect("the merged pull request's diff");
    assert_eq!(
        diff.files_changed
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["feature.txt"]
    );

    let releases = release::Entity::find()
        .filter(release::Column::RepoId.eq(repo.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].tag_name, "v1.0");
    assert_eq!(releases[0].original_author.as_deref(), Some("importer"));
}
