//! Regression coverage for card_44bf421f18f2: a fork must be able to serve the
//! LFS objects of the repository it was forked from.
//!
//! `fork_repo` used to run `git clone --bare` and nothing else. The pointer
//! files came along with the history, but LFS keeps its content outside Git:
//! an `lfs_objects` row per `(repo_id, oid)` that the batch API looks up, and
//! bytes under a key built from the repository's `<owner>/<name>`. Neither
//! was created for the fork, so `git clone <fork>` + `git lfs pull` got a
//! per-object `404 object not found` for every file the source tracks.
//!
//! The assertions go further than the batch answer. A fork that merely
//! *pointed* at the source's storage would pass the first download and break
//! the moment the source went away, so the source is deleted halfway through
//! and every object has to come back from the fork alone. The legacy
//! `<owner>.lfs/<repo>` layout is seeded too, because installations that
//! predate blob storage still hold objects there and forking them is the same
//! promise.

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use sha2::{Digest, Sha256};

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

const OWNER: &str = "lfs_fork_source";
const FORKER: &str = "lfs_fork_forker";
const REPO: &str = "lfs-assets";

fn oid_of(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

async fn create_public_repo(base: &str, token: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": REPO,
            "is_private": false,
            "auto_init": true,
            "readme": "default",
        }))
        .send()
        .await
        .expect("create source repository");
    assert_eq!(response.status(), 201, "seeding the source failed");
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .expect("source repository id")
}

/// Store an object the way `git lfs push` does: announce it through the batch
/// API, then `PUT` the bytes to the signed href it hands back.
async fn upload_object(base: &str, token: &str, payload: &[u8]) -> String {
    let oid = oid_of(payload);
    let client = reqwest::Client::new();
    let batch = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid, "size": payload.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch.status(), 200, "LFS upload batch failed");
    let batch = batch.json::<serde_json::Value>().await.unwrap();
    let href = batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .expect("upload href")
        .to_string();
    let stored = client
        .put(href)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200, "LFS object upload failed");
    oid
}

/// Ask the fork for `objects` the way `git lfs pull` does, and return the
/// per-object answers keyed by oid.
async fn batch_download(
    base: &str,
    token: &str,
    owner: &str,
    objects: &[(&str, usize)],
) -> std::collections::HashMap<String, serde_json::Value> {
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{REPO}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "download",
            "objects": objects
                .iter()
                .map(|(oid, size)| serde_json::json!({"oid": oid, "size": size}))
                .collect::<Vec<_>>(),
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "LFS download batch failed");
    let body = response.json::<serde_json::Value>().await.unwrap();
    body["objects"]
        .as_array()
        .expect("batch objects")
        .iter()
        .map(|object| (object["oid"].as_str().unwrap().to_string(), object.clone()))
        .collect()
}

async fn fetch(href: &str) -> Vec<u8> {
    let response = reqwest::get(href).await.unwrap();
    assert_eq!(response.status(), 200, "download href {href} failed");
    response.bytes().await.unwrap().to_vec()
}

#[tokio::test]
async fn a_fork_serves_every_lfs_object_of_its_source_even_after_the_source_is_gone() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner_token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    let (forker_token, forker_id) =
        register_full(&base, FORKER, &format!("{FORKER}@example.com")).await;
    let source_id = create_public_repo(&base, &owner_token).await;

    // Two objects through the ordinary upload path (blob storage, compressed).
    let uploaded: Vec<Vec<u8>> = (0..2)
        .map(|n| format!("lfs payload that must follow the fork {n}").into_bytes())
        .collect();
    let mut expected: Vec<(String, Vec<u8>)> = Vec::new();
    for payload in &uploaded {
        let oid = upload_object(&base, &owner_token, payload).await;
        expected.push((oid, payload.clone()));
    }

    // One object in the pre-blob-storage layout: raw bytes under
    // `<owner>.lfs/<repo>/<shard>/<oid>` and a row that says it is uploaded.
    let legacy_payload = b"legacy LFS object from before blob storage".to_vec();
    let legacy_oid = oid_of(&legacy_payload);
    let legacy_path = repo_root
        .join(format!("{OWNER}.lfs"))
        .join(REPO)
        .join(&legacy_oid[..2])
        .join(&legacy_oid);
    std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
    std::fs::write(&legacy_path, &legacy_payload).unwrap();
    rg_db::entities::lfs_object::ActiveModel {
        repo_id: Set(source_id),
        oid: Set(legacy_oid.clone()),
        size: Set(legacy_payload.len() as i64),
        uploaded: Set(true),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("seed legacy LFS row");
    expected.push((legacy_oid, legacy_payload));

    // An object the source only announced and never received. The fork must
    // not claim to have it either.
    let pending_payload = b"announced but never uploaded".to_vec();
    let pending_oid = oid_of(&pending_payload);
    rg_db::entities::lfs_object::ActiveModel {
        repo_id: Set(source_id),
        oid: Set(pending_oid.clone()),
        size: Set(pending_payload.len() as i64),
        uploaded: Set(false),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("seed pending LFS row");

    let fork = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/fork"))
        .bearer_auth(&forker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(fork.status(), 201, "fork failed");
    let fork_id = fork.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .expect("fork id");

    let mut asked: Vec<(&str, usize)> = expected
        .iter()
        .map(|(oid, payload)| (oid.as_str(), payload.len()))
        .collect();
    asked.push((pending_oid.as_str(), pending_payload.len()));

    // 1. The batch answer: a download action per stored object, not a 404.
    let answers = batch_download(&base, &forker_token, FORKER, &asked).await;
    for (oid, _) in &expected {
        let answer = &answers[oid];
        assert!(
            answer["error"].is_null(),
            "the fork answered an error for source object {oid}: {answer}"
        );
        assert!(
            answer["actions"]["download"]["href"].is_string(),
            "the fork offered no download for source object {oid}: {answer}"
        );
    }
    assert_eq!(
        answers[&pending_oid]["error"]["code"], 404,
        "an object the source never received must not appear in the fork: {}",
        answers[&pending_oid]
    );

    // 2. The source goes away. Whatever the fork serves now is its own.
    let deleted = reqwest::Client::new()
        .delete(format!("{base}/api/v1/repos/{OWNER}/{REPO}"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        deleted.status(),
        200,
        "deleting the source failed: {}",
        deleted.text().await.unwrap_or_default()
    );

    // 3. Every object, byte for byte, from the fork alone.
    let answers = batch_download(&base, &forker_token, FORKER, &asked).await;
    for (oid, payload) in &expected {
        let href = answers[oid]["actions"]["download"]["href"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "after the source was deleted the fork offered no download for {oid}: {}",
                    answers[oid]
                )
            });
        assert_eq!(
            &fetch(href).await,
            payload,
            "the fork returned different bytes for {oid}"
        );
    }

    // 4. The rows belong to the fork and describe stored objects only.
    let fork_rows = rg_db::entities::lfs_object::Entity::find()
        .filter(rg_db::entities::lfs_object::Column::RepoId.eq(fork_id))
        .filter(rg_db::entities::lfs_object::Column::Uploaded.eq(true))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(
        fork_rows,
        expected.len() as u64,
        "the fork (owner {forker_id}) must hold exactly one uploaded row per stored source object"
    );
}

/// Two payloads whose oids land in different shard directories, so breaking
/// one shard leaves the other object readable.
fn two_payloads_in_distinct_shards() -> (Vec<u8>, Vec<u8>) {
    let first = b"lfs object the fork copies before the failure".to_vec();
    let first_shard = oid_of(&first)[..2].to_string();
    for n in 0..10_000u32 {
        let candidate = format!("lfs object the fork cannot read {n}").into_bytes();
        if oid_of(&candidate)[..2] != first_shard {
            return (first, candidate);
        }
    }
    panic!("no second LFS shard among the candidate payloads");
}

#[tokio::test]
async fn a_fork_that_cannot_copy_its_source_lfs_objects_leaves_nothing_behind() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner_token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    let (forker_token, _) = register_full(&base, FORKER, &format!("{FORKER}@example.com")).await;
    create_public_repo(&base, &owner_token).await;

    // The first object copies; the second sits behind a shard the server
    // cannot read, so the copy fails after it has already published one key.
    let (copied, unreadable) = two_payloads_in_distinct_shards();
    let copied_oid = upload_object(&base, &owner_token, &copied).await;
    let unreadable_oid = upload_object(&base, &owner_token, &unreadable).await;
    let unreadable_shard = repo_root
        .join("lfs")
        .join(OWNER)
        .join(REPO)
        .join(&unreadable_oid[..2]);
    std::fs::remove_dir_all(&unreadable_shard).unwrap();
    std::fs::write(&unreadable_shard, b"not a directory").unwrap();

    let fork = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/fork"))
        .bearer_auth(&forker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        fork.status(),
        500,
        "a storage failure while copying LFS objects is the server's, and the fork must not stand"
    );

    let lookup = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{FORKER}/{REPO}"))
        .bearer_auth(&forker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(lookup.status(), 404, "the failed fork's row survived");
    assert!(
        !repo_root.join(format!("{FORKER}/{REPO}.git")).exists(),
        "the failed fork's bare repository survived"
    );
    let copied_key = repo_root
        .join("lfs")
        .join(FORKER)
        .join(REPO)
        .join(&copied_oid[..2])
        .join(format!("{copied_oid}.zst"));
    assert!(
        !copied_key.exists(),
        "the object copied before the failure was left under the fork's key"
    );
    let rows = rg_db::entities::lfs_object::Entity::find()
        .count(&db)
        .await
        .unwrap();
    assert_eq!(rows, 2, "the failed fork left LFS rows behind");

    // Nothing of the attempt blocks the next one.
    std::fs::remove_file(&unreadable_shard).unwrap();
    let retry = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/fork"))
        .bearer_auth(&forker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        retry.status(),
        201,
        "a retry after the failure must fork: {}",
        retry.text().await.unwrap_or_default()
    );
}
