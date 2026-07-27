use crate::common::{register_full, spawn_test_app_with_db, spawn_test_app_with_oci_root};
use base64::Engine as _;

async fn create_repo(base: &str, token: &str, name: &str, is_private: bool) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "is_private": is_private
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create repo failed");
}

fn basic_auth(username: &str, password: &str) -> String {
    let raw = format!("{}:{}", username, password);
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

async fn request_oci_token(
    base: &str,
    scope: &str,
    auth_header: Option<String>,
) -> rg_core::auth::oci_token::OciTokenClaims {
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("{}/v2/auth/token", base))
        .query(&[("service", "forgekeep-registry"), ("scope", scope)]);
    if let Some(auth) = auth_header {
        req = req.header(reqwest::header::AUTHORIZATION, auth);
    }

    let resp = req.send().await.unwrap();
    assert_eq!(resp.status(), 200, "token request failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    let token = body["token"].as_str().unwrap();
    rg_core::auth::oci_token::validate_oci_token(token, "test-secret-key").expect("valid OCI token")
}

#[tokio::test]
async fn private_oci_tags_require_read_access() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) = register_full(&base, "oci_owner", "oci_owner@example.com").await;
    let (other_token, _other_id) = register_full(&base, "oci_other", "oci_other@example.com").await;
    create_repo(&base, &owner_token, "private-oci", true).await;

    let anon_resp = client
        .get(format!("{}/v2/oci_owner/private-oci/tags/list", base))
        .send()
        .await
        .unwrap();
    assert_eq!(anon_resp.status(), 401);

    let other_resp = client
        .get(format!("{}/v2/oci_owner/private-oci/tags/list", base))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(other_resp.status(), 401);

    let owner_resp = client
        .get(format!("{}/v2/oci_owner/private-oci/tags/list", base))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_resp.status(), 404);
}

#[tokio::test]
async fn oci_token_endpoint_grants_only_authorized_scopes() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner_token, _owner_id) =
        register_full(&base, "oci_token_owner", "oci_token_owner@example.com").await;
    let (_other_token, _other_id) =
        register_full(&base, "oci_token_other", "oci_token_other@example.com").await;
    create_repo(&base, &owner_token, "public-image", false).await;
    create_repo(&base, &owner_token, "private-image", true).await;

    let public_pull =
        request_oci_token(&base, "repository:oci_token_owner/public-image:pull", None).await;
    assert_eq!(
        public_pull.scope.as_deref(),
        Some("repository:oci_token_owner/public-image:pull")
    );

    let private_anon =
        request_oci_token(&base, "repository:oci_token_owner/private-image:pull", None).await;
    assert!(private_anon.scope.is_none());

    let owner_push = request_oci_token(
        &base,
        "repository:oci_token_owner/private-image:pull,push",
        Some(basic_auth("oci_token_owner", "Qz7$wRtm")),
    )
    .await;
    assert_eq!(
        owner_push.scope.as_deref(),
        Some("repository:oci_token_owner/private-image:pull,push")
    );

    let other_push = request_oci_token(
        &base,
        "repository:oci_token_owner/private-image:pull,push",
        Some(basic_auth("oci_token_other", "Qz7$wRtm")),
    )
    .await;
    assert!(other_push.scope.is_none());
}

/// The spec's version check is `GET /v2/`, trailing slash included.
///
/// Endpoint end-1 of the OCI distribution spec, and the very first request any
/// docker/podman/containerd client sends: it is how a client discovers that the
/// host speaks the registry protocol and where to get a token. Under `nest` an
/// inner `"/"` route answers the prefix *without* the slash, so the spelling the
/// clients actually send fell through to the SPA fallback and the whole registry
/// looked absent. Both spellings have to answer.
#[tokio::test]
async fn the_registry_answers_the_spec_version_check_path() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    for path in ["/v2/", "/v2"] {
        let resp = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(
            resp.status(),
            401,
            "GET {path} must be the registry's version check, not a fallback"
        );
        assert_eq!(
            resp.headers()
                .get("Docker-Distribution-API-Version")
                .and_then(|v| v.to_str().ok()),
            Some("registry/2.0"),
            "GET {path} must identify itself as a registry"
        );
        assert!(
            resp.headers()
                .contains_key(reqwest::header::WWW_AUTHENTICATE),
            "GET {path} must carry the auth challenge that starts the token flow"
        );
    }
}

/// The realm in the challenge has to be a path this registry actually serves.
///
/// A client does not guess the token endpoint: it reads `realm=` out of the
/// `WWW-Authenticate` header of the 401 and requests its token there. Pointing
/// it at a path we do not route means `docker login` and every pull of a private
/// image fail at the first hop, with the registry answering the question
/// correctly and the client never seeing it.
#[tokio::test]
async fn the_advertised_token_realm_is_a_path_the_registry_serves() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "oci_realm_owner", "oci_realm_owner@example.com").await;
    create_repo(&base, &owner_token, "realm-image", false).await;

    let challenge = client
        .get(format!("{base}/v2/"))
        .send()
        .await
        .unwrap()
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .expect("the version check must issue a challenge")
        .to_string();

    let realm = challenge
        .split_once("realm=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(realm, _)| realm.to_string())
        .expect("the challenge must name a realm");

    let resp = client
        .get(&realm)
        .query(&[
            ("service", "forgekeep-registry"),
            ("scope", "repository:oci_realm_owner/realm-image:pull"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "the advertised realm {realm} is not a route this registry serves"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["token"].as_str().is_some_and(|t| !t.is_empty()),
        "the realm must issue a token: {body}"
    );
}

/// `201 Created` on a blob push has to mean the blob is retrievable.
///
/// The registry writes the bytes to blob storage and the locating row to the
/// database as two separate steps. When the second one was discarded, the push
/// still answered `201` with a `Docker-Content-Digest` — and the very next
/// `HEAD .../blobs/<digest>` returned 404. A client has no reason to retry
/// something it was told succeeded, so the image stayed broken. This walks the
/// real push sequence and then reads the blob back.
#[tokio::test]
async fn a_created_blob_is_retrievable_right_after_the_push() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) =
        register_full(&base, "oci_push_owner", "oci_push_owner@example.com").await;
    create_repo(&base, &token, "pushed-image", false).await;

    let payload = b"forgekeep-oci-blob-roundtrip";
    let digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    );

    let start = client
        .post(format!(
            "{}/v2/oci_push_owner/pushed-image/blobs/uploads/",
            base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let start_status = start.status();
    if start_status != 202 {
        panic!(
            "start upload failed: {start_status} body={:?}",
            start.text().await.unwrap()
        );
    }
    let location = start
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();

    let chunk = client
        .patch(format!("{}{}", base, location))
        .bearer_auth(&token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202, "chunk upload failed");

    let finish = client
        .put(format!("{}{}", base, location))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(finish.status(), 201, "complete upload failed");

    // The invariant: whatever the push claimed, the blob must now be there.
    let head = client
        .head(format!(
            "{}/v2/oci_push_owner/pushed-image/blobs/{}",
            base, digest
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        head.status(),
        200,
        "push answered 201 but the blob is not retrievable"
    );

    let fetched = client
        .get(format!(
            "{}/v2/oci_push_owner/pushed-image/blobs/{}",
            base, digest
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), 200);
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), payload);
}

/// Start an upload session and stage `payload` in it, returning the session
/// `Location` and its UUID.
async fn staged_upload(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    payload: &[u8],
) -> (String, String) {
    let client = reqwest::Client::new();
    let start = client
        .post(format!("{base}/v2/{owner}/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "start upload failed");
    let location = start
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();
    let uuid = location.rsplit('/').next().unwrap().to_string();

    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202, "chunk upload failed");

    (location, uuid)
}

/// A failed `PUT .../blobs/uploads/{uuid}?digest=` has to say *whose* fault it
/// was.
///
/// Every way of failing used to answer `400 DIGEST_INVALID`, including the ones
/// the registry caused itself. `docker push` does not retry a 400: it prints
/// `digest invalid` and stops, so an unwritable staging directory sent the
/// operator to inspect an image that was fine. Both halves of the distinction
/// are checked against the same request shape.
#[tokio::test]
async fn a_failed_finalize_separates_a_wrong_digest_from_a_broken_registry() {
    let (base, repo_root, oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _user_id) =
        register_full(&base, "oci_blame_owner", "oci_blame_owner@example.com").await;
    create_repo(&base, &token, "blamed-image", false).await;

    let payload = b"forgekeep-oci-blame";
    let digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    );

    // 1. The client named a digest its own bytes do not hash to — its fault.
    let (location, _) = staged_upload(
        &base,
        &token,
        "oci_blame_owner",
        "blamed-image",
        b"different bytes",
    )
    .await;
    let mismatch = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        mismatch.status(),
        400,
        "a digest mismatch is the client's fault"
    );
    let body: serde_json::Value = mismatch.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "DIGEST_INVALID");

    // 2. Same request, correct digest, correct bytes — but the blob store the
    //    verified upload gets published into cannot host it. That is the
    //    registry's own failure: `500`, and the body has to name the path so
    //    the operator can fix the thing that is actually broken.
    let (location, uuid) =
        staged_upload(&base, &token, "oci_blame_owner", "blamed-image", payload).await;
    let occupied = repo_root.join("oci");
    std::fs::write(&occupied, b"not a directory").unwrap();

    let ours = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        ours.status(),
        500,
        "a blob store that cannot take the blob is the registry's fault, not a bad digest"
    );
    let body: serde_json::Value = ours.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "UNKNOWN");
    let message = body["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&occupied.display().to_string()),
        "the 500 must name the blob path it could not write: {message}"
    );
    std::fs::remove_file(&occupied).unwrap();

    // 3. And the staging file itself, which the registry owns just as much:
    //    replaced by a directory, the same PUT must still be a 500 naming it.
    let staged = oci_root
        .join("oci-uploads")
        .join("oci_blame_owner")
        .join("blamed-image")
        .join(&uuid)
        .join("data");
    assert!(
        staged.is_file(),
        "staged upload not at {}",
        staged.display()
    );
    std::fs::remove_file(&staged).unwrap();
    std::fs::create_dir(&staged).unwrap();

    let staging = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        staging.status(),
        500,
        "an unusable staging file is the registry's fault, not a bad digest"
    );
    let body: serde_json::Value = staging.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "UNKNOWN");
    let message = body["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&staged.display().to_string()),
        "the 500 must name the staging path: {message}"
    );
}
