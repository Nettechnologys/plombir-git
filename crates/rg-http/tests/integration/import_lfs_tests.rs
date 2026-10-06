//! Regression coverage for card_bc7c8ddbf9b7: an imported repository must
//! bring its LFS objects, not only the pointer files.
//!
//! The import ran `git clone --bare` and nothing else. The pointers came along
//! with the history, the content stayed on the source, and the repository
//! arrived looking complete while every LFS file in it answered `404` to
//! `git lfs pull` — with nothing in the task to say so.
//!
//! The source here is a real LFS server: this same instance, serving a public
//! repository whose objects were uploaded the way `git lfs push` uploads them.
//! The import reaches it through `[imports].trusted_origins`, the operator's
//! opt-in for a private self-hosted source. One pointer names an object the
//! source never received, and the import has to name it — by oid and path —
//! while still bringing everything else. The check afterwards is what
//! `git lfs pull` does against the imported repository: clone it, read the
//! pointer files of the checkout, ask the batch endpoint the client derives
//! from the clone URL, download, and compare.

use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::common::{
    build_test_app_state_with, register_full, setup_test_db, wait_for_listener, StateOverrides,
};

const SOURCE_OWNER: &str = "lfs_import_source";
const SOURCE_REPO: &str = "lfs-source";
const IMPORTER: &str = "lfs_import_target";
const TARGET_REPO: &str = "imported-assets";

fn oid_of(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

fn pointer_text(oid: &str, size: usize) -> String {
    format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
}

/// An app that trusts its own origin as an import source, so it can import
/// from itself the way an instance imports from a private self-hosted forge.
async fn spawn_self_trusting_app() -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test app");
    let address = listener.local_addr().expect("test app address");
    let origin = format!("http://{address}");
    let state = build_test_app_state_with(
        db,
        repo_root,
        StateOverrides {
            trusted_import_origins: Some(
                rg_core::import::trust::TrustedImportOrigins::parse(std::slice::from_ref(&origin))
                    .expect("trusted origin config"),
            ),
            ..Default::default()
        },
    );
    let app = rg_http::create_router_for_test(state);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("serve test app");
    });
    wait_for_listener(&address.to_string()).await;
    origin
}

async fn create_public_repo(base: &str, token: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": SOURCE_REPO,
            "is_private": false,
            "auto_init": true,
            "readme": "default",
        }))
        .send()
        .await
        .expect("create source repository");
    assert_eq!(response.status(), 201, "seeding the source failed");
}

/// Store an object the way `git lfs push` does: announce it through the batch
/// API, then `PUT` the bytes to the signed href it hands back.
async fn upload_object(base: &str, token: &str, payload: &[u8]) -> String {
    let oid = oid_of(payload);
    let client = reqwest::Client::new();
    let batch = client
        .post(format!(
            "{base}/api/v1/repos/{SOURCE_OWNER}/{SOURCE_REPO}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid, "size": payload.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .expect("upload batch");
    assert_eq!(batch.status(), 200, "LFS upload batch failed");
    let batch = batch.json::<serde_json::Value>().await.expect("batch body");
    let href = batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .expect("upload href")
        .to_string();
    let stored = client
        .put(href)
        .body(payload.to_vec())
        .send()
        .await
        .expect("upload object");
    assert_eq!(stored.status(), 200, "LFS object upload failed");
    oid
}

async fn commit_file(base: &str, token: &str, path: &str, content: &str) {
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{SOURCE_OWNER}/{SOURCE_REPO}/contents/{path}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "content": content,
            "message": format!("add {path}"),
        }))
        .send()
        .await
        .expect("commit request");
    assert_eq!(
        response.status(),
        200,
        "the fixture needs {path} committed: {}",
        response.text().await.unwrap_or_default()
    );
}

/// Start the import and wait for it to finish, returning the task as the
/// status endpoint reports it.
async fn import_and_wait(base: &str, token: &str) -> serde_json::Value {
    let client = reqwest::Client::new();
    let started = client
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "platform": "git",
            "source_url": format!("{base}/git/{SOURCE_OWNER}/{SOURCE_REPO}.git"),
            "target_owner": IMPORTER,
            "target_name": TARGET_REPO,
            "import_repo": true,
            "import_issues": false,
            "import_pull_requests": false,
            "import_wiki": false,
            "import_releases": false,
            "import_labels": false,
            "import_milestones": false
        }))
        .send()
        .await
        .expect("start import");
    assert_eq!(started.status(), 201, "the import was not accepted");
    let task_id = started.json::<serde_json::Value>().await.expect("task")["id"]
        .as_i64()
        .expect("task id");

    for _ in 0..600 {
        let task = client
            .get(format!("{base}/api/v1/imports/{task_id}"))
            .bearer_auth(token)
            .send()
            .await
            .expect("read import status")
            .json::<serde_json::Value>()
            .await
            .expect("status body");
        match task["status"].as_str() {
            Some("completed") | Some("failed") => return task,
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("the import did not finish");
}

/// What `git lfs pull` asks the imported repository for: the batch endpoint
/// it derives from the clone URL, unauthenticated for a public repository.
async fn pull_batch(clone_url: &str, objects: &[(String, usize)]) -> serde_json::Value {
    let response = reqwest::Client::new()
        .post(format!("{clone_url}/info/lfs/objects/batch"))
        .header("Accept", "application/vnd.git-lfs+json")
        .header("Content-Type", "application/vnd.git-lfs+json")
        .body(
            serde_json::json!({
                "operation": "download",
                "transfers": ["basic"],
                "objects": objects
                    .iter()
                    .map(|(oid, size)| serde_json::json!({"oid": oid, "size": size}))
                    .collect::<Vec<_>>(),
            })
            .to_string(),
        )
        .send()
        .await
        .expect("pull batch");
    assert_eq!(
        response.status(),
        200,
        "the imported repository's batch failed"
    );
    response.json().await.expect("pull batch body")
}

#[tokio::test]
async fn an_import_brings_its_lfs_objects_and_names_the_one_the_source_lacks() {
    let base = spawn_self_trusting_app().await;
    let (source_token, _) =
        register_full(&base, SOURCE_OWNER, &format!("{SOURCE_OWNER}@example.com")).await;
    let (importer_token, _) =
        register_full(&base, IMPORTER, &format!("{IMPORTER}@example.com")).await;
    create_public_repo(&base, &source_token).await;

    let texture = b"a texture the import has to bring across".to_vec();
    let model = b"a model in a nested directory, committed twice".to_vec();
    let texture_oid = upload_object(&base, &source_token, &texture).await;
    let model_oid = upload_object(&base, &source_token, &model).await;
    // Committed, never pushed to the source's LFS store.
    let lost = b"content that only ever existed on someone's laptop".to_vec();
    let lost_oid = oid_of(&lost);

    commit_file(
        &base,
        &source_token,
        "assets/texture.png",
        &pointer_text(&texture_oid, texture.len()),
    )
    .await;
    commit_file(
        &base,
        &source_token,
        "assets/nested/model.fbx",
        &pointer_text(&model_oid, model.len()),
    )
    .await;
    // The same object under a second name is one object to fetch, not two.
    commit_file(
        &base,
        &source_token,
        "copies/model.fbx",
        &pointer_text(&model_oid, model.len()),
    )
    .await;
    commit_file(
        &base,
        &source_token,
        "assets/lost.bin",
        &pointer_text(&lost_oid, lost.len()),
    )
    .await;

    let task = import_and_wait(&base, &importer_token).await;
    assert_eq!(
        task["status"], "completed",
        "one missing object must not fail the import: {task}"
    );
    let stage = task["stage"].as_str().expect("stage");
    assert!(
        stage.contains("1 LFS object(s) could not be fetched"),
        "the line the import page shows must not read as a clean import: {stage}"
    );
    let stats: serde_json::Value =
        serde_json::from_str(task["stats"].as_str().expect("stats")).expect("stats JSON");
    assert_eq!(stats["repo_cloned"], true, "{stats}");
    assert_eq!(stats["lfs_objects_imported"], 2, "{stats}");
    let failed = stats["lfs_objects_failed"]
        .as_array()
        .expect("failure list");
    assert_eq!(failed.len(), 1, "{stats}");
    assert_eq!(failed[0]["oid"], lost_oid.as_str());
    assert_eq!(failed[0]["path"], "assets/lost.bin");
    assert!(
        failed[0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("404")),
        "{stats}"
    );

    // `git lfs pull`, step by step: clone, read the pointers the checkout
    // holds, ask the derived batch endpoint, download, compare.
    let clone_url = format!("{base}/git/{IMPORTER}/{TARGET_REPO}.git");
    let checkout = tempfile::tempdir().expect("checkout dir");
    // On the blocking pool: the server under test runs on this runtime, and a
    // `git clone` that held the test's thread would wait on it forever.
    let work = checkout.path().join("work");
    let cloned = {
        let (clone_url, work) = (clone_url.clone(), work.clone());
        tokio::task::spawn_blocking(move || {
            rg_git::cli_gateway::global_gateway()
                .as_ref()
                .expect("git gateway must initialize")
                .run(&["clone", "-q", &clone_url, &work.to_string_lossy()], None)
                .expect("run git clone")
        })
        .await
        .expect("git clone task")
    };
    assert!(
        cloned.success(),
        "the imported repository does not clone: {}",
        cloned.stderr_str()
    );
    let mut wanted = Vec::new();
    for path in [
        "assets/texture.png",
        "assets/nested/model.fbx",
        "copies/model.fbx",
        "assets/lost.bin",
    ] {
        let bytes = std::fs::read(work.join(path)).expect("pointer file in the checkout");
        let pointer = rg_core::lfs::pointer::parse(&bytes)
            .unwrap_or_else(|| panic!("{path} is not a pointer in the imported repository"));
        wanted.push((pointer.oid, pointer.size as usize));
    }
    wanted.sort();
    wanted.dedup();

    let answer = pull_batch(&clone_url, &wanted).await;
    let objects = answer["objects"].as_array().expect("batch objects");
    for (oid, payload) in [(&texture_oid, &texture), (&model_oid, &model)] {
        let object = objects
            .iter()
            .find(|object| object["oid"] == oid.as_str())
            .unwrap_or_else(|| panic!("the batch did not answer for {oid}"));
        let href = object["actions"]["download"]["href"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("the imported repository offers no download for {oid}: {object}")
            });
        let downloaded = reqwest::get(href).await.expect("download");
        assert_eq!(downloaded.status(), 200, "download of {oid}");
        assert_eq!(
            downloaded.bytes().await.expect("object bytes").as_ref(),
            payload.as_slice(),
            "the imported repository serves different bytes for {oid}"
        );
    }
    let lost_answer = objects
        .iter()
        .find(|object| object["oid"] == lost_oid.as_str())
        .expect("the batch answered for the lost object");
    assert_eq!(
        lost_answer["error"]["code"], 404,
        "an object the source never had cannot be served: {lost_answer}"
    );
}
