//! One repository, one OCI storage namespace — on a database that hands one
//! repository to several spellings of its address (card_7e82c1b1f642).
//!
//! On MySQL's default collation `/v2/alice/myrepo` reaches `Alice/MyRepo`, and
//! `docker` only ever sends lowercase. Storage keys used to be built from the
//! request's spelling, so the layers of `Alice/MyRepo` sat under
//! `oci/alice/myrepo/`, a pull under the row's own spelling answered `404`, and
//! deleting the repository — which walks the row's spelling — left every layer
//! behind. SQLite and PostgreSQL compare names exactly, so only a server
//! database with a case-insensitive collation can show it.

use sha2::{Digest, Sha256};

use crate::common::{create_repo, register_full, wait_for_listener};

fn sha256(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(payload)))
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable MySQL (case-insensitive collation)"]
async fn a_lowercase_push_lands_in_the_rows_namespace_and_leaves_with_it() {
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

    // A server database outlives one run, so the name carries a suffix.
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..10];
    let owner = format!("Oci{suffix}");
    let lower_owner = owner.to_lowercase();
    let (token, _) = register_full(&base, &owner, &format!("{lower_owner}@example.com")).await;
    create_repo(&base, &token, "MyRepo").await;
    let client = reqwest::Client::new();

    // Pushed the way docker addresses it: lowercase.
    let payload = b"one layer, one namespace";
    let digest = sha256(payload);
    let start = client
        .post(format!("{base}/v2/{lower_owner}/myrepo/blobs/uploads/"))
        .bearer_auth(&token)
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
    assert!(
        location.starts_with(&format!("/v2/{lower_owner}/myrepo/")),
        "the Location keeps the request's spelling — the scoped token was issued for it: \
         {location}"
    );
    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202);
    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(finish, payload.len()).await;

    let repository = rg_core::repo::service::find_repo_by_owner_name(&db, &owner, "MyRepo")
        .await
        .unwrap()
        .expect("the repository row");
    let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&db, repository.id)
        .await
        .unwrap()
        .expect("the OCI repository row");
    let blob = rg_db::ops::oci_ops::find_blob(&db, oci_repo.id, &digest)
        .await
        .unwrap()
        .expect("the blob row");
    assert!(
        blob.storage_path
            .starts_with(&format!("oci/{owner}/MyRepo/")),
        "the key follows the row's spelling, not the request's: {}",
        blob.storage_path
    );

    for spelling in [format!("{owner}/MyRepo"), format!("{lower_owner}/myrepo")] {
        for method in [reqwest::Method::HEAD, reqwest::Method::GET] {
            let response = client
                .request(
                    method.clone(),
                    format!("{base}/v2/{spelling}/blobs/{digest}"),
                )
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200, "{method} under {spelling}");
        }
    }

    let deleted = client
        .delete(format!("{base}/api/v1/repos/{owner}/MyRepo"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(
        deleted.status().is_success(),
        "{}",
        deleted.text().await.unwrap()
    );
    let storage = rg_core::blob_storage::instance_blob_storage(&repo_root);
    for (owner_segment, repo_segment) in
        [(owner.as_str(), "MyRepo"), (lower_owner.as_str(), "myrepo")]
    {
        let prefix =
            rg_core::blob_storage::BlobKey::from_segments(["oci", owner_segment, repo_segment])
                .unwrap();
        let left = rg_core::blob_storage::BlobStorage::list(&storage, Some(&prefix))
            .await
            .unwrap();
        assert!(
            left.is_empty(),
            "deleting the repository left {left:?} under {owner_segment}/{repo_segment}"
        );
    }
}
