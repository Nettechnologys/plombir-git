use crate::common::{register_full, spawn_test_app_with_db};
use sha2::{Digest, Sha256};

async fn create_repo(base: &str, token: &str, name: &str, is_private: bool) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": is_private}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn batch(
    base: &str,
    owner: &str,
    repo: &str,
    token: Option<&str>,
    operation: &str,
    oid: &str,
    size: usize,
) -> reqwest::Response {
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/lfs/objects/batch"
        ))
        .json(&serde_json::json!({
            "operation": operation,
            "objects": [{"oid": oid, "size": size}],
            "transfers": ["basic"]
        }));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn signed_lfs_urls_are_ttl_and_action_bound() {
    let (base, _) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "lfs_owner", "lfs_owner@example.com").await;
    let repo_id = create_repo(&base, &owner_token, "signed-lfs", true).await;
    let content = b"signed LFS content";
    let oid = hex::encode(Sha256::digest(content));

    let upload_batch = batch(
        &base,
        "lfs_owner",
        "signed-lfs",
        Some(&owner_token),
        "upload",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(upload_batch.status(), 200);
    let upload_batch = upload_batch.json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        upload_batch["objects"][0]["actions"]["upload"]["expires_in"],
        rg_core::lfs::service::UPLOAD_URL_TTL_SECONDS
    );
    let upload_href = upload_batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap();
    assert!(upload_href.contains("expires="));
    assert!(upload_href.contains("signature="));

    // An upload URL cannot be replayed as a download URL.
    let wrong_action = reqwest::Client::new()
        .get(upload_href)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_action.status(), 403);

    // The signed action URL works without forwarding the Batch bearer token.
    let uploaded = reqwest::Client::new()
        .put(upload_href)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 200);

    let download_batch = batch(
        &base,
        "lfs_owner",
        "signed-lfs",
        Some(&owner_token),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(download_batch.status(), 200);
    let download_batch = download_batch.json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        download_batch["objects"][0]["actions"]["download"]["expires_in"],
        rg_core::lfs::service::DOWNLOAD_URL_TTL_SECONDS
    );
    let download_href = download_batch["objects"][0]["actions"]["download"]["href"]
        .as_str()
        .unwrap();
    let downloaded = reqwest::Client::new()
        .get(download_href)
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), 200);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), content);

    let expires = chrono::Utc::now().timestamp() - 1;
    let signature = rg_core::lfs::service::sign_action_url(
        b"test-secret-key",
        rg_core::lfs::service::LfsActionKind::Download,
        repo_id,
        &oid,
        expires,
    );
    let expired = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/lfs_owner/signed-lfs/lfs/objects/{oid}?expires={expires}&signature={signature}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 410);
}

#[tokio::test]
async fn lfs_upload_batch_always_requires_write_access() {
    let (base, _) = spawn_test_app_with_db().await;
    let (owner_token, _) =
        register_full(&base, "lfs_write_owner", "lfs_write_owner@example.com").await;
    let (outsider_token, _) = register_full(
        &base,
        "lfs_write_outsider",
        "lfs_write_outsider@example.com",
    )
    .await;
    create_repo(&base, &owner_token, "private-lfs", true).await;
    create_repo(&base, &owner_token, "public-lfs", false).await;
    let oid = "a".repeat(64);

    let outsider = batch(
        &base,
        "lfs_write_owner",
        "private-lfs",
        Some(&outsider_token),
        "upload",
        &oid,
        1,
    )
    .await;
    assert_eq!(outsider.status(), 403);

    let anonymous = batch(
        &base,
        "lfs_write_owner",
        "public-lfs",
        None,
        "upload",
        &oid,
        1,
    )
    .await;
    assert_eq!(anonymous.status(), 401);
}

/// LFS downloads answer with the repository's own read gate.
///
/// The three outcomes used to be produced by two different mechanisms: the
/// anonymous `401` came out of the LFS credential helper (which simply demanded
/// a bearer token) and only the outsider's `403` came from a permission check —
/// and on a public repository the check was skipped entirely. They agreed by
/// coincidence, not by construction, so nothing kept them agreeing.
#[tokio::test]
async fn lfs_download_batch_answers_with_the_repository_read_gate() {
    let (base, _) = spawn_test_app_with_db().await;
    let (owner_token, _) =
        register_full(&base, "lfs_read_owner", "lfs_read_owner@example.com").await;
    let (outsider_token, _) =
        register_full(&base, "lfs_read_outsider", "lfs_read_outsider@example.com").await;
    create_repo(&base, &owner_token, "private-read-lfs", true).await;
    create_repo(&base, &owner_token, "public-read-lfs", false).await;
    let content = b"public LFS content";
    let oid = hex::encode(Sha256::digest(content));

    // Anonymous on a private repository: a token would help → 401.
    let anonymous = batch(
        &base,
        "lfs_read_owner",
        "private-read-lfs",
        None,
        "download",
        &oid,
        1,
    )
    .await;
    assert_eq!(anonymous.status(), 401);

    // Authenticated outsider on the same repository: a token does not help → 403.
    let outsider = batch(
        &base,
        "lfs_read_owner",
        "private-read-lfs",
        Some(&outsider_token),
        "download",
        &oid,
        1,
    )
    .await;
    assert_eq!(outsider.status(), 403);

    // A public repository stays anonymously readable — with a real object
    // behind the request, so the 200 proves the gate let it through rather than
    // some earlier failure short-circuiting the response.
    let upload = batch(
        &base,
        "lfs_read_owner",
        "public-read-lfs",
        Some(&owner_token),
        "upload",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(upload.status(), 200);
    let upload = upload.json::<serde_json::Value>().await.unwrap();
    let upload_href = upload["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap();
    let stored = reqwest::Client::new()
        .put(upload_href)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200);

    let public = batch(
        &base,
        "lfs_read_owner",
        "public-read-lfs",
        None,
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(public.status(), 200);
    let public = public.json::<serde_json::Value>().await.unwrap();
    assert!(
        public["objects"][0]["actions"]["download"]["href"]
            .as_str()
            .is_some(),
        "anonymous download batch on a public repo must hand out a download action: {public}"
    );
}

#[test]
fn lfs_action_signature_rejects_tampering_and_expiry() {
    use rg_core::lfs::service::{
        sign_action_url, verify_action_url, LfsActionKind, LfsActionSignatureError,
    };

    let oid = "b".repeat(64);
    let expires = 2_000_000_000;
    let signature = sign_action_url(b"secret", LfsActionKind::Upload, 7, &oid, expires);
    assert_eq!(
        verify_action_url(
            b"secret",
            LfsActionKind::Upload,
            7,
            &oid,
            expires,
            &signature,
            expires - 1,
        ),
        Ok(())
    );
    assert_eq!(
        verify_action_url(
            b"secret",
            LfsActionKind::Download,
            7,
            &oid,
            expires,
            &signature,
            expires - 1,
        ),
        Err(LfsActionSignatureError::Invalid)
    );
    assert_eq!(
        verify_action_url(
            b"secret",
            LfsActionKind::Upload,
            8,
            &oid,
            expires,
            &signature,
            expires - 1,
        ),
        Err(LfsActionSignatureError::Invalid)
    );
    assert_eq!(
        verify_action_url(
            b"secret",
            LfsActionKind::Upload,
            7,
            &oid,
            expires,
            &signature,
            expires,
        ),
        Err(LfsActionSignatureError::Expired)
    );
}
