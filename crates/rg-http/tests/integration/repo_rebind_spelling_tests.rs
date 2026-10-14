//! A rename or a transfer moves the storage the repository row names — not
//! the storage the request's spelling of its address would name
//! (card_3f1936e7d4a7).
//!
//! On MySQL's default collation `alice/myrepo` used to find `Alice/MyRepo`.
//! The rebind built every source it moves from the request's spelling —
//! `<owner>/<repo>.git`, the LFS and OCI prefixes, the recovery journal — so a
//! rename addressed in lowercase looked for directories that do not exist, and
//! a transfer addressed that way renamed the repository as a side effect.
//! Since card_3b3d49c60ab1 an address resolves only by its rows' spelling on
//! every backend, so the lowercase request is refused at the door; the rebind
//! still moves what the row names, and the OCI layer docker pushed in
//! lowercase is what proves it. SQLite and PostgreSQL compare names exactly,
//! so only a server database with a case-insensitive collation can show it.

use sha2::{Digest, Sha256};

use crate::common::{register_full, wait_for_listener};

fn oci_digest(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(payload)))
}

async fn send(
    method: reqwest::Method,
    url: String,
    token: &str,
    body: serde_json::Value,
) -> (u16, String) {
    let response = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

/// Store an LFS object the way `git lfs push` does, under the row's spelling.
async fn upload_lfs_object(base: &str, token: &str, owner: &str, repo: &str, payload: &[u8]) {
    let oid = hex::encode(Sha256::digest(payload));
    let (status, batch) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/{owner}/{repo}/lfs/objects/batch"),
        token,
        serde_json::json!({
            "operation": "upload",
            "objects": [{ "oid": oid, "size": payload.len() }],
            "transfers": ["basic"],
        }),
    )
    .await;
    assert_eq!(status, 200, "{batch}");
    let batch: serde_json::Value = serde_json::from_str(&batch).unwrap();
    let href = batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .expect("upload href")
        .to_string();
    let stored = reqwest::Client::new()
        .put(href)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200, "LFS object upload failed");
}

/// Whether the LFS API of `owner/repo` hands out a download for `payload` —
/// that is, whether the object is where the repository now reads it from.
async fn lfs_object_is_served(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    payload: &[u8],
) -> bool {
    let oid = hex::encode(Sha256::digest(payload));
    let (status, batch) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/{owner}/{repo}/lfs/objects/batch"),
        token,
        serde_json::json!({
            "operation": "download",
            "objects": [{ "oid": oid, "size": payload.len() }],
            "transfers": ["basic"],
        }),
    )
    .await;
    assert_eq!(status, 200, "{batch}");
    let batch: serde_json::Value = serde_json::from_str(&batch).unwrap();
    batch["objects"][0]["actions"]["download"]["href"].is_string()
}

/// Push one OCI blob the way docker addresses it: lowercase.
async fn push_oci_blob(base: &str, token: &str, path: &str, payload: &[u8]) {
    let client = reqwest::Client::new();
    let start = client
        .post(format!("{base}/v2/{path}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "{}", start.text().await.unwrap());
    let location = start
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();
    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202);
    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", oci_digest(payload).as_str())])
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(finish, payload.len()).await;
}

async fn objects_under(repo_root: &std::path::Path, segments: [&str; 3]) -> usize {
    let storage = rg_core::blob_storage::instance_blob_storage(repo_root);
    let prefix = rg_core::blob_storage::BlobKey::from_segments(segments).unwrap();
    rg_core::blob_storage::BlobStorage::list(&storage, Some(&prefix))
        .await
        .unwrap()
        .len()
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable MySQL (case-insensitive collation)"]
async fn a_rename_and_a_transfer_move_the_rows_storage_and_refuse_another_spelling() {
    let _shared = crate::common::SHARED_SERVER_DB.lock().await;
    crate::common::install_test_tracing();
    let database_url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    assert!(
        database_url.starts_with("mysql://"),
        "only a case-insensitive collation reaches one repository by two spellings"
    );
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to disposable server database");
    rg_db::run_migrations(&db)
        .await
        .expect("migrate disposable server database");
    let dir = tempfile::tempdir().unwrap();
    let repo_root = dir.path().join("repos");
    let base = crate::common::spawn_test_app_over_db_with(
        db.clone(),
        repo_root.clone(),
        crate::common::StateOverrides::default(),
    )
    .await;
    wait_for_listener(base.trim_start_matches("http://")).await;

    // A server database outlives one run, so the names carry a suffix.
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..10];
    let owner = format!("Reb{suffix}");
    let lower_owner = owner.to_lowercase();
    let org = format!("RebOrg{suffix}");
    let (token, _) = register_full(&base, &owner, &format!("{lower_owner}@example.com")).await;
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        &token,
        serde_json::json!({ "name": "MyRepo", "auto_init": true, "readme": "default" }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/orgs"),
        &token,
        serde_json::json!({ "name": org }),
    )
    .await;
    assert!(status == 201 || status == 200, "{status}: {body}");

    let lfs_payload = b"one LFS object that has to follow the rename";
    upload_lfs_object(&base, &token, &owner, "MyRepo", lfs_payload).await;
    let oci_payload = b"one OCI layer that has to follow the rename";
    push_oci_blob(&base, &token, &format!("{lower_owner}/myrepo"), oci_payload).await;
    assert_eq!(
        objects_under(&repo_root, ["oci", &owner, "MyRepo"]).await,
        1
    );

    // Addressed the way a lowercasing client spells it, the rename finds no
    // repository and moves nothing.
    let (status, body) = send(
        reqwest::Method::PATCH,
        format!("{base}/api/v1/repos/{lower_owner}/myrepo"),
        &token,
        serde_json::json!({ "name": "Renamed" }),
    )
    .await;
    assert_eq!(status, 404, "another spelling of the address: {body}");
    assert!(repo_root.join(format!("{owner}/MyRepo.git")).is_dir());

    // Addressed by the row's spelling, it moves what the row names.
    let (status, body) = send(
        reqwest::Method::PATCH,
        format!("{base}/api/v1/repos/{owner}/MyRepo"),
        &token,
        serde_json::json!({ "name": "Renamed" }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(repo_root.join(format!("{owner}/Renamed.git")).is_dir());
    assert!(!repo_root.join(format!("{owner}/MyRepo.git")).exists());
    assert_eq!(
        objects_under(&repo_root, ["oci", &owner, "Renamed"]).await,
        1,
        "the OCI layer did not follow the rename"
    );
    assert_eq!(
        objects_under(&repo_root, ["oci", &owner, "MyRepo"]).await,
        0
    );
    assert!(
        lfs_object_is_served(&base, &token, &owner, "Renamed", lfs_payload).await,
        "the LFS object did not follow the rename"
    );

    // The transfer: in lowercase it is refused, and it cannot rename the
    // repository as a side effect; by the row's spelling its storage moves.
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/{lower_owner}/renamed/transfer"),
        &token,
        serde_json::json!({ "new_owner": org }),
    )
    .await;
    assert_eq!(status, 404, "another spelling of the address: {body}");
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/{owner}/Renamed/transfer"),
        &token,
        serde_json::json!({ "new_owner": org }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let moved: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        moved["name"], "Renamed",
        "a transfer must not rename the repository to the request's spelling"
    );
    assert!(repo_root.join(format!("{org}/Renamed.git")).is_dir());
    assert!(!repo_root.join(format!("{owner}/Renamed.git")).exists());
    assert_eq!(objects_under(&repo_root, ["oci", &org, "Renamed"]).await, 1);
    assert_eq!(
        objects_under(&repo_root, ["oci", &owner, "Renamed"]).await,
        0
    );
    assert!(lfs_object_is_served(&base, &token, &org, "Renamed", lfs_payload).await);

    // Nothing was ever written under the request's spelling.
    assert!(!repo_root.join(&lower_owner).exists());
}
