//! The web editor's write endpoints must run the same post-push automation a
//! `git push` runs.
//!
//! `POST /repos/:owner/:name/contents/*` clones the bare repo, commits and
//! pushes back — the branch moves exactly as it does over git. Until
//! card_13202be354ac the handler stopped at reading the new SHA for the
//! response, so an edit made in the UI triggered no pipeline, sent no webhook
//! and left every open PR on that branch pointing at the previous commit, which
//! is the head auto-merge and the merge queue then decided on. The same defect
//! had already been fixed for the SSH transport (card_b4fefeee8abf); this is
//! the third path that moves a ref.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{ActiveValue::NotSet, Set};

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

/// A CI engine that always claims a config is present and records what it was
/// asked to run. The harness default (`NoopCiEngine`) answers
/// `has_ci_config = false`, which would make "no pipeline" indistinguishable
/// from the bug.
#[derive(Default)]
struct RecordingCiEngine {
    triggered: Mutex<Vec<(String, String, String)>>,
}

impl rg_core::ci::CiTrigger for RecordingCiEngine {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        let entry = (
            params.commit_sha.to_string(),
            params.ref_name.to_string(),
            params.trigger_type.to_string(),
        );
        Box::pin(async move {
            self.triggered.lock().unwrap().push(entry);
            Ok(1)
        })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// A file committed through the web editor must trigger CI on the new commit
/// and refresh the head SHA of the open PR on that branch — the two effects the
/// card names, both of which a `git push` of the same commit already produced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_web_editor_commit_runs_the_post_push_hooks() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let ci_engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = ci_engine.clone();

    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (jwt, user_id) = register_full(&base, "webedit", "webedit@example.com").await;
    let repo_id = crate::common::create_repo(&base, &jwt, "edit-repo").await;

    // The open PR whose head branch the edit lands on. Refreshing its head SHA
    // is a pure DB write inside the hook task, so what the assertion observes
    // is that task having run — nothing else touches this row.
    let now = chrono::Utc::now();
    let stale_sha = "1111111111111111111111111111111111111111";
    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("edited in the browser".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(user_id),
            reviewer_id: Set(None),
            head_branch: Set("main".to_string()),
            base_branch: Set("release".to_string()),
            head_sha: Set(Some(stale_sha.to_string())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed open PR");

    // ── The edit, exactly as the browser sends it. ──
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/webedit/edit-repo/contents/docs/notes.md"
        ))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "content": "written from the web editor\n",
            "message": "add notes",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the web-editor write must succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    let commit_sha = body["commit_sha"].as_str().unwrap().to_string();
    assert!(
        !commit_sha.is_empty(),
        "the handler must read back the commit it just created"
    );

    // ── The drain, as `rg_http::run` performs it on shutdown. ──
    // The hooks are detached, so this is what makes the assertions below
    // deterministic instead of a sleep-and-hope.
    let tracker = rg_core::task_tracker::delivery_tracker();
    tracker.close();
    tokio::time::timeout(Duration::from_secs(120), tracker.wait())
        .await
        .expect("delivery tracker drained within timeout");
    // The tracker is process-wide; other tests in this binary spawn into it.
    tracker.reopen();

    let refreshed = rg_db::ops::pull_request_ops::find_by_id(&db, pr.id)
        .await
        .expect("reload PR")
        .expect("PR still exists");
    assert_eq!(
        refreshed.head_sha.as_deref(),
        Some(commit_sha.as_str()),
        "an edit through the web editor must refresh the head SHA of the open PR \
         on that branch — auto-merge and the merge queue decide on this value"
    );

    let triggered = ci_engine.triggered.lock().unwrap().clone();
    assert_eq!(
        triggered,
        vec![(
            commit_sha.clone(),
            "refs/heads/main".to_string(),
            "push".to_string()
        )],
        "the commit must trigger exactly one push pipeline, on the branch it landed on"
    );

    server.abort();
}

/// The delete endpoint moves the branch just as the write endpoint does, and
/// was missing the hooks for the same reason. Its ref update must report the
/// real previous SHA — not zeros, which would make the hooks read a live branch
/// as freshly created and fire `branch.created` for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_web_editor_delete_runs_the_post_push_hooks() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let ci_engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = ci_engine.clone();

    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (jwt, user_id) = register_full(&base, "webdel", "webdel@example.com").await;
    let repo_id = crate::common::create_repo(&base, &jwt, "del-repo").await;

    let client = reqwest::Client::new();
    let created = client
        .post(format!(
            "{base}/api/v1/repos/webdel/del-repo/contents/doomed.txt"
        ))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "content": "delete me\n",
            "message": "add doomed.txt",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 200, "seed write must succeed");
    let created: serde_json::Value = created.json().await.unwrap();
    let sha_before_delete = created["commit_sha"].as_str().unwrap().to_string();

    // The blob SHA is the delete endpoint's optimistic-concurrency token.
    let blob = client
        .get(format!("{base}/api/v1/repos/webdel/del-repo/blob/doomed.txt"))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(blob.status(), 200, "the seeded file must be readable back");
    let blob: serde_json::Value = blob.json().await.unwrap();
    let blob_sha = blob["sha"].as_str().expect("blob response carries a sha");

    let now = chrono::Utc::now();
    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("deleted in the browser".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(user_id),
            reviewer_id: Set(None),
            head_branch: Set("main".to_string()),
            base_branch: Set("release".to_string()),
            head_sha: Set(Some(sha_before_delete.clone())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed open PR");

    ci_engine.triggered.lock().unwrap().clear();

    let resp = client
        .delete(format!(
            "{base}/api/v1/repos/webdel/del-repo/contents/doomed.txt?message=drop+it&sha={blob_sha}"
        ))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the web-editor delete must succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    let commit_sha = body["commit_sha"].as_str().unwrap().to_string();
    assert_ne!(
        commit_sha, sha_before_delete,
        "the delete must have produced a new commit"
    );

    let tracker = rg_core::task_tracker::delivery_tracker();
    tracker.close();
    tokio::time::timeout(Duration::from_secs(120), tracker.wait())
        .await
        .expect("delivery tracker drained within timeout");
    tracker.reopen();

    let refreshed = rg_db::ops::pull_request_ops::find_by_id(&db, pr.id)
        .await
        .expect("reload PR")
        .expect("PR still exists");
    assert_eq!(
        refreshed.head_sha.as_deref(),
        Some(commit_sha.as_str()),
        "a delete through the web editor must refresh the open PR's head SHA too"
    );

    let triggered = ci_engine.triggered.lock().unwrap().clone();
    assert_eq!(
        triggered,
        vec![(
            commit_sha.clone(),
            "refs/heads/main".to_string(),
            "push".to_string()
        )],
        "the delete commit must trigger its own push pipeline"
    );

    server.abort();
}
