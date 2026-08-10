//! card_fd0ebc0adfcd: deleting an account is not a row removal.
//!
//! `repositories.owner_id` is declared `REFERENCES users(id) ON DELETE CASCADE`
//! and foreign keys are enforced on every backend, so the single
//! `DELETE FROM users` this endpoint used to be destroyed the repository rows
//! outright while every byte they named stayed live — Git trees, the
//! `packages` / `lfs` / `releases` / `attachments` prefixes, the CI cache and
//! artifact directories, the OCI registry. Nothing was left to reach those
//! bytes and nothing was left to collect them.
//!
//! What these tests guard:
//!
//! * **The bytes go first.** A successful admin `DELETE` leaves neither live
//!   objects nor private tombstones for the account's repositories.
//! * **Ownership the account cannot take with it is refused.** An account that
//!   still owns an organization is a `409`, not a cascade that empties the
//!   organization of repositories and leaves it pointing at a user id nothing
//!   resolves.
//! * **A storage failure is not a deletion.** The account survives with its
//!   repository intact, so the request can be retried.
//! * **Other people's repositories are not touched.** What the account uploaded
//!   into somebody else's repository stays there, readable, with a ghost
//!   uploader — the cascade that used to destroy those rows while their bytes
//!   stayed live is gone (card_1cfc81035e92).

use rg_core::blob_storage::BlobKey;
use rg_db::sea_orm::{ColumnTrait, EntityTrait, NotSet, QueryFilter, Set};

use crate::common::{
    create_issue, create_repo, fault::spawn_test_app_for_fault_sweep, register_full,
    spawn_test_app_with_state,
};

async fn promote_user_to_admin(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote user to admin")
        .expect("registered user must exist");
}

async fn seed_waiting_environment_job(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    environment: &rg_db::entities::ci_environment::Model,
    marker: char,
) -> (i64, i64) {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        &marker.to_string().repeat(40),
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create approval pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "deploy", 0)
        .await
        .expect("create approval stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "production",
        "echo deploy",
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .expect("create approval job");
    rg_db::ops::ci_environment_ops::attach_job(db, job.id, Some(environment), "production")
        .await
        .expect("attach protected environment");
    assert!(
        rg_db::ops::pipeline_ops::try_pause_stage_at_manual(db, stage.id)
            .await
            .expect("pause pipeline for environment approval")
    );
    (pipeline.id, job.id)
}

/// A review is durable history, but its approval is a live authorization
/// verdict. Deactivation, the retirement marker, and final account deletion
/// must all revoke that verdict without erasing the review from the timeline.
#[tokio::test]
async fn ghost_review_stays_in_history_but_stops_authorizing_merge() {
    let (base, db, _state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "review-admin", "review-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (host_token, host_id) =
        register_full(&base, "review-host", "review-host@example.com").await;
    let (_reviewer_token, reviewer_id) = register_full(
        &base,
        "departing-reviewer",
        "departing-reviewer@example.com",
    )
    .await;
    let repo_id = create_repo(&base, &host_token, "review-lifecycle").await;
    let now = chrono::Utc::now();
    let head_sha = "1".repeat(40);

    let pull = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("Approval must follow its reviewer".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(host_id),
            reviewer_id: Set(Some(reviewer_id)),
            head_branch: Set("review-lifecycle".to_string()),
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
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed pull request");
    let review = rg_db::ops::pr_review_ops::create(
        &db,
        rg_db::entities::pr_review::ActiveModel {
            id: NotSet,
            pr_id: Set(pull.id),
            repo_id: Set(repo_id),
            reviewer_id: Set(reviewer_id),
            action: Set("approve".to_string()),
            body: Set(Some("Approved while the reviewer was active".to_string())),
            commit_id: Set(Some(head_sha)),
            created_at: Set(now),
        },
    )
    .await
    .expect("seed approval");

    let protection = client
        .post(format!(
            "{base}/api/v1/repos/review-host/review-lifecycle/branches/protection"
        ))
        .bearer_auth(&host_token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_approval": true,
            "required_approvals": 1
        }))
        .send()
        .await
        .expect("create branch protection");
    assert_eq!(
        protection.status(),
        201,
        "branch protection setup failed: {}",
        protection.text().await.unwrap_or_default()
    );

    assert!(
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pull.id,)
            .await
            .is_ok(),
        "an active reviewer's current approval must satisfy the gate"
    );

    let deactivated = client
        .patch(format!("{base}/api/v1/admin/users/{reviewer_id}"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"is_active": false}))
        .send()
        .await
        .expect("deactivate reviewer");
    assert_eq!(deactivated.status(), 200);
    assert!(
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pull.id,)
            .await
            .is_err(),
        "a deactivated reviewer still authorizes a merge"
    );

    let reactivated = client
        .patch(format!("{base}/api/v1/admin/users/{reviewer_id}"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"is_active": true}))
        .send()
        .await
        .expect("reactivate reviewer");
    assert_eq!(reactivated.status(), 200);
    assert!(
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pull.id,)
            .await
            .is_ok(),
        "reactivating the reviewer did not restore the live approval"
    );

    assert!(
        rg_db::ops::user_ops::begin_user_retirement(&db, reviewer_id)
            .await
            .expect("mark reviewer for retirement")
    );
    assert!(
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pull.id,)
            .await
            .is_err(),
        "a reviewer already claimed for deletion still authorizes a merge"
    );
    rg_db::ops::user_ops::abort_user_retirement(&db, reviewer_id)
        .await
        .expect("release reviewer retirement marker");
    assert!(
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pull.id,)
            .await
            .is_ok(),
        "releasing the retirement marker did not restore the live approval"
    );

    let deleted = client
        .delete(format!("{base}/api/v1/admin/users/{reviewer_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete reviewer");
    assert_eq!(
        deleted.status(),
        200,
        "reviewer deletion failed: {}",
        deleted.text().await.unwrap_or_default()
    );
    let merge_error =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pull.id)
            .await
            .expect_err("a deleted reviewer still authorizes a merge");
    assert!(
        format!("{merge_error:#}").contains("requires at least 1 approval(s), got 0"),
        "unexpected merge-gate error: {merge_error:#}"
    );
    assert_eq!(
        rg_db::ops::pr_review_ops::find_by_id(&db, review.id)
            .await
            .expect("read review after reviewer deletion")
            .expect("review disappeared with its reviewer")
            .reviewer_id,
        reviewer_id
    );

    let timeline = client
        .get(format!(
            "{base}/api/v1/repos/review-host/review-lifecycle/pulls/1/timeline"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read timeline after reviewer deletion");
    assert_eq!(timeline.status(), 200);
    let timeline: Vec<serde_json::Value> = timeline.json().await.expect("timeline body");
    assert!(
        timeline
            .iter()
            .any(|event| event["kind"] == "review_approve" && event["actor"].is_null()),
        "the approval did not remain as ghost-authored history: {timeline:?}"
    );
}

/// Environment approvals are durable deployment history, but only approvals
/// from currently usable accounts may release a waiting job.
#[tokio::test]
async fn ghost_environment_approval_stays_in_history_but_stops_authorizing_release() {
    let (base, db, _state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(&base, "env-admin", "env-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (owner_token, _owner_id) = register_full(&base, "env-host", "env-host@example.com").await;
    let (departing_token, departing_id) = register_full(
        &base,
        "departing-approver",
        "departing-approver@example.com",
    )
    .await;
    let (remaining_token, remaining_id) = register_full(
        &base,
        "remaining-approver",
        "remaining-approver@example.com",
    )
    .await;
    let repo_id = create_repo(&base, &owner_token, "approval-lifecycle").await;

    let environment_response = client
        .post(format!(
            "{base}/api/v1/repos/env-host/approval-lifecycle/actions/environments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 2,
            "allowed_approver_ids": [departing_id, remaining_id]
        }))
        .send()
        .await
        .expect("create protected environment");
    assert_eq!(environment_response.status(), 201);
    let environment_id = environment_response
        .json::<serde_json::Value>()
        .await
        .expect("environment response body")["id"]
        .as_i64()
        .expect("environment id");
    let environment = rg_db::ops::ci_environment_ops::find_by_id(&db, environment_id)
        .await
        .expect("read environment")
        .expect("environment disappeared");

    let (baseline_pipeline_id, baseline_job_id) =
        seed_waiting_environment_job(&db, repo_id, &environment, 'a').await;
    let baseline_url = format!(
        "{base}/api/v1/repos/env-host/approval-lifecycle/pipelines/{baseline_pipeline_id}/jobs/{baseline_job_id}/approve"
    );
    let first_live = client
        .post(&baseline_url)
        .bearer_auth(&departing_token)
        .send()
        .await
        .expect("first live approval");
    assert_eq!(first_live.status(), 200);
    let first_live: serde_json::Value = first_live.json().await.expect("first approval body");
    assert_eq!(first_live["approvals"], 1);
    assert_eq!(first_live["released"], false);
    let second_live = client
        .post(&baseline_url)
        .bearer_auth(&remaining_token)
        .send()
        .await
        .expect("second live approval");
    assert_eq!(second_live.status(), 200);
    let second_live: serde_json::Value = second_live.json().await.expect("second approval body");
    assert_eq!(second_live["approvals"], 2);
    assert_eq!(second_live["released"], true);

    let (pipeline_id, job_id) = seed_waiting_environment_job(&db, repo_id, &environment, 'b').await;
    let approve_url = format!(
        "{base}/api/v1/repos/env-host/approval-lifecycle/pipelines/{pipeline_id}/jobs/{job_id}/approve"
    );
    let initial = client
        .post(&approve_url)
        .bearer_auth(&departing_token)
        .send()
        .await
        .expect("approval before account lifecycle changes");
    assert_eq!(initial.status(), 200);
    let initial: serde_json::Value = initial.json().await.expect("initial approval body");
    assert_eq!(initial["approvals"], 1);
    assert_eq!(initial["released"], false);

    let deactivated = client
        .patch(format!("{base}/api/v1/admin/users/{departing_id}"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"is_active": false}))
        .send()
        .await
        .expect("deactivate approver");
    assert_eq!(deactivated.status(), 200);
    assert_eq!(
        rg_db::ops::ci_environment_ops::count_approvals(&db, job_id)
            .await
            .expect("count after deactivation"),
        0,
        "a deactivated approver still contributes a current approval"
    );

    let reactivated = client
        .patch(format!("{base}/api/v1/admin/users/{departing_id}"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"is_active": true}))
        .send()
        .await
        .expect("reactivate approver");
    assert_eq!(reactivated.status(), 200);
    assert_eq!(
        rg_db::ops::ci_environment_ops::count_approvals(&db, job_id)
            .await
            .expect("count after reactivation"),
        1,
        "reactivating the approver did not restore the approval"
    );

    assert!(
        rg_db::ops::user_ops::begin_user_retirement(&db, departing_id)
            .await
            .expect("mark approver for retirement")
    );
    assert_eq!(
        rg_db::ops::ci_environment_ops::count_approvals(&db, job_id)
            .await
            .expect("count during retirement"),
        0,
        "an approver claimed for deletion still contributes a current approval"
    );
    rg_db::ops::user_ops::abort_user_retirement(&db, departing_id)
        .await
        .expect("release approver retirement marker");
    assert_eq!(
        rg_db::ops::ci_environment_ops::count_approvals(&db, job_id)
            .await
            .expect("count after retirement abort"),
        1,
        "releasing the retirement marker did not restore the approval"
    );

    let deleted = client
        .delete(format!("{base}/api/v1/admin/users/{departing_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete approver");
    assert_eq!(
        deleted.status(),
        200,
        "approver deletion failed: {}",
        deleted.text().await.unwrap_or_default()
    );
    assert_eq!(
        rg_db::ops::ci_environment_ops::count_approvals(&db, job_id)
            .await
            .expect("count after deletion"),
        0,
        "a deleted approver still contributes a current approval"
    );
    let history = rg_db::entities::ci_environment_approval::Entity::find()
        .filter(rg_db::entities::ci_environment_approval::Column::JobId.eq(job_id))
        .all(&db)
        .await
        .expect("read approval history after deletion");
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].approved_by, None,
        "the deleted approver's decision did not remain as ghost history"
    );

    let remaining = client
        .post(&approve_url)
        .bearer_auth(&remaining_token)
        .send()
        .await
        .expect("remaining live approval");
    assert_eq!(remaining.status(), 200);
    let remaining: serde_json::Value = remaining.json().await.expect("remaining approval body");
    assert_eq!(remaining["approvals"], 1);
    assert_eq!(remaining["released"], false);
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .expect("read waiting job")
            .expect("job disappeared")
            .status,
        "waiting_approval"
    );
}

fn representative_keys(owner: &str, repo: &str, repo_id: i64) -> Vec<BlobKey> {
    let repo_id = repo_id.to_string();
    vec![
        BlobKey::from_segments([
            "packages",
            owner,
            repo,
            "generic",
            "demo",
            "1.0.0",
            "objects",
            "one",
            "package.bin",
        ])
        .unwrap(),
        BlobKey::from_segments([
            "lfs",
            owner,
            repo,
            "bb",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.zst",
        ])
        .unwrap(),
        BlobKey::from_segments(["releases", owner, repo, "1", "1", "release.bin"]).unwrap(),
        BlobKey::from_segments(["attachments", repo_id.as_str(), "uuid", "attachment.bin"])
            .unwrap(),
    ]
}

fn live_prefixes(owner: &str, repo: &str, repo_id: i64) -> Vec<BlobKey> {
    let repo_id = repo_id.to_string();
    vec![
        BlobKey::from_segments(["packages", owner, repo]).unwrap(),
        BlobKey::from_segments(["lfs", owner, repo]).unwrap(),
        BlobKey::from_segments(["releases", owner, repo]).unwrap(),
        BlobKey::from_segments(["attachments", repo_id.as_str()]).unwrap(),
    ]
}

/// A settled CI job, so the artifact tree the deletion has to reach exists.
///
/// Artifacts are the namespace that joins a repository through its *jobs*
/// rather than through a repository-shaped prefix, which makes them the one
/// most easily left behind — hence their own seed here rather than a reuse of
/// the blob prefixes above.
async fn seed_terminal_job(db: &rg_db::DatabaseConnection, repo_id: i64) -> i64 {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .expect("create historical pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create historical stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "unit", "echo ok", None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create historical job");
    let finished_at = chrono::Utc::now().naive_utc();
    rg_db::ops::pipeline_ops::update_job_result(
        db,
        job.id,
        "success",
        Some(0),
        None,
        None,
        Some(finished_at),
    )
    .await
    .expect("settle historical job");
    rg_db::ops::pipeline_ops::update_stage_status(db, stage.id, "success", None, Some(finished_at))
        .await
        .expect("settle historical stage");
    rg_db::ops::pipeline_ops::update_pipeline_status(
        db,
        pipeline.id,
        "success",
        None,
        Some(finished_at),
    )
    .await
    .expect("settle historical pipeline");
    job.id
}

/// The whole contract in one pass: the account's repository is retired through
/// the staged repository-deletion path before its row is touched, so the `200`
/// means the bytes are gone rather than merely unreachable.
#[tokio::test]
async fn deleting_an_account_retires_the_storage_of_its_repositories() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "acct-admin", "acct-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (victim_token, victim_id) =
        register_full(&base, "acct-owner", "acct-owner@example.com").await;
    let repo_id = create_repo(&base, &victim_token, "leftovers").await;
    let job_id = seed_terminal_job(&db, repo_id).await;

    let keys = representative_keys("acct-owner", "leftovers", repo_id);
    for (index, key) in keys.iter().enumerate() {
        state
            .blob_storage
            .put(key, format!("payload-{index}").as_bytes())
            .await
            .expect("seed representative repository blob");
    }

    let bare = state.repo_root.join("acct-owner/leftovers.git");
    let filesystem_directories = [
        state.repo_root.join("acct-owner.lfs/leftovers"),
        state.repo_root.join("_ci_cache").join(repo_id.to_string()),
        state
            .repo_root
            .join("_artifacts/jobs")
            .join(job_id.to_string()),
    ];
    for (index, directory) in filesystem_directories.iter().enumerate() {
        std::fs::create_dir_all(directory).expect("create repository filesystem namespace");
        std::fs::write(
            directory.join(format!("marker-{index}")),
            format!("filesystem payload {index}"),
        )
        .expect("seed repository filesystem namespace");
    }
    assert!(
        bare.exists(),
        "repository creation did not seed Git storage"
    );

    let response = client
        .delete(format!("{base}/api/v1/admin/users/{victim_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete account");
    assert_eq!(
        response.status(),
        200,
        "account deletion failed: {}",
        response.text().await.unwrap_or_default()
    );

    assert!(
        !bare.exists(),
        "the account is gone but its repository's Git tree is still live"
    );
    for directory in filesystem_directories {
        assert!(
            !directory.exists(),
            "the account is gone but {} is still live",
            directory.display()
        );
    }
    for prefix in live_prefixes("acct-owner", "leftovers", repo_id) {
        assert!(
            state
                .blob_storage
                .list(Some(&prefix))
                .await
                .expect("inventory live prefix")
                .is_empty(),
            "the account is gone but objects below {prefix} are still live"
        );
    }
    let tombstones =
        BlobKey::from_segments(["_deleted", "repositories", repo_id.to_string().as_str()]).unwrap();
    assert!(
        state
            .blob_storage
            .list(Some(&tombstones))
            .await
            .expect("inventory deletion tombstones")
            .is_empty(),
        "account deletion reported success while staged repository blobs remained"
    );

    assert!(
        rg_db::ops::repo_ops::find_by_id(&db, repo_id)
            .await
            .expect("read repository row after account deletion")
            .is_none(),
        "the repository row outlived the cascade this test exists to describe"
    );
    let missing = client
        .get(format!("{base}/api/v1/admin/users/{victim_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("read account after deletion");
    assert_eq!(
        missing.status(),
        404,
        "the account row survived its deletion"
    );
}

/// The other half of the same `DELETE`: the guest's *own* namespace is retired,
/// but what the guest uploaded into the host's repository is not the guest's to
/// take. The attachment on the host's issue, the release the guest published
/// there and its asset all stay — rows readable, bytes downloadable, uploader a
/// ghost.
///
/// Before `m20260805_000002_uploads_outlive_their_uploader` this endpoint
/// answered `200` while destroying all three rows through
/// `ON DELETE CASCADE` — and their blobs stayed live in a repository nothing
/// was going to retire, so the release simply lost its files.
#[tokio::test]
async fn deleting_an_account_keeps_what_it_uploaded_into_another_repository() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "ghost-admin", "ghost-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (host_token, _host_id) = register_full(&base, "ghost-host", "ghost-host@example.com").await;
    let (guest_token, guest_id) =
        register_full(&base, "ghost-guest", "ghost-guest@example.com").await;

    let repo_id = create_repo(&base, &host_token, "shared").await;
    let added = client
        .post(format!(
            "{base}/api/v1/repos/ghost-host/shared/collaborators"
        ))
        .bearer_auth(&host_token)
        .json(&serde_json::json!({"username": "ghost-guest", "permission": "write"}))
        .send()
        .await
        .expect("add the guest as a collaborator");
    assert_eq!(added.status(), 201);

    // The host's issue, the guest's attachment on it.
    let (_issue_id, issue_number) =
        create_issue(&base, &host_token, "ghost-host", "shared", "Needs a log").await;
    let uploaded = client
        .post(format!(
            "{base}/api/v1/repos/ghost-host/shared/issues/{issue_number}/assets"
        ))
        .bearer_auth(&guest_token)
        .multipart(
            reqwest::multipart::Form::new().part(
                "attachment",
                reqwest::multipart::Part::bytes(b"guest evidence".to_vec())
                    .file_name("trace.log")
                    .mime_str("text/plain")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .expect("upload an attachment into the host's issue");
    assert_eq!(
        uploaded.status(),
        201,
        "attachment upload failed: {}",
        uploaded.text().await.unwrap_or_default()
    );
    let attachment: serde_json::Value = uploaded.json().await.expect("read attachment body");
    let attachment_id = attachment["id"].as_i64().expect("attachment id");

    // The guest's release in the host's repository, and its asset.
    let release: serde_json::Value = client
        .post(format!("{base}/api/v1/repos/ghost-host/shared/releases"))
        .bearer_auth(&guest_token)
        .json(&serde_json::json!({"tag_name": "v1.0.0", "title": "First"}))
        .send()
        .await
        .expect("publish a release into the host's repository")
        .json()
        .await
        .expect("read release body");
    let release_id = release["id"].as_i64().expect("release id");
    let uploaded_asset = client
        .post(format!(
            "{base}/api/v1/repos/ghost-host/shared/releases/{release_id}/assets"
        ))
        .bearer_auth(&guest_token)
        .header("x-asset-filename", "binary.bin")
        .header("content-type", "application/octet-stream")
        .body(b"guest release payload".to_vec())
        .send()
        .await
        .expect("upload a release asset");
    assert_eq!(
        uploaded_asset.status(),
        201,
        "asset upload failed: {}",
        uploaded_asset.text().await.unwrap_or_default()
    );
    let asset: serde_json::Value = uploaded_asset.json().await.expect("read asset body");
    let asset_id = asset["id"].as_i64().expect("asset id");

    let deleted = client
        .delete(format!("{base}/api/v1/admin/users/{guest_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete the guest account");
    assert_eq!(
        deleted.status(),
        200,
        "account deletion failed: {}",
        deleted.text().await.unwrap_or_default()
    );

    // The rows the cascade used to take, and the ghost that replaced the
    // account they named.
    let attachment_row = rg_db::ops::attachment_ops::find_by_id(&db, attachment_id)
        .await
        .expect("read the attachment after its uploader was deleted")
        .expect("the guest's attachment was destroyed with the guest");
    assert_eq!(attachment_row.uploader_id, None, "uploader not ghosted");
    let asset_row = rg_db::ops::release_ops::find_asset_by_id(&db, asset_id)
        .await
        .expect("read the release asset after its uploader was deleted")
        .expect("the guest's release asset was destroyed with the guest");
    assert_eq!(asset_row.uploader_id, None, "uploader not ghosted");
    let release_row = rg_db::ops::release_ops::find_by_id(&db, release_id)
        .await
        .expect("read the release after its author was deleted")
        .expect("the guest's release was destroyed with the guest, taking its assets with it");
    assert_eq!(release_row.author_id, None, "author not ghosted");

    // And the bytes are still where the rows say they are — served, not merely
    // present, since that is what the repository's owner actually notices.
    let attachment_download = client
        .get(format!(
            "{base}/api/v1/repos/ghost-host/shared/issues/{issue_number}/assets/{attachment_id}"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("download the attachment after its uploader was deleted");
    assert_eq!(attachment_download.status(), 200);
    assert_eq!(
        attachment_download.bytes().await.expect("attachment bytes"),
        b"guest evidence".as_slice()
    );

    let asset_download = client
        .get(format!(
            "{base}/api/v1/repos/ghost-host/shared/releases/assets/{asset_id}/download"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("download the release asset after its uploader was deleted");
    assert_eq!(asset_download.status(), 200);
    assert_eq!(
        asset_download.bytes().await.expect("asset bytes"),
        b"guest release payload".as_slice()
    );

    assert!(
        rg_db::ops::repo_ops::find_by_id(&db, repo_id)
            .await
            .expect("read the host's repository")
            .is_some_and(|repo| repo.deleted_at.is_none()),
        "deleting the guest reached the host's repository"
    );
    assert!(
        state.repo_root.join("ghost-host/shared.git").exists(),
        "deleting the guest reached the host's Git storage"
    );
}

const GUEST_DEPLOY_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA guest";

/// The same `DELETE`, on what the guest *configured* rather than uploaded: the
/// host's CI secret, deploy key, commit status, board and logged hours all stay,
/// carrying a ghost author — and every endpoint that reads them still answers
/// `200`, which is the half a schema-level test cannot see.
///
/// Before `m20260805_000004_repo_config_outlives_its_author` this endpoint
/// answered `200` while `ON DELETE CASCADE` quietly took all five out of a
/// repository the guest did not own: the host's pipelines would start failing on
/// an empty variable, its deployments lose their key, and nobody would have a
/// reason to connect that to a collaborator's account going away
/// (card_a2123e31ee6e).
#[tokio::test]
async fn deleting_an_account_keeps_what_it_configured_in_another_repository() {
    let (base, db, _state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(&base, "cfg-admin", "cfg-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (host_token, _host_id) = register_full(&base, "cfg-host", "cfg-host@example.com").await;
    let (guest_token, guest_id) = register_full(&base, "cfg-guest", "cfg-guest@example.com").await;

    let repo_id = create_repo(&base, &host_token, "shared").await;
    let added = client
        .post(format!("{base}/api/v1/repos/cfg-host/shared/collaborators"))
        .bearer_auth(&host_token)
        .json(&serde_json::json!({"username": "cfg-guest", "permission": "admin"}))
        .send()
        .await
        .expect("add the guest as an administering collaborator");
    assert_eq!(added.status(), 201);

    let secret = client
        .put(format!(
            "{base}/api/v1/repos/cfg-host/shared/actions/secrets/DEPLOY_TOKEN"
        ))
        .bearer_auth(&guest_token)
        .json(&serde_json::json!({"value": "s3cr3t"}))
        .send()
        .await
        .expect("set a CI secret on the host's repository");
    assert_eq!(
        secret.status(),
        201,
        "setting the secret failed: {}",
        secret.text().await.unwrap_or_default()
    );

    let key = client
        .post(format!("{base}/api/v1/repos/cfg-host/shared/keys"))
        .bearer_auth(&guest_token)
        .json(&serde_json::json!({
            "title": "deployer",
            "key": GUEST_DEPLOY_KEY,
            "read_only": true
        }))
        .send()
        .await
        .expect("add a deploy key to the host's repository");
    assert_eq!(
        key.status(),
        201,
        "adding the deploy key failed: {}",
        key.text().await.unwrap_or_default()
    );

    let status = client
        .post(format!(
            "{base}/api/v1/repos/cfg-host/shared/statuses/c0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ff"
        ))
        .bearer_auth(&guest_token)
        .json(&serde_json::json!({"state": "success", "context": "ci/build"}))
        .send()
        .await
        .expect("report a commit status on the host's repository");
    assert_eq!(
        status.status(),
        201,
        "reporting the commit status failed: {}",
        status.text().await.unwrap_or_default()
    );

    let board = client
        .post(format!("{base}/api/v1/repos/cfg-host/shared/boards"))
        .bearer_auth(&guest_token)
        .json(&serde_json::json!({"name": "Roadmap"}))
        .send()
        .await
        .expect("create a board in the host's repository");
    assert_eq!(
        board.status(),
        201,
        "creating the board failed: {}",
        board.text().await.unwrap_or_default()
    );

    let (_issue_id, issue_number) =
        create_issue(&base, &host_token, "cfg-host", "shared", "Needs work").await;
    let logged = client
        .post(format!(
            "{base}/api/v1/repos/cfg-host/shared/issues/{issue_number}/time"
        ))
        .bearer_auth(&guest_token)
        .json(&serde_json::json!({"duration_minutes": 180}))
        .send()
        .await
        .expect("log time on the host's issue");
    assert_eq!(
        logged.status(),
        201,
        "logging time failed: {}",
        logged.text().await.unwrap_or_default()
    );

    let deleted = client
        .delete(format!("{base}/api/v1/admin/users/{guest_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete the guest account");
    assert_eq!(
        deleted.status(),
        200,
        "account deletion failed: {}",
        deleted.text().await.unwrap_or_default()
    );

    // The rows the cascade used to take, straight off the database.
    assert_eq!(
        rg_db::ops::ci_secret_ops::list_by_repo(&db, repo_id)
            .await
            .expect("read the repository's CI secrets")
            .len(),
        1,
        "the repository's CI secret died with the collaborator who set it"
    );
    assert_eq!(
        rg_db::ops::deploy_key_ops::list_by_repo(&db, repo_id)
            .await
            .expect("read the repository's deploy keys")
            .len(),
        1,
        "the repository's deploy key died with the collaborator who added it"
    );
    assert_eq!(
        rg_db::ops::board_ops::list_boards_by_repo(&db, repo_id)
            .await
            .expect("read the repository's boards")
            .len(),
        1,
        "the repository's board died with the collaborator who created it"
    );

    // And every reader of a ghosted row still answers, which is what the owner
    // of the repository actually meets.
    for path in [
        "actions/secrets".to_string(),
        "keys".to_string(),
        "boards".to_string(),
        "commits/c0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ff/statuses".to_string(),
        format!("issues/{issue_number}/time"),
        format!("issues/{issue_number}/time/total"),
    ] {
        let response = client
            .get(format!("{base}/api/v1/repos/cfg-host/shared/{path}"))
            .bearer_auth(&host_token)
            .send()
            .await
            .unwrap_or_else(|error| panic!("read {path} after the author was deleted: {error}"));
        assert_eq!(
            response.status(),
            200,
            "GET {path} failed once its rows carried a ghost author: {}",
            response.text().await.unwrap_or_default()
        );
    }
}

/// Author/action columns without a foreign key deliberately keep a durable
/// numeric snapshot. The contract is therefore two-sided: every authored row
/// stays in the repository, and every routed reader treats the now-unresolvable
/// id as a ghost instead of failing or filtering the row out
/// (card_7e4a56345094).
///
/// The PR fixture covers all of that subsystem's no-FK actor columns in one
/// timeline read: PR author/reviewer/auto-merge actor, review author, inline
/// comment author/resolver/suggestion actor, reviewer-request actor and merge
/// queue enqueuer. The other reads are one representative for each remaining
/// externally visible family named by the card.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn deleting_an_account_keeps_authored_history_readable_as_ghosts() {
    let (base, db, _state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "history-admin", "history-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (host_token, host_id) =
        register_full(&base, "history-host", "history-host@example.com").await;
    let (_guest_token, guest_id) =
        register_full(&base, "history-guest", "history-guest@example.com").await;
    let repo_id = create_repo(&base, &host_token, "shared-history").await;
    let now = chrono::Utc::now();

    let issue = rg_db::ops::issue_ops::create(
        &db,
        rg_db::entities::issue::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("The author may leave".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            author_id: Set(guest_id),
            assignee_id: Set(Some(guest_id)),
            milestone_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            deleted_at: Set(None),
        },
    )
    .await
    .expect("seed issue authored by the guest");
    let issue_comment = rg_db::ops::issue_comment_ops::create(
        &db,
        rg_db::entities::issue_comment::ActiveModel {
            id: NotSet,
            issue_id: Set(issue.id),
            author_id: Set(guest_id),
            body: Set("This context must stay".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("seed issue comment authored by the guest");

    let pull = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("Keep the review history".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(true),
            auto_merge_strategy: Set(Some("merge".to_string())),
            auto_merge_enabled_by_id: Set(Some(guest_id)),
            auto_merge_enabled_at: Set(Some(now)),
            author_id: Set(guest_id),
            reviewer_id: Set(Some(guest_id)),
            head_branch: Set("history".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some("1".repeat(40))),
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
    .expect("seed pull request authored by the guest");
    let review = rg_db::ops::pr_review_ops::create(
        &db,
        rg_db::entities::pr_review::ActiveModel {
            id: NotSet,
            pr_id: Set(pull.id),
            repo_id: Set(repo_id),
            reviewer_id: Set(guest_id),
            action: Set("comment".to_string()),
            body: Set(Some("Historical review".to_string())),
            commit_id: Set(pull.head_sha.clone()),
            created_at: Set(now),
        },
    )
    .await
    .expect("seed review authored by the guest");
    let review_comment = rg_db::ops::review_comment_ops::create(
        &db,
        rg_db::entities::review_comment::ActiveModel {
            id: NotSet,
            review_id: Set(review.id),
            pr_id: Set(pull.id),
            author_id: Set(guest_id),
            path: Set("src/lib.rs".to_string()),
            position: Set(None),
            line: Set(Some(1)),
            start_line: Set(None),
            side: Set(Some("RIGHT".to_string())),
            start_side: Set(None),
            body: Set("Keep this thread".to_string()),
            suggestion: Set(Some("replacement".to_string())),
            suggestion_applied_at: Set(Some(now)),
            suggestion_applied_by_id: Set(Some(guest_id)),
            suggestion_commit_sha: Set(Some("2".repeat(40))),
            commit_id: Set(pull.head_sha.clone()),
            reply_to_id: Set(None),
            resolved_at: Set(Some(now)),
            resolved_by_id: Set(Some(guest_id)),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("seed review comment authored and resolved by the guest");
    let reviewer_request = rg_db::ops::pr_reviewer_request_ops::create(
        &db,
        rg_db::entities::pr_reviewer_request::ActiveModel {
            id: NotSet,
            pr_id: Set(pull.id),
            // The reviewer stays live so the row is not removed by its separate,
            // deliberate reviewer_id CASCADE. This fixture is about the no-FK
            // requested_by_id actor.
            reviewer_id: Set(host_id),
            requested_by_id: Set(guest_id),
            created_at: Set(now),
        },
    )
    .await
    .expect("seed reviewer request made by the guest");
    let queue_entry =
        rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pull.id, guest_id, "merge")
            .await
            .expect("seed merge queue entry made by the guest");

    let wiki_page = rg_db::ops::wiki_page_ops::create(
        &db,
        rg_db::entities::wiki_page::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            title: Set("Ghosts".to_string()),
            content: Set("Authored content stays".to_string()),
            message: Set(Some("Initial page".to_string())),
            author_id: Set(Some(guest_id)),
            sha: Set(None),
            edit_version: Set(1),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("seed wiki page authored by the guest");
    let wiki_revision = rg_db::ops::wiki_revision_ops::create(
        &db,
        rg_db::entities::wiki_revision::ActiveModel {
            id: NotSet,
            wiki_page_id: Set(wiki_page.id),
            content: Set(wiki_page.content.clone()),
            message: Set(wiki_page.message.clone()),
            author_id: Set(Some(guest_id)),
            version: Set(1),
            created_at: Set(now),
        },
    )
    .await
    .expect("seed wiki revision authored by the guest");

    let registry = rg_db::ops::package_registry_ops::find_or_create(&db, repo_id, "generic")
        .await
        .expect("seed generic package registry");
    let package = rg_db::ops::package_ops::create(
        &db,
        registry.id,
        guest_id,
        "ghost-package",
        Some("survives its first publisher"),
        None,
        None,
    )
    .await
    .expect("seed package first published by the guest");
    let package_version = rg_db::ops::package_version_ops::create(
        &db,
        package.id,
        "1.0.0",
        None,
        Some("1.0.0"),
        None,
        0,
        None,
        Some(guest_id),
    )
    .await
    .expect("seed package version authored by the guest");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        &"3".repeat(40),
        "refs/heads/main",
        "manual",
        Some(guest_id),
    )
    .await
    .expect("seed pipeline triggered by the guest");

    let audit = rg_db::ops::audit_log_ops::insert(
        &db,
        rg_db::entities::audit_log::ActiveModel {
            id: NotSet,
            user_id: Set(Some(guest_id)),
            username: Set(Some("history-guest".to_string())),
            action: Set("history.seed".to_string()),
            resource_type: Set(Some("repository".to_string())),
            resource_id: Set(Some(repo_id)),
            resource_name: Set(Some("history-host/shared-history".to_string())),
            ip_address: Set(None),
            user_agent: Set(None),
            details: Set(None),
            created_at: Set(now),
        },
    )
    .await
    .expect("seed durable audit actor snapshot");
    let oci_repo = rg_db::ops::oci_ops::find_or_create_repo(
        &db,
        repo_id,
        "history-host/shared-history",
        host_id,
    )
    .await
    .expect("seed OCI repository owned by the host namespace");
    let manifest = rg_db::ops::oci_ops::upsert_tag_manifest(
        &db,
        oci_repo.id,
        "latest",
        &format!("sha256:{}", "4".repeat(64)),
        "application/vnd.oci.image.manifest.v1+json",
        2,
        "{}",
        2,
        Some(guest_id),
        &[],
    )
    .await
    .expect("seed OCI manifest pushed by the guest");

    let deleted = client
        .delete(format!("{base}/api/v1/admin/users/{guest_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete the guest account");
    assert_eq!(
        deleted.status(),
        200,
        "account deletion failed: {}",
        deleted.text().await.unwrap_or_default()
    );
    assert!(
        rg_db::ops::user_ops::find_by_id(&db, guest_id)
            .await
            .expect("read deleted guest")
            .is_none(),
        "the fixture did not create a ghost: the account row still exists"
    );

    // The no-FK ids are durable snapshots: deleting the referenced user must
    // neither null them nor remove their rows.
    assert_eq!(
        rg_db::ops::issue_ops::find_by_id(&db, issue.id)
            .await
            .expect("read issue after author deletion")
            .expect("issue disappeared with its author")
            .author_id,
        guest_id
    );
    assert_eq!(
        rg_db::ops::issue_comment_ops::find_by_id(&db, issue_comment.id)
            .await
            .expect("read issue comment after author deletion")
            .expect("issue comment disappeared with its author")
            .author_id,
        guest_id
    );
    let pull_after = rg_db::ops::pull_request_ops::find_by_id(&db, pull.id)
        .await
        .expect("read pull request after author deletion")
        .expect("pull request disappeared with its author");
    assert_eq!(pull_after.author_id, guest_id);
    assert_eq!(pull_after.reviewer_id, Some(guest_id));
    assert_eq!(pull_after.auto_merge_enabled_by_id, Some(guest_id));
    assert_eq!(
        rg_db::ops::pr_review_ops::find_by_id(&db, review.id)
            .await
            .expect("read review after reviewer deletion")
            .expect("review disappeared with its reviewer")
            .reviewer_id,
        guest_id
    );
    let comment_after = rg_db::ops::review_comment_ops::find_by_id(&db, review_comment.id)
        .await
        .expect("read review comment after author deletion")
        .expect("review comment disappeared with its author");
    assert_eq!(comment_after.author_id, guest_id);
    assert_eq!(comment_after.suggestion_applied_by_id, Some(guest_id));
    assert_eq!(comment_after.resolved_by_id, Some(guest_id));
    assert_eq!(
        rg_db::ops::pr_reviewer_request_ops::find(&db, pull.id, host_id)
            .await
            .expect("read reviewer request after requester deletion")
            .expect("reviewer request disappeared with its requester")
            .id,
        reviewer_request.id
    );
    assert_eq!(
        rg_db::ops::merge_queue_ops::find_by_pr(&db, pull.id)
            .await
            .expect("read queue entry after enqueuer deletion")
            .expect("queue entry disappeared with its enqueuer")
            .id,
        queue_entry.id
    );
    assert_eq!(
        rg_db::ops::wiki_page_ops::find_by_repo_and_title(&db, repo_id, "Ghosts")
            .await
            .expect("read wiki page after author deletion")
            .expect("wiki page disappeared with its author")
            .author_id,
        Some(guest_id)
    );
    assert_eq!(
        rg_db::ops::wiki_revision_ops::find_by_id(&db, wiki_revision.id)
            .await
            .expect("read wiki revision after author deletion")
            .expect("wiki revision disappeared with its author")
            .author_id,
        Some(guest_id)
    );
    assert_eq!(
        rg_db::ops::package_ops::find_by_registry_and_name(&db, registry.id, "ghost-package")
            .await
            .expect("read package after publisher deletion")
            .expect("package disappeared with its publisher")
            .owner_id,
        guest_id
    );
    assert_eq!(
        rg_db::ops::package_version_ops::find_by_package_and_version(&db, package.id, "1.0.0",)
            .await
            .expect("read package version after author deletion")
            .expect("package version disappeared with its author")
            .id,
        package_version.id
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline.id)
            .await
            .expect("read pipeline after trigger actor deletion")
            .expect("pipeline disappeared with its trigger actor")
            .triggered_by,
        Some(guest_id)
    );
    assert_eq!(
        rg_db::ops::audit_log_ops::find_by_id(&db, audit.id)
            .await
            .expect("read audit row after actor deletion")
            .expect("audit history disappeared with its actor")
            .username
            .as_deref(),
        Some("history-guest")
    );
    assert_eq!(
        rg_db::ops::oci_ops::find_manifest_by_digest(&db, oci_repo.id, &manifest.digest)
            .await
            .expect("read OCI manifest after pusher deletion")
            .expect("OCI manifest disappeared with its pusher")
            .push_by,
        Some(guest_id)
    );

    // Routed readers: no 500, no empty collection, and the surfaces that
    // enrich users explicitly render the missing account as `null`.
    let issue_response = client
        .get(format!(
            "{base}/api/v1/repos/history-host/shared-history/issues/1"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read issue with a ghost author");
    assert_eq!(issue_response.status(), 200);
    let issue_body: serde_json::Value = issue_response.json().await.expect("issue body");
    assert!(
        issue_body["author"].is_null(),
        "issue author is not a ghost: {issue_body}"
    );
    assert_eq!(issue_body["assignee_id"], guest_id);

    let comments_response = client
        .get(format!(
            "{base}/api/v1/repos/history-host/shared-history/issues/1/comments"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read issue comments with a ghost author");
    assert_eq!(comments_response.status(), 200);
    let comments: serde_json::Value = comments_response.json().await.expect("comments body");
    assert_eq!(comments.as_array().map(Vec::len), Some(1));
    assert!(
        comments[0]["author"].is_null(),
        "comment author is not a ghost: {comments}"
    );

    let pull_response = client
        .get(format!(
            "{base}/api/v1/repos/history-host/shared-history/pulls/1"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read pull request with a ghost author");
    assert_eq!(pull_response.status(), 200);
    let pull_body: serde_json::Value = pull_response.json().await.expect("pull request body");
    assert_eq!(pull_body["author_id"], guest_id);

    let timeline_response = client
        .get(format!(
            "{base}/api/v1/repos/history-host/shared-history/pulls/1/timeline"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read pull request timeline with ghost actors");
    assert_eq!(timeline_response.status(), 200);
    let timeline: Vec<serde_json::Value> = timeline_response.json().await.expect("timeline body");
    assert!(
        timeline.len() >= 5,
        "authored PR history was filtered out: {timeline:?}"
    );
    assert!(
        timeline
            .iter()
            .filter(|event| event["actor"].is_null())
            .count()
            >= 5,
        "deleted PR actors were not rendered as ghosts: {timeline:?}"
    );

    let wiki_response = client
        .get(format!(
            "{base}/api/v1/repos/history-host/shared-history/wiki/Ghosts"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read wiki page with a ghost author");
    assert_eq!(wiki_response.status(), 200);
    let wiki_body: serde_json::Value = wiki_response.json().await.expect("wiki body");
    assert_eq!(wiki_body["author_id"], guest_id);

    let wiki_history_response = client
        .get(format!(
            "{base}/api/v1/repos/history-host/shared-history/wiki/Ghosts/history"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read wiki history with a ghost author");
    assert_eq!(wiki_history_response.status(), 200);
    let wiki_history: serde_json::Value = wiki_history_response
        .json()
        .await
        .expect("wiki history body");
    assert_eq!(wiki_history.as_array().map(Vec::len), Some(1));
    assert_eq!(wiki_history[0]["author_id"], guest_id);

    let packages_response = client
        .get(format!(
            "{base}/api/v1/repos/history-host/shared-history/packages/generic/list"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read package list after publisher deletion");
    assert_eq!(packages_response.status(), 200);
    let packages: serde_json::Value = packages_response.json().await.expect("packages body");
    assert!(
        packages["packages"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["name"] == "ghost-package")),
        "the ghost-authored package was filtered out: {packages}"
    );

    let pipeline_response = client
        .get(format!(
            "{base}/api/v1/repos/history-host/shared-history/pipelines/{}",
            pipeline.id
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read pipeline after trigger actor deletion");
    assert_eq!(pipeline_response.status(), 200);
    let pipeline_body: serde_json::Value = pipeline_response.json().await.expect("pipeline body");
    assert_eq!(pipeline_body["pipeline"]["triggered_by"], guest_id);
}

/// The organization is the ownership the account cannot take with it:
/// `organizations.owner_id` has no foreign key, so the row would survive
/// pointing at nothing — while the cascade on `repositories.owner_id` still
/// took its repositories. Refusing is the honest answer, and it must leave both
/// sides exactly as they were.
#[tokio::test]
async fn an_account_that_still_owns_an_organization_is_not_deleted() {
    let (base, db, _state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(&base, "org-admin", "org-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (owner_token, owner_id) = register_full(&base, "org-owner", "org-owner@example.com").await;

    let created = client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "name": "kept-org", "display_name": "Kept Org" }))
        .send()
        .await
        .expect("create organization");
    assert_eq!(created.status(), 201);

    let refused = client
        .delete(format!("{base}/api/v1/admin/users/{owner_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete an account that owns an organization");
    assert_eq!(
        refused.status(),
        409,
        "deleting the owner of an organization must be refused, not absorbed"
    );
    let body: serde_json::Value = refused.json().await.expect("read refusal body");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("kept-org")),
        "the refusal must name what is in the way (body: {body})"
    );

    let still_there = client
        .get(format!("{base}/api/v1/admin/users/{owner_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("read account after the refusal");
    assert_eq!(
        still_there.status(),
        200,
        "a refused deletion removed the account anyway"
    );
    assert!(
        rg_db::ops::org_ops::get_org_by_name(&db, "kept-org")
            .await
            .expect("read organization after the refusal")
            .is_some(),
        "a refused deletion touched the organization"
    );
}

/// The storage half fails after the account's repository has already been
/// staged aside. Nothing is deleted: not the bytes, not the repository row, not
/// the account — so the operator can fix the backend and repeat the request.
#[tokio::test]
async fn storage_failure_does_not_delete_the_account() {
    let app = spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&app.base, "fault-admin", "fault-admin@example.com").await;
    promote_user_to_admin(&app.db, admin_id).await;
    let (owner_token, owner_id) =
        register_full(&app.base, "fault-owner", "fault-owner@example.com").await;
    let repo_id = create_repo(&app.base, &owner_token, "survivor").await;

    let package = representative_keys("fault-owner", "survivor", repo_id)
        .into_iter()
        .next()
        .unwrap();
    let package_path = package
        .as_str()
        .split('/')
        .fold(app.repo_root.clone(), |path, segment| path.join(segment));
    std::fs::create_dir_all(package_path.parent().unwrap()).unwrap();
    std::fs::write(&package_path, b"keep this blob").unwrap();
    let bare = app.repo_root.join("fault-owner/survivor.git");

    app.blob_faults.fail_delete();
    let response = client
        .delete(format!("{}/api/v1/admin/users/{owner_id}", app.base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete account with failed blob storage");
    assert_eq!(
        response.status(),
        500,
        "a storage failure was reported as a completed account deletion"
    );

    app.blob_faults.heal();
    assert!(
        bare.exists(),
        "the failed deletion did not restore the repository's Git tree"
    );
    assert_eq!(std::fs::read(&package_path).unwrap(), b"keep this blob");
    assert!(
        rg_db::ops::repo_ops::find_by_id(&app.db, repo_id)
            .await
            .expect("read repository row after the failed deletion")
            .is_some_and(|repo| repo.deleted_at.is_none()),
        "the failed deletion removed the repository row anyway"
    );
    let still_there = client
        .get(format!("{}/api/v1/admin/users/{owner_id}", app.base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("read account after the failed deletion");
    assert_eq!(
        still_there.status(),
        200,
        "the failed deletion removed the account anyway"
    );
}
