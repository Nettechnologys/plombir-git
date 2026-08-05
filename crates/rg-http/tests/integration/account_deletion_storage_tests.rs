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
