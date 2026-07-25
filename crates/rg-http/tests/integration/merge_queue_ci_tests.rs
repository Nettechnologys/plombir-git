//! Merge queue speculative merge-group CI regression coverage.

use std::sync::Arc;

use crate::common::{build_test_app_state, register_full, setup_test_db};

struct PendingMergeGroupCi;

impl rg_core::ci::CiTrigger for PendingMergeGroupCi {
    fn has_ci_config(&self, _repo_path: &std::path::Path, _commit_sha: &str) -> bool {
        true
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        Box::pin(async move {
            Ok(rg_db::ops::pipeline_ops::create_pipeline(
                params.db,
                params.repo_id,
                params.commit_sha,
                params.ref_name,
                params.trigger_type,
                params.triggered_by,
            )
            .await?
            .id)
        })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn queue_waits_for_speculative_merge_group_ci_before_updating_base() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = Arc::new(PendingMergeGroupCi);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "queue-ci-owner", "queue-ci@example.com").await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "speculative", "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);

    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let bare = repo_root.join("queue-ci-owner/speculative.git");
    let worktree = tempfile::tempdir().unwrap();
    let worktree_path = worktree.path();
    let worktree_arg = worktree_path.to_string_lossy();
    git.run(&["init", "--initial-branch=main", &worktree_arg], None)
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["config", "user.name", "Queue CI"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(
        &["config", "user.email", "queue-ci@example.com"],
        Some(worktree_path),
    )
    .unwrap()
    .ensure_success()
    .unwrap();
    std::fs::write(worktree_path.join("value.txt"), "base\n").unwrap();
    git.run(&["add", "."], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["commit", "-m", "base"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    let bare_arg = bare.to_string_lossy();
    git.run(&["remote", "add", "origin", &bare_arg], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "main"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["checkout", "-b", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    std::fs::write(worktree_path.join("value.txt"), "feature\n").unwrap();
    git.run(&["commit", "-am", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    let base_before = git
        .run(&["rev-parse", "refs/heads/main"], Some(&bare))
        .unwrap()
        .stdout_str()
        .trim()
        .to_string();

    let client = reqwest::Client::new();
    let pr = client
        .post(format!(
            "{base}/api/v1/repos/queue-ci-owner/speculative/pulls"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "Speculative merge",
            "head": "feature",
            "base": "main"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pr.status(), 201, "{}", pr.text().await.unwrap());

    let queue_url = format!("{base}/api/v1/repos/queue-ci-owner/speculative/pulls/1/merge-queue");
    let queued = client
        .put(&queue_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), 200, "{}", queued.text().await.unwrap());
    let queued = rg_db::ops::merge_queue_ops::list_by_repo(
        &db,
        rg_db::ops::repo_ops::find_by_owner_and_name(
            &db,
            rg_db::ops::user_ops::find_by_username(&db, "queue-ci-owner")
                .await
                .unwrap()
                .unwrap()
                .id,
            "speculative",
        )
        .await
        .unwrap()
        .unwrap()
        .id,
    )
    .await
    .unwrap()
    .remove(0);
    assert!(queued.merge_group_sha.is_some());
    let pipeline_id = queued.merge_group_pipeline_id.unwrap();
    assert_eq!(
        git.run(&["rev-parse", "refs/heads/main"], Some(&bare))
            .unwrap()
            .stdout_str()
            .trim(),
        base_before
    );

    rg_db::ops::pipeline_ops::update_pipeline_status(
        &db,
        pipeline_id,
        "success",
        None,
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .unwrap();
    let processed = client
        .put(&queue_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        processed.status(),
        200,
        "{}",
        processed.text().await.unwrap()
    );
    let pr = client
        .get(format!(
            "{base}/api/v1/repos/queue-ci-owner/speculative/pulls/1"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(pr["state"], "merged");
    assert_ne!(
        git.run(&["rev-parse", "refs/heads/main"], Some(&bare))
            .unwrap()
            .stdout_str()
            .trim(),
        base_before
    );
}

/// card_a997f30c142c: the reason a merge-queue entry failed is persisted into
/// `failure_reason` and rendered in the UI — it is the whole answer to "why
/// didn't my PR merge". `error.to_string()` printed only the outermost
/// `.context(...)` ("failed to resolve merge ref 'feature'") and dropped the
/// gix error under it, so the operator saw a restatement of the question.
#[tokio::test]
async fn a_failed_merge_persists_the_inner_cause_not_just_the_outer_context() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = Arc::new(PendingMergeGroupCi);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "queue-fail-owner", "queue-fail@example.com").await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": "flatten", "is_private": true}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );

    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let bare = repo_root.join("queue-fail-owner/flatten.git");
    let worktree = tempfile::tempdir().unwrap();
    let worktree_path = worktree.path();
    let worktree_arg = worktree_path.to_string_lossy();
    for args in [
        vec!["init", "--initial-branch=main", &worktree_arg],
        vec!["-C", &worktree_arg, "config", "user.name", "Queue Fail"],
        vec![
            "-C",
            &worktree_arg,
            "config",
            "user.email",
            "queue-fail@example.com",
        ],
    ] {
        git.run(&args, None).unwrap().ensure_success().unwrap();
    }
    std::fs::write(worktree_path.join("value.txt"), "base\n").unwrap();
    let bare_arg = bare.to_string_lossy();
    for args in [
        vec!["add", "."],
        vec!["commit", "-m", "base"],
        vec!["remote", "add", "origin", &bare_arg],
        vec!["push", "origin", "main"],
        vec!["checkout", "-b", "feature"],
    ] {
        git.run(&args, Some(worktree_path))
            .unwrap()
            .ensure_success()
            .unwrap();
    }
    std::fs::write(worktree_path.join("value.txt"), "feature\n").unwrap();
    for args in [
        vec!["commit", "-am", "feature"],
        vec!["push", "origin", "feature"],
    ] {
        git.run(&args, Some(worktree_path))
            .unwrap()
            .ensure_success()
            .unwrap();
    }

    assert_eq!(
        client
            .post(format!("{base}/api/v1/repos/queue-fail-owner/flatten/pulls"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "title": "Doomed merge",
                "head": "feature",
                "base": "main"
            }))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );

    let queue_url = format!("{base}/api/v1/repos/queue-fail-owner/flatten/pulls/1/merge-queue");
    assert_eq!(
        client
            .put(&queue_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": "merge"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    let repo_id = rg_db::ops::repo_ops::find_by_owner_and_name(
        &db,
        rg_db::ops::user_ops::find_by_username(&db, "queue-fail-owner")
            .await
            .unwrap()
            .unwrap()
            .id,
        "flatten",
    )
    .await
    .unwrap()
    .unwrap()
    .id;
    let queued = rg_db::ops::merge_queue_ops::list_by_repo(&db, repo_id)
        .await
        .unwrap()
        .remove(0);
    rg_db::ops::pipeline_ops::update_pipeline_status(
        &db,
        queued.merge_group_pipeline_id.unwrap(),
        "success",
        None,
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .unwrap();

    // Drop the branch the merge will need. The merge-group CI step only reads
    // the base ref and the stored head SHA, so it still reports Ready and the
    // failure lands where we want it: inside `gix_merge_no_ff`, which wraps the
    // gix error in a `.context(...)` — exactly the two-layer chain at issue.
    git.run(&["update-ref", "-d", "refs/heads/feature"], Some(&bare))
        .unwrap()
        .ensure_success()
        .unwrap();

    assert_eq!(
        client
            .put(&queue_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": "merge"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // `list_by_repo` only returns queued/running entries, so a finished one has
    // to be looked up by PR.
    let entry = rg_db::ops::merge_queue_ops::find_by_pr(&db, queued.pr_id)
        .await
        .unwrap()
        .expect("the queue entry must still be there");
    assert_eq!(entry.status, "failed", "the merge was expected to fail");
    let reason = entry
        .failure_reason
        .expect("a failed entry must carry a reason");
    assert!(
        reason.contains("failed to resolve merge ref"),
        "the outer context must survive: {reason}"
    );
    assert!(
        reason.len() > "failed to resolve merge ref 'feature'".len(),
        "the reason must carry the cause underneath the context, not just the \
         context itself: {reason}"
    );
}
