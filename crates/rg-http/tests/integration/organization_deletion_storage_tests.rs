//! card_f823672b3f60: deleting an organization is a repository lifecycle, not
//! one `DELETE FROM organizations`.
//!
//! Organization repositories have no foreign key to their organization. The
//! old endpoint therefore freed the organization name while its active rows,
//! Git trees, blob prefixes and OCI repository all remained live but became
//! unreachable. These tests guard the two consequences that matter:
//!
//! * success retires every repository before the organization row and a reused
//!   name starts with an empty namespace;
//! * a repository storage failure is a failed organization deletion, leaving
//!   both the organization and that repository retryable.

use rg_core::blob_storage::BlobKey;
use sha2::Digest;

use crate::common::{
    fault::spawn_test_app_for_fault_sweep, register_full, spawn_test_app_with_state,
};

async fn create_org(base: &str, token: &str, name: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .expect("create organization");
    assert_eq!(
        response.status(),
        201,
        "create organization failed: {}",
        response.text().await.unwrap_or_default()
    );
    response
        .json::<serde_json::Value>()
        .await
        .expect("read organization response")["id"]
        .as_i64()
        .expect("organization response has an id")
}

async fn create_org_repo(base: &str, token: &str, org: &str, name: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "org": org }))
        .send()
        .await
        .expect("create organization repository");
    assert_eq!(
        response.status(),
        201,
        "create organization repository failed: {}",
        response.text().await.unwrap_or_default()
    );
    response
        .json::<serde_json::Value>()
        .await
        .expect("read repository response")["id"]
        .as_i64()
        .expect("repository response has an id")
}

fn representative_blob_keys(org: &str, repo: &str) -> Vec<BlobKey> {
    vec![
        BlobKey::from_segments([
            "packages",
            org,
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
            org,
            repo,
            "cc",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc.zst",
        ])
        .unwrap(),
    ]
}

fn live_blob_prefixes(org: &str, repo: &str) -> Vec<BlobKey> {
    vec![
        BlobKey::from_segments(["packages", org, repo]).unwrap(),
        BlobKey::from_segments(["lfs", org, repo]).unwrap(),
    ]
}

/// The full routed contract: Git, package/LFS and an OCI layer leave the live
/// namespace before the organization name is freed. Recreating both names must
/// not expose any byte from the old repository.
#[tokio::test]
async fn deleting_an_organization_retires_its_repositories_before_reusing_the_name() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "org-delete-owner", "org-delete@example.com").await;
    let old_org_id = create_org(&base, &token, "recycled-org").await;
    let old_repo_id = create_org_repo(&base, &token, "recycled-org", "payloads").await;

    let keys = representative_blob_keys("recycled-org", "payloads");
    for (index, key) in keys.iter().enumerate() {
        state
            .blob_storage
            .put(key, format!("payload-{index}").as_bytes())
            .await
            .expect("seed organization repository blob");
    }

    let layer = b"organization-owned OCI layer";
    let layer_digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(layer)));
    state
        .oci_storage
        .store_blob("recycled-org", "payloads", &layer_digest, layer)
        .await
        .expect("seed organization OCI layer");
    assert!(state
        .oci_storage
        .blob_exists("recycled-org", "payloads", &layer_digest)
        .await
        .expect("read seeded OCI layer"));

    let bare = state.repo_root.join("recycled-org/payloads.git");
    assert!(
        bare.exists(),
        "repository creation did not seed Git storage"
    );

    let response = reqwest::Client::new()
        .delete(format!("{base}/api/v1/orgs/recycled-org"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete organization");
    assert_eq!(
        response.status(),
        200,
        "organization deletion failed: {}",
        response.text().await.unwrap_or_default()
    );

    assert!(
        !bare.exists(),
        "organization deletion left the Git tree live"
    );
    assert!(
        rg_db::ops::org_ops::get_org(&db, old_org_id)
            .await
            .expect("read deleted organization")
            .is_none(),
        "organization row survived a successful deletion"
    );
    assert!(
        rg_db::ops::repo_ops::find_by_id(&db, old_repo_id)
            .await
            .expect("read deleted organization repository")
            .is_none(),
        "organization deletion left an active repository row"
    );
    for prefix in live_blob_prefixes("recycled-org", "payloads") {
        assert!(
            state
                .blob_storage
                .list(Some(&prefix))
                .await
                .expect("inventory retired repository prefix")
                .is_empty(),
            "organization deletion left live objects below {prefix}"
        );
    }
    assert!(
        !state
            .oci_storage
            .blob_exists("recycled-org", "payloads", &layer_digest)
            .await
            .expect("read retired OCI layer"),
        "organization deletion left its OCI layer live"
    );

    let new_org_id = create_org(&base, &token, "recycled-org").await;
    assert_ne!(
        new_org_id, old_org_id,
        "organization recreation reused the row"
    );
    let new_repo_id = create_org_repo(&base, &token, "recycled-org", "payloads").await;
    assert_ne!(
        new_repo_id, old_repo_id,
        "repository recreation reused the row"
    );
    for prefix in live_blob_prefixes("recycled-org", "payloads") {
        assert!(
            state
                .blob_storage
                .list(Some(&prefix))
                .await
                .expect("inventory recreated repository prefix")
                .is_empty(),
            "recreated organization inherited objects below {prefix}"
        );
    }
    assert!(
        !state
            .oci_storage
            .blob_exists("recycled-org", "payloads", &layer_digest)
            .await
            .expect("read recreated OCI namespace"),
        "recreated organization inherited the old OCI layer"
    );
}

/// A repository prefix move is the first cross-store prepare step. If it
/// fails, the endpoint must return `5xx`, restore the Git tree and keep both
/// metadata rows so the same request can be retried.
#[tokio::test]
async fn repository_storage_failure_does_not_delete_the_organization() {
    let app = spawn_test_app_for_fault_sweep().await;
    let (token, _) = register_full(
        &app.base,
        "org-delete-fault-owner",
        "org-delete-fault@example.com",
    )
    .await;
    let org_id = create_org(&app.base, &token, "fault-org").await;
    let repo_id = create_org_repo(&app.base, &token, "fault-org", "survivor").await;
    let package = representative_blob_keys("fault-org", "survivor")
        .into_iter()
        .next()
        .unwrap();
    let package_path = package
        .as_str()
        .split('/')
        .fold(app.repo_root.clone(), |path, segment| path.join(segment));
    std::fs::create_dir_all(package_path.parent().unwrap()).unwrap();
    std::fs::write(&package_path, b"keep this organization blob").unwrap();
    let bare = app.repo_root.join("fault-org/survivor.git");

    app.blob_faults.fail_delete();
    let response = reqwest::Client::new()
        .delete(format!("{}/api/v1/orgs/fault-org", app.base))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete organization with failed repository storage");
    assert_eq!(
        response.status(),
        500,
        "repository storage failure was reported as a completed organization deletion"
    );

    app.blob_faults.heal();
    assert!(
        bare.exists(),
        "failed deletion did not restore the Git tree"
    );
    assert_eq!(
        std::fs::read(&package_path).unwrap(),
        b"keep this organization blob"
    );
    assert!(
        rg_db::ops::repo_ops::find_by_id(&app.db, repo_id)
            .await
            .expect("read repository after failed organization deletion")
            .is_some(),
        "failed organization deletion removed the repository row"
    );
    assert!(
        rg_db::ops::org_ops::get_org(&app.db, org_id)
            .await
            .expect("read organization after failed deletion")
            .is_some(),
        "failed repository deletion removed the organization row"
    );
}
