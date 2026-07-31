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
    batch_objects(
        base,
        owner,
        repo,
        token,
        operation,
        serde_json::json!([{"oid": oid, "size": size}]),
    )
    .await
}

async fn batch_objects(
    base: &str,
    owner: &str,
    repo: &str,
    token: Option<&str>,
    operation: &str,
    objects: serde_json::Value,
) -> reqwest::Response {
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/lfs/objects/batch"
        ))
        .json(&serde_json::json!({
            "operation": operation,
            "objects": objects,
            "transfers": ["basic"]
        }));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn lfs_download_batch_reports_missing_objects_individually() {
    let (base, _) = spawn_test_app_with_db().await;
    let (owner_token, _) =
        register_full(&base, "lfs_missing_owner", "lfs_missing_owner@example.com").await;
    create_repo(&base, &owner_token, "missing-lfs", true).await;

    let content = b"stored LFS content";
    let stored_oid = hex::encode(Sha256::digest(content));
    let missing_oid = hex::encode(Sha256::digest(b"missing LFS content"));

    let upload = batch(
        &base,
        "lfs_missing_owner",
        "missing-lfs",
        Some(&owner_token),
        "upload",
        &stored_oid,
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

    let missing = batch(
        &base,
        "lfs_missing_owner",
        "missing-lfs",
        Some(&owner_token),
        "download",
        &missing_oid,
        content.len(),
    )
    .await;
    assert_eq!(missing.status(), 200);
    let missing = missing.json::<serde_json::Value>().await.unwrap();
    assert_eq!(missing["objects"][0]["error"]["code"], 404);

    let mixed = batch_objects(
        &base,
        "lfs_missing_owner",
        "missing-lfs",
        Some(&owner_token),
        "download",
        serde_json::json!([
            {"oid": stored_oid, "size": content.len()},
            {"oid": missing_oid, "size": content.len()}
        ]),
    )
    .await;
    assert_eq!(mixed.status(), 200);
    let mixed = mixed.json::<serde_json::Value>().await.unwrap();
    assert!(
        mixed["objects"][0]["actions"]["download"]["href"]
            .as_str()
            .is_some(),
        "stored object must retain its download action: {mixed}"
    );
    assert_eq!(mixed["objects"][1]["error"]["code"], 404);
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
        None,
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

/// Deactivating an account has to reach the capabilities it already handed out.
///
/// A signed LFS URL presents no credential, so the revocation middleware never
/// sees it: the signature is valid, the expiry has not passed, and before the
/// actor was bound into it there was nothing to look the account up by. An
/// upload URL lives six hours, so an offboarded user kept write access to a
/// private repository for the rest of the afternoon.
#[tokio::test]
async fn deactivating_an_account_revokes_its_outstanding_lfs_urls() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "lfs_revoked", "lfs_revoked@example.com").await;
    create_repo(&base, &owner_token, "revoked-lfs", true).await;
    let content = b"content behind a revoked account";
    let oid = hex::encode(Sha256::digest(content));

    let upload_batch = batch(
        &base,
        "lfs_revoked",
        "revoked-lfs",
        Some(&owner_token),
        "upload",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(upload_batch.status(), 200);
    let upload_href = upload_batch.json::<serde_json::Value>().await.unwrap()["objects"][0]
        ["actions"]["upload"]["href"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        upload_href.contains("actor="),
        "an authenticated issue must name the account it was issued to: {upload_href}"
    );

    // Baseline: the URL works while the account stands, so the rejection below
    // is the deactivation and not a broken fixture.
    let before = reqwest::Client::new()
        .put(&upload_href)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(before.status(), 200);

    rg_db::ops::user_ops::update_by_id(&db, owner_id, None, None, None, Some(false))
        .await
        .expect("deactivate user")
        .expect("registered user must exist");

    let after = reqwest::Client::new()
        .put(&upload_href)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        after.status(),
        401,
        "an upload URL issued before the deactivation must stop working"
    );

    // Stripping the actor is the obvious way to ask for the old behaviour back;
    // it has to read as a forgery, not as an anonymous issue.
    let stripped: String = upload_href
        .split('&')
        .filter(|part| !part.starts_with("actor="))
        .collect::<Vec<_>>()
        .join("&");
    let forged = reqwest::Client::new()
        .put(&stripped)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), 403);
}

#[test]
fn lfs_action_signature_rejects_tampering_and_expiry() {
    use rg_core::lfs::service::{
        sign_action_url, verify_action_url, LfsActionKind, LfsActionSignatureError,
    };

    let oid = "b".repeat(64);
    let expires = 2_000_000_000;
    let signature = sign_action_url(b"secret", LfsActionKind::Upload, 7, &oid, expires, Some(42));
    assert_eq!(
        verify_action_url(
            b"secret",
            LfsActionKind::Upload,
            7,
            &oid,
            expires,
            Some(42),
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
            Some(42),
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
            Some(42),
            &signature,
            expires - 1,
        ),
        Err(LfsActionSignatureError::Invalid)
    );
    // Re-pointing the URL at another account is the interesting forgery now
    // that the actor decides whether the URL still works: it has to break the
    // signature rather than move the capability.
    assert_eq!(
        verify_action_url(
            b"secret",
            LfsActionKind::Upload,
            7,
            &oid,
            expires,
            Some(43),
            &signature,
            expires - 1,
        ),
        Err(LfsActionSignatureError::Invalid)
    );
    // And so is dropping `actor=` altogether, which is the cheaper way to ask
    // for the pre-revocation behaviour back.
    assert_eq!(
        verify_action_url(
            b"secret",
            LfsActionKind::Upload,
            7,
            &oid,
            expires,
            None,
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
            Some(42),
            &signature,
            expires,
        ),
        Err(LfsActionSignatureError::Expired)
    );
}

// ── A signed URL is a capability, and a capability has to keep answering ────
//
// The three tests below are one argument in three shapes: a signed LFS action
// URL must stop working the moment the access it was minted from stops
// existing. It used to re-check the signature and the *account* and nothing
// else, so dropping a collaborator left their download URLs good for the rest
// of the hour and their upload URLs for the rest of the six — the account being
// untouched the whole time, which is exactly what the standing check looks at.
//
// Each one carries its baseline in the same run and *before* the revocation: a
// URL that answers 403 proves nothing on its own, since a URL that was never
// valid answers 403 too.

/// Add `username` to `owner/repo` as a writer, and return the collaborator row
/// id the removal route needs.
async fn add_collaborator(
    base: &str,
    owner_token: &str,
    owner: &str,
    repo: &str,
    username: &str,
) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({"username": username, "permission": "write"}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "fixture: adding '{username}' to {owner}/{repo} failed: {}",
        response.status()
    );
    response.json::<serde_json::Value>().await.unwrap()["user_id"]
        .as_i64()
        .expect("collaborator user_id")
}

async fn remove_collaborator(base: &str, owner_token: &str, owner: &str, repo: &str, user_id: i64) {
    let response = reqwest::Client::new()
        .delete(format!(
            "{base}/api/v1/repos/{owner}/{repo}/collaborators/{user_id}"
        ))
        .bearer_auth(owner_token)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "fixture: removing collaborator {user_id} failed: {}",
        response.status()
    );
}

/// Dropping a collaborator has to reach the download URLs they already hold.
#[tokio::test]
async fn a_download_url_stops_working_when_the_collaborator_is_dropped() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "lfsrevowner", "lfsrevowner@example.com").await;
    let (mate_token, _) = register_full(&base, "lfsrevmate", "lfsrevmate@example.com").await;
    create_repo(&base, &owner_token, "revoke-dl", true).await;
    let mate_id = add_collaborator(
        &base,
        &owner_token,
        "lfsrevowner",
        "revoke-dl",
        "lfsrevmate",
    )
    .await;

    // Seed the object as the owner, so the download below has something real to
    // fetch and a 404 cannot be mistaken for a denial.
    let content = b"revocation download content";
    let oid = hex::encode(Sha256::digest(content));
    let upload = batch(
        &base,
        "lfsrevowner",
        "revoke-dl",
        Some(&owner_token),
        "upload",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(upload.status(), 200);
    let upload_href = upload.json::<serde_json::Value>().await.unwrap()["objects"][0]["actions"]
        ["upload"]["href"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        reqwest::Client::new()
            .put(&upload_href)
            .body(content.to_vec())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // The collaborator mints a download URL while they still may.
    let download = batch(
        &base,
        "lfsrevowner",
        "revoke-dl",
        Some(&mate_token),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(download.status(), 200);
    let href = download.json::<serde_json::Value>().await.unwrap()["objects"][0]["actions"]
        ["download"]["href"]
        .as_str()
        .unwrap()
        .to_owned();

    // Baseline, before anything is revoked: this exact URL serves this object.
    let before = reqwest::Client::new().get(&href).send().await.unwrap();
    assert_eq!(
        before.status(),
        200,
        "the URL never worked, so it stopping later would prove nothing"
    );
    assert_eq!(before.bytes().await.unwrap().as_ref(), content);

    remove_collaborator(&base, &owner_token, "lfsrevowner", "revoke-dl", mate_id).await;

    let after = reqwest::Client::new().get(&href).send().await.unwrap();
    assert!(
        matches!(after.status().as_u16(), 401 | 403 | 404),
        "a dropped collaborator's download URL still serves the object ({})",
        after.status()
    );
}

/// The six-hour window is the one that matters: an upload URL outliving the
/// write access it was minted from is a write to a private repository by
/// somebody who no longer has any.
#[tokio::test]
async fn an_upload_url_stops_working_when_write_access_is_dropped() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "lfsupowner", "lfsupowner@example.com").await;
    let (mate_token, _) = register_full(&base, "lfsupmate", "lfsupmate@example.com").await;
    create_repo(&base, &owner_token, "revoke-up", true).await;
    let mate_id =
        add_collaborator(&base, &owner_token, "lfsupowner", "revoke-up", "lfsupmate").await;

    // Two URLs minted in one batch, while the collaborator still has write
    // access: the first is the baseline, the second is the probe. Both are
    // equally valid at this point, which is what makes the pair an argument.
    let baseline_content = b"still a collaborator";
    let probe_content = b"no longer a collaborator";
    let baseline_oid = hex::encode(Sha256::digest(baseline_content));
    let probe_oid = hex::encode(Sha256::digest(probe_content));

    let mut hrefs = Vec::new();
    for (oid, content) in [
        (&baseline_oid, &baseline_content[..]),
        (&probe_oid, &probe_content[..]),
    ] {
        let minted = batch(
            &base,
            "lfsupowner",
            "revoke-up",
            Some(&mate_token),
            "upload",
            oid,
            content.len(),
        )
        .await;
        assert_eq!(minted.status(), 200);
        hrefs.push(
            minted.json::<serde_json::Value>().await.unwrap()["objects"][0]["actions"]["upload"]
                ["href"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }

    let before = reqwest::Client::new()
        .put(&hrefs[0])
        .body(baseline_content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        before.status(),
        200,
        "the minted upload URL never worked, so it failing later would prove nothing"
    );

    remove_collaborator(&base, &owner_token, "lfsupowner", "revoke-up", mate_id).await;

    let after = reqwest::Client::new()
        .put(&hrefs[1])
        .body(probe_content.to_vec())
        .send()
        .await
        .unwrap();
    assert!(
        matches!(after.status().as_u16(), 401 | 403 | 404),
        "a dropped collaborator's upload URL still writes to the repository ({})",
        after.status()
    );
}

/// An anonymous signed URL was waved through on the argument that the read gate
/// would have admitted the same caller anyway. True when it was minted; the
/// repository is allowed to stop being public afterwards.
#[tokio::test]
async fn an_anonymous_download_url_stops_working_when_the_repository_turns_private() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "lfspubowner", "lfspubowner@example.com").await;
    let repo_id = create_repo(&base, &owner_token, "goes-private", false).await;

    let content = b"public while it lasted";
    let oid = hex::encode(Sha256::digest(content));
    let upload = batch(
        &base,
        "lfspubowner",
        "goes-private",
        Some(&owner_token),
        "upload",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(upload.status(), 200);
    let upload_href = upload.json::<serde_json::Value>().await.unwrap()["objects"][0]["actions"]
        ["upload"]["href"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        reqwest::Client::new()
            .put(&upload_href)
            .body(content.to_vec())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // Minted with no credentials at all, so the URL carries no actor.
    let download = batch(
        &base,
        "lfspubowner",
        "goes-private",
        None,
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(download.status(), 200);
    let href = download.json::<serde_json::Value>().await.unwrap()["objects"][0]["actions"]
        ["download"]["href"]
        .as_str()
        .unwrap()
        .to_owned();

    let before = reqwest::Client::new().get(&href).send().await.unwrap();
    assert_eq!(
        before.status(),
        200,
        "the anonymous URL never worked, so it stopping later would prove nothing"
    );

    // No REST route flips visibility, so the fixture does it where the handler
    // would: the row the read gate reads on every request.
    {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};
        let repo = rg_db::entities::repository::Entity::find_by_id(repo_id)
            .one(&db)
            .await
            .unwrap()
            .expect("fixture repository");
        let mut repo: rg_db::entities::repository::ActiveModel = repo.into();
        repo.is_private = Set(true);
        repo.update(&db).await.expect("flip repository to private");
    }

    let after = reqwest::Client::new().get(&href).send().await.unwrap();
    assert!(
        matches!(after.status().as_u16(), 401 | 403 | 404),
        "an anonymous URL minted while the repository was public still serves it after it \
         turned private ({})",
        after.status()
    );
}
