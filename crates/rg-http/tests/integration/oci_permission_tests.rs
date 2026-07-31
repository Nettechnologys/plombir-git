use crate::common::{
    register_full, spawn_test_app_with_db, spawn_test_app_with_oci_root, spawn_test_app_with_state,
};
use axum::{
    body::to_bytes,
    extract::{Path, State},
    http::{header, HeaderMap},
};
use base64::Engine as _;
use sea_orm::ConnectionTrait;

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

/// The raw scoped bearer token `GET /v2/auth/token` hands out — the string a
/// docker client then presents on every request until it expires.
async fn request_oci_token_raw(base: &str, scope: &str, auth_header: Option<String>) -> String {
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
    body["token"]
        .as_str()
        .expect("token in response")
        .to_string()
}

async fn request_oci_token(
    base: &str,
    scope: &str,
    auth_header: Option<String>,
) -> rg_core::auth::oci_token::OciTokenClaims {
    let token = request_oci_token_raw(base, scope, auth_header).await;
    rg_core::auth::oci_token::validate_oci_token(&token, "test-secret-key")
        .expect("valid OCI token")
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
///
/// The same invariant runs in reverse too: once storage confirms the bytes,
/// missing or unreadable metadata must be a server error rather than a
/// successful zero-byte blob. The routed `HEAD` proves the wire status; a direct
/// call to the same production handler also exposes the OCI envelope that HTTP
/// correctly omits from a HEAD response body.
#[tokio::test]
async fn a_created_blob_is_retrievable_right_after_the_push() {
    let (base, db, state) = spawn_test_app_with_state().await;
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
    let expected_length = payload.len().to_string();
    assert_eq!(
        head.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()),
        Some(expected_length.as_str()),
        "HEAD must report the recorded blob size"
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

    db.execute_unprepared("DELETE FROM oci_blob")
        .await
        .expect("delete blob metadata");

    let missing_row = client
        .head(format!(
            "{}/v2/oci_push_owner/pushed-image/blobs/{}",
            base, digest
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        missing_row.status(),
        500,
        "stored bytes without their metadata row must not become 200 Content-Length: 0"
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    let missing_row = rg_http::oci::head_blob(
        State(state.clone()),
        headers.clone(),
        Path((
            "oci_push_owner".to_string(),
            "pushed-image".to_string(),
            digest.clone(),
        )),
    )
    .await;
    assert_eq!(missing_row.status(), 500);
    let missing_body: serde_json::Value = serde_json::from_slice(
        &to_bytes(missing_row.into_body(), usize::MAX)
            .await
            .expect("read missing-row OCI envelope"),
    )
    .expect("missing-row response is OCI JSON");
    assert_eq!(missing_body["errors"][0]["code"], "UNKNOWN");

    db.execute_unprepared("DROP TABLE oci_blob")
        .await
        .expect("drop oci_blob");
    let broken_lookup = rg_http::oci::head_blob(
        State(state),
        headers,
        Path((
            "oci_push_owner".to_string(),
            "pushed-image".to_string(),
            digest,
        )),
    )
    .await;
    assert_eq!(broken_lookup.status(), 500);
    let broken_body: serde_json::Value = serde_json::from_slice(
        &to_bytes(broken_lookup.into_body(), usize::MAX)
            .await
            .expect("read lookup-failure OCI envelope"),
    )
    .expect("lookup failure is OCI JSON");
    assert_eq!(broken_body["errors"][0]["code"], "UNKNOWN");
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

// ── A scoped token outliving the access it was minted for ────────────────
//
// An OCI scoped token is a capability: `get_token` runs the repository gate and
// freezes its answer into the `scope` string. What the string cannot carry is
// the answer's shelf life. Drop the collaborator, deactivate the account, and
// the token keeps saying `pull,push` for the rest of its 300 seconds — long
// enough to push a tag into a private registry, and invisible to
// `session_standing_middleware`, which parses `sub` as a user id while an OCI
// token carries a username there.
//
// Every test below mints its baseline *before* the revocation and in the same
// run: a token that answers 401 proves nothing on its own, since a token that
// was never good answers 401 too.

/// Add `username` to `owner/repo` as a writer, returning the id the removal
/// route takes.
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

/// `POST /v2/{owner}/{repo}/blobs/uploads/` — the first request of a
/// `docker push`, and the cheapest one that needs `push`.
async fn start_upload_with(
    base: &str,
    owner: &str,
    repo: &str,
    oci_token: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v2/{owner}/{repo}/blobs/uploads/"))
        .bearer_auth(oci_token)
        .send()
        .await
        .unwrap()
}

/// `GET /v2/{owner}/{repo}/tags/list` — the cheapest request that needs `pull`.
async fn list_tags_with(base: &str, owner: &str, repo: &str, oci_token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}/v2/{owner}/{repo}/tags/list"))
        .bearer_auth(oci_token)
        .send()
        .await
        .unwrap()
}

/// Dropping a collaborator has to reach the scoped tokens they already hold.
#[tokio::test]
async fn a_scoped_token_stops_working_when_the_collaborator_is_dropped() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "ocirevowner", "ocirevowner@example.com").await;
    register_full(&base, "ocirevmate", "ocirevmate@example.com").await;
    create_repo(&base, &owner_token, "revoke-oci", true).await;
    let mate_id = add_collaborator(
        &base,
        &owner_token,
        "ocirevowner",
        "revoke-oci",
        "ocirevmate",
    )
    .await;

    // The collaborator takes a token while they still may. The gate ran here,
    // and its answer is now frozen into the token for the next five minutes.
    let token = request_oci_token_raw(
        &base,
        "repository:ocirevowner/revoke-oci:pull,push",
        Some(basic_auth("ocirevmate", "Qz7$wRtm")),
    )
    .await;

    let push_before = start_upload_with(&base, "ocirevowner", "revoke-oci", &token).await;
    assert_eq!(
        push_before.status(),
        202,
        "the token never granted a push, so it failing later would prove nothing"
    );
    let pull_before = list_tags_with(&base, "ocirevowner", "revoke-oci", &token).await;
    assert_ne!(
        pull_before.status(),
        401,
        "the token never granted a pull, so it failing later would prove nothing"
    );

    remove_collaborator(&base, &owner_token, "ocirevowner", "revoke-oci", mate_id).await;

    let push_after = start_upload_with(&base, "ocirevowner", "revoke-oci", &token).await;
    assert_eq!(
        push_after.status(),
        401,
        "a dropped collaborator's token still starts a push into the private registry"
    );
    let pull_after = list_tags_with(&base, "ocirevowner", "revoke-oci", &token).await;
    assert_eq!(
        pull_after.status(),
        401,
        "a dropped collaborator's token still pulls from the private registry"
    );

    // The refusal has to be one docker can read: the OCI envelope, not the
    // AppError body, and the code the spec names for it.
    let body: serde_json::Value = push_after.json().await.unwrap();
    assert_eq!(
        body["errors"][0]["code"], "UNAUTHORIZED",
        "the refusal must keep the OCI error-envelope: {body}"
    );
    assert!(
        body.get("error").is_none(),
        "the refusal must not be the AppError JSON body: {body}"
    );
}

/// Deactivating an account has to reach the scoped tokens it already holds.
///
/// This is the half `session_standing_middleware` cannot reach: it resolves a
/// caller by parsing `sub` as a user id, and an OCI token puts a username
/// there, so an offboarded account's token was never even looked at.
#[tokio::test]
async fn deactivating_an_account_revokes_its_unexpired_oci_token() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "ocigoneowner", "ocigoneowner@example.com").await;
    create_repo(&base, &owner_token, "gone-oci", true).await;

    let token = request_oci_token_raw(
        &base,
        "repository:ocigoneowner/gone-oci:pull,push",
        Some(basic_auth("ocigoneowner", "Qz7$wRtm")),
    )
    .await;

    let before = start_upload_with(&base, "ocigoneowner", "gone-oci", &token).await;
    assert_eq!(
        before.status(),
        202,
        "the token never granted a push, so it failing later would prove nothing"
    );

    rg_db::ops::user_ops::update_by_id(&db, owner_id, None, None, None, Some(false))
        .await
        .expect("deactivate user")
        .expect("registered user must exist");

    let after = start_upload_with(&base, "ocigoneowner", "gone-oci", &token).await;
    assert_eq!(
        after.status(),
        401,
        "a deactivated account's unexpired token still pushes to its private registry"
    );
    let pull = list_tags_with(&base, "ocigoneowner", "gone-oci", &token).await;
    assert_eq!(
        pull.status(),
        401,
        "a deactivated account's unexpired token still pulls from its private registry"
    );
}

/// A token that outlived its *repository* rather than its holder.
///
/// The anonymous case: a public image's token is minted for `anonymous` and the
/// gate admits it because the repository is public. Flip the repository to
/// private and the frozen scope still says `pull`.
#[tokio::test]
async fn an_anonymous_scoped_token_stops_working_when_the_repository_turns_private() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "ociflipowner", "ociflipowner@example.com").await;
    create_repo(&base, &owner_token, "flip-oci", false).await;

    let token = request_oci_token_raw(&base, "repository:ociflipowner/flip-oci:pull", None).await;

    let before = list_tags_with(&base, "ociflipowner", "flip-oci", &token).await;
    assert_ne!(
        before.status(),
        401,
        "the anonymous token never pulled, so it failing later would prove nothing"
    );

    // No REST route flips visibility, so the fixture writes the row the read
    // gate reads on every request — the same way the LFS revocation tests do.
    {
        use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
        let repo = rg_db::entities::repository::Entity::find()
            .filter(rg_db::entities::repository::Column::Name.eq("flip-oci"))
            .one(&db)
            .await
            .unwrap()
            .expect("fixture repository");
        let repo_id = repo.id;
        let mut repo: rg_db::entities::repository::ActiveModel = repo.into();
        repo.is_private = Set(true);
        repo.update(&db).await.expect("flip repository to private");
        rg_core::repo::service::invalidate_perm_cache_all(&db);
        let _ = repo_id;
    }

    let after = list_tags_with(&base, "ociflipowner", "flip-oci", &token).await;
    assert_eq!(
        after.status(),
        401,
        "a token minted while the repository was public still pulls after it turned private"
    );
}

/// A database that cannot answer is not a credentials problem.
///
/// The scoped branch now makes DB calls of its own — resolving `sub` to an
/// account, then asking the repository gate — and each is a place where a
/// failure could be folded into "not allowed". Folded, it answers 401, which is
/// the one status that sends docker straight back to the token endpoint to loop
/// instead of backing off. With a perfectly valid token presented, an outage
/// must still read as an outage.
#[tokio::test]
async fn a_database_outage_under_a_scoped_token_is_503_not_401() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _) = register_full(&base, "ocioutowner", "ocioutowner@example.com").await;
    create_repo(&base, &owner_token, "outage-oci", true).await;

    let token = request_oci_token_raw(
        &base,
        "repository:ocioutowner/outage-oci:pull,push",
        Some(basic_auth("ocioutowner", "Qz7$wRtm")),
    )
    .await;

    db.close().await.expect("close pool");

    let response = list_tags_with(&base, "ocioutowner", "outage-oci", &token).await;
    assert_eq!(
        response.status(),
        503,
        "an outage under a valid scoped token must stay retryable, not read as a bad credential"
    );
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(
        body.get("errors").and_then(|e| e.as_array()).is_some(),
        "the outage must keep the OCI error-envelope: {body}"
    );
}
