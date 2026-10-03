//! A runner on its own machine must be able to publish a CI artifact
//! (card_ba2e171eb366).
//!
//! The publish route takes JSON metadata naming a file that must already exist
//! **server-side**, which no runner on another host can produce — so for as
//! long as it was the only artifact route, the whole floor above it (the
//! pipeline's artifact list, the download, the retention policy and its
//! settings page) described something no instance could ever contain. The only
//! callers were fixtures that wrote the file themselves through the shared
//! filesystem the tests happen to have.
//!
//! These tests drive `rg-runner`'s own client functions — the code the shipped
//! `plombir-git-runner` binary runs — against the live router, and then read the
//! artifact back through the two routes a user actually uses.

use crate::common::{register_full, spawn_test_app_with_db};

/// The artifact declaration as `pipeline_jobs.artifacts` carries it — the same
/// JSON `rg-ci` writes when it translates `artifacts:` or
/// `actions/upload-artifact`, asserted on there by
/// `a_workflow_upload_step_reaches_the_job_row_as_a_declaration`.
const DECLARATION: &str = r#"{"name":"build-report","paths":["out/report.txt"]}"#;

async fn create_private_repo(base: &str, token: &str, name: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "create private repo failed");
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// A pipeline holding one pending job that declares an artifact.
async fn create_declaring_job(db: &rg_db::DatabaseConnection, repo_id: i64) -> (i64, i64) {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "build", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "build",
        "echo ok",
        None,
        None,
        None,
        None,
        None,
        Some(DECLARATION),
        false,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    (pipeline.id, job.id)
}

/// A one-file `tar`, the shape the runner packs a declared path into.
fn packed_archive(directory: &std::path::Path, content: &[u8]) -> std::path::PathBuf {
    let archive = directory.join("artifact.tar");
    let mut builder = tar::Builder::new(std::fs::File::create(&archive).unwrap());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "out/report.txt", content)
        .unwrap();
    builder.finish().unwrap();
    archive
}

/// The declaration reaches the runner over the wire, and what the runner then
/// stages and publishes is what the repository's artifact list serves.
#[tokio::test]
async fn a_runner_publishes_the_artifact_its_job_declared() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "artifact_flow", "artifact_flow@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "artifact-flow").await;
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "publishing-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let (pipeline_id, job_id) = create_declaring_job(&db, repo_id).await;

    // The poll body is where the declaration has to survive the wire: it used
    // to stop at the database, so a runner had nothing to act on even once both
    // routes existed. Read as JSON rather than through the runner's own struct
    // because a field the runner did *not* deserialize would be invisible in
    // the typed view — `rg-runner`'s own
    // `a_polled_job_carries_the_artifact_its_workflow_declared` holds the other
    // half, that the runner reads exactly these two names.
    let polled = client
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/poll?timeout=5",
            runner.id
        ))
        .bearer_auth(&runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(polled.status(), 200);
    let polled = polled.json::<serde_json::Value>().await.unwrap();
    assert_eq!(polled["job_id"], job_id);
    assert_eq!(polled["artifact_name"], "build-report");
    assert_eq!(
        polled["artifact_paths"],
        serde_json::json!(["out/report.txt"])
    );

    let workspace = tempfile::tempdir().unwrap();
    let archive = packed_archive(workspace.path(), b"artifact-bytes");
    let staged =
        rg_runner::api::stage_artifact(&client, &base, runner.id, job_id, &runner_token, &archive)
            .await
            .expect("a runner must be able to hand the server artifact bytes");
    rg_runner::api::publish_artifact(
        &client,
        &base,
        runner.id,
        job_id,
        &runner_token,
        polled["artifact_name"].as_str().unwrap(),
        &staged,
    )
    .await
    .expect("a staged archive must be publishable as an artifact");

    // Storage owns the bytes now, so the staged copy is a second full copy of
    // the artifact that no row names and no retention sweep walks. Asserted
    // here rather than after the job settles: publication is the moment it
    // stops being needed, and waiting for the settle would keep an
    // artifact-sized file alive for the whole rest of the run.
    assert!(
        !std::path::Path::new(&staged).exists(),
        "publishing left {staged} behind — every artifact would be stored twice"
    );

    // The list a repository page shows — the one that was structurally always
    // empty.
    let listed = client
        .get(format!(
            "{base}/api/v1/repos/artifact_flow/artifact-flow/pipelines/{pipeline_id}/artifacts"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let artifacts = listed.json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        artifacts.as_array().map(Vec::len),
        Some(1),
        "the pipeline's artifact list must carry what the runner published: {artifacts}"
    );
    assert_eq!(artifacts[0]["name"], "build-report");
    assert_eq!(artifacts[0]["job_id"], job_id);
    let artifact_id = artifacts[0]["id"].as_i64().unwrap();

    let downloaded = client
        .get(format!("{base}/api/v1/artifacts/{artifact_id}/download"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), 200);
    let bytes = downloaded.bytes().await.unwrap();
    let mut unpacked = tar::Archive::new(std::io::Cursor::new(bytes));
    let mut found = Vec::new();
    for entry in unpacked.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().display().to_string();
        let mut content = String::new();
        std::io::Read::read_to_string(&mut entry, &mut content).unwrap();
        found.push((path, content));
    }
    assert_eq!(
        found,
        vec![("out/report.txt".to_string(), "artifact-bytes".to_string())],
        "the bytes a download serves have to be the bytes the runner staged"
    );
}

/// Staging is scoped to the job the runner was assigned, exactly as every other
/// runner route is: a runner that holds a token cannot stage bytes against
/// somebody else's job.
#[tokio::test]
async fn staging_is_refused_for_a_job_this_runner_was_not_assigned() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "artifact_gate", "artifact_gate@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "artifact-gate").await;
    let (mine, my_token) =
        rg_db::ops::runner_ops::register_runner(&db, repo_id, "mine", "", None, None, None)
            .await
            .unwrap();
    let (theirs, _their_token) =
        rg_db::ops::runner_ops::register_runner(&db, repo_id, "theirs", "", None, None, None)
            .await
            .unwrap();
    let (_pipeline_id, job_id) = create_declaring_job(&db, repo_id).await;
    rg_db::ops::pipeline_ops::assign_job(&db, job_id, theirs.id)
        .await
        .unwrap();

    let workspace = tempfile::tempdir().unwrap();
    let archive = packed_archive(workspace.path(), b"not yours");
    let error =
        rg_runner::api::stage_artifact(&client, &base, mine.id, job_id, &my_token, &archive)
            .await
            .expect_err("a job assigned to another runner must not accept these bytes");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("404"),
        "another runner's job must be answered exactly as an unknown one is: {rendered}"
    );
}

/// A job that staged bytes and never published them must not leave them behind.
///
/// Nothing else would ever come back for them: no artifact row names a staged
/// archive, and retention walks rows. A runner killed between the two calls
/// would otherwise strand a full copy of its build output on the server for
/// good.
#[tokio::test]
async fn a_settled_job_leaves_no_staged_archive_behind() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "artifact_stage", "artifact_stage@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "artifact-stage").await;
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "abandoning-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let (_pipeline_id, job_id) = create_declaring_job(&db, repo_id).await;
    rg_db::ops::pipeline_ops::assign_job(&db, job_id, runner.id)
        .await
        .unwrap();

    let workspace = tempfile::tempdir().unwrap();
    let archive = packed_archive(workspace.path(), b"abandoned");
    let staged =
        rg_runner::api::stage_artifact(&client, &base, runner.id, job_id, &runner_token, &archive)
            .await
            .expect("staging must succeed for an assigned job");
    assert!(
        std::path::Path::new(&staged).exists(),
        "the staged archive has to be on disk for this test to prove anything"
    );

    // The runner never publishes; it reports the job finished instead.
    let finished = client
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/finish",
            runner.id
        ))
        .bearer_auth(&runner_token)
        .json(&serde_json::json!({ "status": "success", "exit_code": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(finished.status(), 200);

    assert!(
        !std::path::Path::new(&staged).exists(),
        "a settled job left {staged} behind — an artifact-sized file no row names"
    );
}
