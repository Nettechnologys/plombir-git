//! Applying a review suggestion must run the same post-push automation a
//! `git push` runs.
//!
//! `POST /repos/:owner/:name/pulls/:number/comments/:id/suggestion/apply` writes
//! a real commit onto the PR's head branch — the branch moves exactly as it does
//! over git. Until card_e324a9281789 the handler ran a hand-written partial copy
//! of the hook set: a CI trigger under the invented event name `suggestion`, and
//! the auto-merge / merge-queue evaluation. No `push` webhook, no real-time
//! event, no watch fan-out, and all of it awaited inside the request instead of
//! detached through the delivery tracker. It is the fifth path that moves a ref
//! (card_13202be354ac fixed the web editor, card_b4fefeee8abf the two git
//! transports).
//!
//! The invented event name was not merely cosmetic: `Workflow::matches_event`
//! knows `push`, `pull_request` and `merge_group`, so a repository whose CI
//! lives in `.gitea/workflows/` matched no workflow at all for `suggestion` and
//! got a warning in the log where it expected a pipeline.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{ActiveValue::NotSet, ConnectionTrait, Set};

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

/// What the hook run asked CI to do. `triggered_by` is in here on purpose: the
/// suggestion path used to name the actor itself, and routing it through the
/// shared hooks must not turn the pipeline anonymous.
type TriggeredPipeline = (String, String, String, Option<i64>);

/// A CI engine that always claims a config is present and records what it was
/// asked to run. The harness default (`NoopCiEngine`) answers
/// `has_ci_config = false`, which would make "no pipeline" indistinguishable
/// from the bug.
#[derive(Default)]
struct RecordingCiEngine {
    triggered: Mutex<Vec<TriggeredPipeline>>,
}

impl rg_core::ci::CiTrigger for RecordingCiEngine {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    /// Mirrors `has_ci_config`: this double has no workflow files to
    /// match an event against, so it answers the same for every event.
    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
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
            params.triggered_by,
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

/// Drain the detached hook tasks the way `rg_http::run` drains them on shutdown.
/// This is what makes the assertions deterministic instead of sleep-and-hope.
async fn drain_delivery_tracker(tracker: &rg_core::task_tracker::TaskTracker, what: &str) {
    tracker.close();
    tokio::time::timeout(Duration::from_secs(120), tracker.wait())
        .await
        .unwrap_or_else(|_| panic!("{what} drained within timeout"));
    tracker.reopen();
}

/// What one drive of the apply path produced, once its detached hooks have
/// been drained.
struct Applied {
    db: sea_orm::DatabaseConnection,
    status: u16,
    /// The handler's response body — `{ comment, commit_sha }` on success.
    body: serde_json::Value,
    /// The head the pull request pointed at before the suggestion was applied.
    head_sha: String,
    pr_id: i64,
    comment_id: i64,
    actor_id: i64,
    watcher_id: i64,
    triggered: Vec<TriggeredPipeline>,
    _server: tokio::task::JoinHandle<()>,
}

/// Drive the whole path: seed a repository, a file, an open pull request and a
/// suggestion comment on it, then apply the suggestion and drain the hooks.
///
/// `fault` is SQL executed against the test database immediately before the
/// apply request and at no other moment, so everything the fixture needs is
/// already written when the outage starts. That is what makes it possible to
/// break exactly one write of the apply path without breaking the path to it.
async fn apply_a_suggestion(fault: Option<&str>) -> Applied {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let ci_engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = ci_engine.clone();
    let delivery_tracker = state.delivery_tracker.clone();

    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (jwt, user_id) = register_full(&base, "suggester", "suggester@example.com").await;
    let repo_id = crate::common::create_repo(&base, &jwt, "sugg-repo").await;

    // A second account watching the repository. The watch fan-out skips the
    // actor themselves, so the recipient has to be somebody else; the repo is
    // public, which is what gets this user past the per-recipient read check.
    let (watcher_jwt, watcher_id) =
        register_full(&base, "sugg-watcher", "watcher@example.com").await;
    let client = reqwest::Client::new();
    let watch = client
        .put(format!("{base}/api/v1/repos/suggester/sugg-repo/watch"))
        .bearer_auth(&watcher_jwt)
        .json(&serde_json::json!({"state": "watching"}))
        .send()
        .await
        .unwrap();
    assert_eq!(watch.status(), 200, "the watcher must be subscribed");

    // ── Seed the file the suggestion rewrites. ──
    let seeded = client
        .post(format!(
            "{base}/api/v1/repos/suggester/sugg-repo/contents/notes.md"
        ))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "content": "the original line\n",
            "message": "add notes",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(seeded.status(), 200, "seed write must succeed");
    let seeded: serde_json::Value = seeded.json().await.unwrap();
    let head_sha = seeded["commit_sha"].as_str().unwrap().to_string();

    // The open PR the suggestion belongs to, pointing at the seeded commit.
    let now = chrono::Utc::now();
    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("take the reviewer's wording".to_string()),
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
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed open PR");

    let comment = client
        .post(format!(
            "{base}/api/v1/repos/suggester/sugg-repo/pulls/1/comments"
        ))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "path": "notes.md",
            "line": 1,
            "side": "RIGHT",
            "body": "reads better like this",
            "suggestion": "the reviewed line",
            "commit_id": head_sha,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        comment.status(),
        201,
        "the suggestion comment must be created"
    );
    let comment: serde_json::Value = comment.json().await.unwrap();
    let comment_id = comment["id"].as_i64().expect("comment carries an id");

    // Drain and clear before the action under test: the seed write's own hooks
    // are detached, so under a loaded parallel run they can still be queued here
    // and would otherwise land in the recorder as a second entry.
    drain_delivery_tracker(&delivery_tracker, "the seed write's hooks").await;
    ci_engine.triggered.lock().unwrap().clear();
    rg_db::ops::notification_ops::mark_all_read(&db, watcher_id)
        .await
        .expect("clear the seed write's watch notification");

    if let Some(fault) = fault {
        db.execute_unprepared(fault)
            .await
            .expect("install the write fault");
    }

    // ── The action under test. ──
    let applied = client
        .post(format!(
            "{base}/api/v1/repos/suggester/sugg-repo/pulls/1/comments/{comment_id}/suggestion/apply"
        ))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    let status = applied.status().as_u16();
    let body: serde_json::Value = applied.json().await.unwrap();

    drain_delivery_tracker(&delivery_tracker, "the suggestion's post-push hooks").await;
    let triggered = ci_engine.triggered.lock().unwrap().clone();

    Applied {
        db,
        status,
        body,
        head_sha,
        pr_id: pr.id,
        comment_id,
        actor_id: user_id,
        watcher_id,
        triggered,
        _server: server,
    }
}

/// The commit is on the branch, so it is owed the same automation a push gets:
/// a pipeline per event it raises, on the branch it landed on, under event
/// names a workflow can actually match, attributed to whoever applied it — and
/// a watch notification, which nothing on this path ever sent before
/// card_e324a9281789.
async fn assert_post_push_hooks_ran(applied: &Applied, commit_sha: &str) {
    let (notifications, _total) = rg_db::ops::notification_ops::list_notifications_paginated(
        &applied.db,
        applied.watcher_id,
        true,
        0,
        100,
    )
    .await
    .expect("list the watcher's unread notifications");
    assert!(
        notifications
            .iter()
            .any(|notification| notification.event_type == "push"),
        "a watcher must be told about the suggestion commit like any other push; \
         got {notifications:?}"
    );

    assert_eq!(
        applied.triggered,
        vec![
            // The branch is the PR's head, so applying the suggestion
            // synchronises it — the `pull_request` event, on its own ref
            // (card_074d93bfe327). This path is the one that made the naive
            // "did the head-SHA row change" test for a sync wrong: it advances
            // the PR itself, before the hooks ever see the move.
            (
                commit_sha.to_string(),
                "refs/pull/1/head".to_string(),
                "pull_request".to_string(),
                Some(applied.actor_id),
            ),
            (
                commit_sha.to_string(),
                "refs/heads/main".to_string(),
                "push".to_string(),
                Some(applied.actor_id),
            ),
        ],
        "the suggestion commit must trigger exactly one pipeline per event it \
         raises, on the branch it landed on, under event names a workflow can \
         actually match — and attributed to whoever applied it"
    );
}

/// The commit the response claims to have made must really be on the branch,
/// and must not be the head the suggestion was prepared against.
fn assert_commit_landed(applied: &Applied) -> String {
    assert_eq!(
        applied.status, 200,
        "applying the suggestion must succeed; body: {}",
        applied.body
    );
    let commit_sha = applied.body["commit_sha"]
        .as_str()
        .unwrap_or_else(|| panic!("the response carries the commit; body: {}", applied.body))
        .to_string();
    assert_ne!(
        commit_sha, applied.head_sha,
        "applying a suggestion must have produced a new commit"
    );
    commit_sha
}

async fn pr_head_sha(applied: &Applied) -> Option<String> {
    rg_db::ops::pull_request_ops::find_by_id(&applied.db, applied.pr_id)
        .await
        .expect("reload PR")
        .expect("PR still exists")
        .head_sha
}

/// Applying a suggestion must trigger a **push** pipeline on the branch the
/// commit landed on, attribute it to whoever applied it, refresh the open PR's
/// head SHA, and reach the watchers — the last of which nothing on this path
/// ever did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn applying_a_suggestion_runs_the_post_push_hooks() {
    let applied = apply_a_suggestion(None).await;
    let commit_sha = assert_commit_landed(&applied);

    assert_eq!(
        pr_head_sha(&applied).await.as_deref(),
        Some(commit_sha.as_str()),
        "the open PR must point at the suggestion commit — this is the value \
         auto-merge and the merge queue select candidates by"
    );

    assert_post_push_hooks_ran(&applied, &commit_sha).await;
}

/// card_69407e37b576: the timeline write is the last thing between the commit
/// and the `Ok`. `update_files_in_commit` has already pushed the commit into
/// `refs/heads/<head>` by then, so an `Err` here cannot un-push it — it can only
/// cost the handler its `after_suggestions_applied` call, and with it the CI
/// pipeline, the `push` webhook, the watch fan-out and the auto-merge / merge
/// queue re-evaluation the new head can unblock, while answering the author 5xx
/// for a commit that is on their branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broken_timeline_write_still_runs_the_post_push_hooks() {
    // Fails only the timeline row this path writes. Dropping `pr_events`
    // outright would also break writes the hook run itself makes, and the test
    // could then pass or fail for a reason that is not the one under test.
    let applied = apply_a_suggestion(Some(
        "CREATE TRIGGER pr_events_suggestion_outage BEFORE INSERT ON pr_events \
         WHEN new.event_type = 'suggestion_applied' \
         BEGIN SELECT RAISE(ABORT, 'storage is unavailable'); END;",
    ))
    .await;
    let commit_sha = assert_commit_landed(&applied);

    assert_eq!(
        pr_head_sha(&applied).await.as_deref(),
        Some(commit_sha.as_str()),
        "only the timeline write failed, so the head SHA still advanced"
    );
    assert_post_push_hooks_ran(&applied, &commit_sha).await;
}

/// The other write past the point of no return: the markers that record which
/// comment was applied, by whom, into which commit. Losing them costs the
/// review thread its bookkeeping — it must not cost the branch its hooks, and
/// the author must not be told the suggestion was not applied when the commit
/// is sitting on their branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_marker_write_still_runs_the_post_push_hooks() {
    let applied = apply_a_suggestion(Some(
        "CREATE TRIGGER review_comments_apply_outage BEFORE UPDATE ON review_comments \
         WHEN new.suggestion_applied_at IS NOT NULL \
         BEGIN SELECT RAISE(ABORT, 'storage is unavailable'); END;",
    ))
    .await;
    let commit_sha = assert_commit_landed(&applied);

    assert_eq!(
        applied.body["comment"]["suggestion_commit_sha"].as_str(),
        Some(commit_sha.as_str()),
        "the response describes the suggestion as applied, because it is: the \
         commit is on the branch whatever the marker row says"
    );

    let comment = rg_db::ops::review_comment_ops::find_by_id(&applied.db, applied.comment_id)
        .await
        .expect("reload the review comment")
        .expect("the comment row is still there");
    // The deliberate, logged cost of surviving the outage: the marker is unset,
    // so the thread does not show the suggestion as applied. Re-applying it is
    // refused all the same — the comment's `commit_id` no longer matches the
    // pull request's head, which answers 409 "outdated".
    assert!(
        comment.suggestion_applied_at.is_none(),
        "the marker write is the one that failed; pretending otherwise would \
         hide the bookkeeping loss the log reports"
    );
    assert_post_push_hooks_ran(&applied, &commit_sha).await;
}
