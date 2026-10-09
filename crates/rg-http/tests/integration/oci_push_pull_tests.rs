//! `docker push` / `docker pull` against the registry, in the client's own wire
//! format.
//!
//! The registry had every blob endpoint working and still could not accept a
//! single real push: the manifest parser read `schema_version` / `media_type`
//! while Docker and OCI both put `schemaVersion` / `mediaType` on the wire
//! (card_83a755704a2c). Nothing caught it because no test had ever PUT a
//! manifest in the shape a client sends. These tests are that shape — the
//! bodies below are copied from what a client actually transmits, so a rename
//! on the parser breaks them.

use crate::common::{
    create_repo, register_full, spawn_test_app_with_db, spawn_test_app_with_oci_root,
    spawn_test_app_with_routes_and_db,
};
use sha2::Digest as _;

const DOCKER_MANIFEST_V2: &str = "application/vnd.docker.distribution.manifest.v2+json";
const DOCKER_CONFIG_V1: &str = "application/vnd.docker.container.image.v1+json";
const DOCKER_LAYER_GZ: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";
const OCI_INDEX_V1: &str = "application/vnd.oci.image.index.v1+json";
const OCI_MANIFEST_V1: &str = "application/vnd.oci.image.manifest.v1+json";

fn sha256(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(payload)))
}

/// Upload one blob the way `docker push` does: POST a session, PATCH the bytes,
/// PUT the digest. Returns the blob digest.
async fn push_blob(base: &str, token: &str, owner: &str, repo: &str, payload: &[u8]) -> String {
    let client = reqwest::Client::new();
    let digest = sha256(payload);

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

    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202, "chunk upload failed");

    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(finish, payload.len()).await;

    digest
}

async fn mount_blob(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    digest: &str,
    from: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v2/{owner}/{repo}/blobs/uploads/"))
        .query(&[("mount", digest), ("from", from)])
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
}

/// A cross-repository mount is a complete publish, not just a storage copy.
///
/// The row is what supplies HEAD's size and what `put_manifest` increments. A
/// 201 with only the bytes present makes the layer look healthy while silently
/// dropping its reference accounting.
#[tokio::test]
async fn a_cross_repository_mount_records_the_blob_and_its_manifest_reference() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_mount", "oci_mount@example.com").await;
    create_repo(&base, &token, "source-image").await;
    create_repo(&base, &token, "target-image").await;

    let payload = b"plombir-git-cross-repository-layer";
    let digest = push_blob(&base, &token, "oci_mount", "source-image", payload).await;

    for attempt in 0..2 {
        let mounted = mount_blob(
            &base,
            &token,
            "oci_mount",
            "target-image",
            &digest,
            "oci_mount/source-image",
        )
        .await;
        let status = mounted.status();
        let returned_digest = mounted
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = mounted.text().await.unwrap();
        assert_eq!(
            status, 201,
            "mount attempt {attempt} failed instead of being idempotent: {body}"
        );
        assert_eq!(returned_digest.as_deref(), Some(digest.as_str()));
    }

    let head = client
        .head(format!("{base}/v2/oci_mount/target-image/blobs/{digest}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    let expected_size = payload.len().to_string();
    assert_eq!(
        head.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()),
        Some(expected_size.as_str()),
        "HEAD must report the mounted blob's stored size, not zero"
    );

    let plombir_git_repo =
        rg_core::repo::service::find_repo_by_owner_name(&db, "oci_mount", "target-image")
            .await
            .unwrap()
            .unwrap();
    let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&db, plombir_git_repo.id)
        .await
        .unwrap()
        .expect("the mount must create the target OCI repository row");
    let mounted_blob = rg_db::ops::oci_ops::find_blob(&db, oci_repo.id, &digest)
        .await
        .unwrap()
        .expect("the 201 mount must create the target blob row");
    assert_eq!(mounted_blob.size, payload.len() as i64);

    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": DOCKER_MANIFEST_V2,
        "config": {
            "mediaType": DOCKER_CONFIG_V1,
            "size": payload.len(),
            "digest": digest,
        },
        "layers": [],
    })
    .to_string();
    let pushed = client
        .put(format!(
            "{base}/v2/oci_mount/target-image/manifests/mounted"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, DOCKER_MANIFEST_V2)
        .body(manifest)
        .send()
        .await
        .unwrap();
    let status = pushed.status();
    let body = pushed.text().await.unwrap();
    assert_eq!(
        status, 201,
        "manifest push over mounted blob failed: {body}"
    );

    // The manifest push is the assertion: it claims every blob it names inside
    // its own transaction, so a 201 over a mounted layer proves `put_manifest`
    // found the row the mount created. Before card_dc95e1661124 the mount wrote
    // no row and this push failed.
    assert!(
        rg_db::ops::oci_ops::find_blob(&db, oci_repo.id, &digest)
            .await
            .unwrap()
            .is_some(),
        "the mounted blob row must survive the manifest push that claims it"
    );
}

/// A manifest pushed to a digest must BE that digest.
///
/// `crane copy`, `oras cp` and every mirroring tool address manifests by digest
/// rather than by tag, and the child manifests of a multi-arch index are pushed
/// that way before the index names them. This path used to drop the reference
/// entirely: the manifest was stored under the digest computed from the body,
/// the 201 was returned anyway, and the `GET` the client makes next answered
/// 404 — so an index assembled from those 201s would reference digests the
/// registry never held (card_11ea5daf5c07).
#[tokio::test]
async fn a_manifest_pushed_to_a_digest_that_is_not_its_own_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_digest", "oci_digest@example.com").await;
    create_repo(&base, &token, "digest-push").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let config_digest = push_blob(&base, &token, "oci_digest", "digest-push", config).await;

    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_V1,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
    })
    .to_string();
    let manifest_digest = sha256(manifest.as_bytes());
    let foreign_digest = format!("sha256:{}", "0".repeat(64));
    assert_ne!(manifest_digest, foreign_digest);

    let refused = client
        .put(format!(
            "{base}/v2/oci_digest/digest-push/manifests/{foreign_digest}"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        400,
        "a manifest whose body is not the digest it was pushed to must be refused"
    );
    let refusal: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(refusal["errors"][0]["code"], "MANIFEST_INVALID");
    let refusal_message = refusal["errors"][0]["message"].as_str().unwrap();
    assert!(
        refusal_message.contains(&foreign_digest) && refusal_message.contains(&manifest_digest),
        "the refusal must name both the claimed and the computed digest, got: {refusal_message}"
    );

    // The refusal is a refusal, not a redirect: nothing was published, at
    // either address. A 400 that still wrote the row would leave the registry
    // holding a manifest no client believes it pushed.
    for address in [&foreign_digest, &manifest_digest] {
        let pulled = client
            .get(format!(
                "{base}/v2/oci_digest/digest-push/manifests/{address}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            pulled.status(),
            404,
            "the refused push must leave nothing at {address}"
        );
    }

    // The honest push over the same bytes still works, and pulls back byte for
    // byte — the check refuses a contradiction, it does not refuse the path.
    let accepted = client
        .put(format!(
            "{base}/v2/oci_digest/digest-push/manifests/{manifest_digest}"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    let status = accepted.status();
    let body = accepted.text().await.unwrap();
    assert_eq!(
        status, 201,
        "a push to its own digest must be accepted: {body}"
    );

    let pulled = client
        .get(format!(
            "{base}/v2/oci_digest/digest-push/manifests/{manifest_digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pulled.status(), 200);
    assert_eq!(pulled.text().await.unwrap(), manifest);
}

/// A `docker push` of an image manifest must be accepted, pull back byte for
/// byte, and expose the exact routed `HEAD` contract clients probe first.
#[tokio::test]
async fn a_docker_image_manifest_pushes_pulls_and_heads_back_unchanged() {
    let (base, facts, _db) = spawn_test_app_with_routes_and_db().await;
    let client = reqwest::Client::new();

    // Axum can serve HEAD through a GET route by stripping its response body.
    // A successful request alone therefore does not prove that the dedicated
    // registration survived. Pin the independently-spelled route tuple and
    // handler before checking its wire behaviour below.
    let manifest_head_routes: Vec<_> = facts
        .iter()
        .filter(|fact| {
            fact.method == "HEAD" && fact.path == "/v2/{owner}/{repo}/manifests/{reference}"
        })
        .collect();
    assert_eq!(
        manifest_head_routes.len(),
        1,
        "the production router must carry exactly one explicit HEAD manifest route"
    );
    assert!(
        manifest_head_routes[0]
            .handler
            .ends_with("::oci::head_manifest"),
        "the explicit HEAD manifest route must call oci::head_manifest, got {}",
        manifest_head_routes[0].handler,
    );

    let (token, _user_id) = register_full(&base, "oci_wire", "oci_wire@example.com").await;
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "wire-image",
            "is_private": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        created.status(),
        201,
        "the private OCI fixture repository must be created"
    );

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let layer = b"\x1f\x8b\x08\x00plombir-git-layer";
    let config_digest = push_blob(&base, &token, "oci_wire", "wire-image", config).await;
    let layer_digest = push_blob(&base, &token, "oci_wire", "wire-image", layer).await;

    // Exactly what the Docker client puts on the wire: camelCase keys, no
    // `manifests` array on an image manifest.
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": DOCKER_MANIFEST_V2,
        "config": {
            "mediaType": DOCKER_CONFIG_V1,
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [{
            "mediaType": DOCKER_LAYER_GZ,
            "size": layer.len(),
            "digest": layer_digest,
        }],
    })
    .to_string();
    let manifest_digest = sha256(manifest.as_bytes());

    let pushed = client
        .put(format!("{base}/v2/oci_wire/wire-image/manifests/v1.0.0"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, DOCKER_MANIFEST_V2)
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    let status = pushed.status();
    let body = pushed.text().await.unwrap();
    assert_eq!(
        status, 201,
        "a real docker manifest must be accepted, got: {body}"
    );

    let anonymous = client
        .head(format!("{base}/v2/oci_wire/wire-image/manifests/v1.0.0"))
        .send()
        .await
        .unwrap();
    let anonymous_status = anonymous.status();
    let anonymous_challenge = anonymous
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let anonymous_body = anonymous.bytes().await.unwrap();
    assert_eq!(
        anonymous_status, 401,
        "manifest HEAD must keep the pull gate"
    );
    assert!(
        anonymous_challenge
            .as_deref()
            .is_some_and(|value| value.contains("repository:oci_wire/wire-image:pull")),
        "manifest HEAD must challenge for the repository pull scope, got {anonymous_challenge:?}"
    );
    assert!(
        anonymous_body.is_empty(),
        "an unauthorized HEAD response must not carry its OCI envelope on the wire"
    );

    let expected_length = manifest.len().to_string();
    for reference in ["v1.0.0", manifest_digest.as_str()] {
        let head = client
            .head(format!(
                "{base}/v2/oci_wire/wire-image/manifests/{reference}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(head.status(), 200, "HEAD by {reference} must resolve");
        assert_eq!(
            head.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(DOCKER_MANIFEST_V2),
            "HEAD by {reference} must preserve the manifest media type"
        );
        assert_eq!(
            head.headers()
                .get("Docker-Content-Digest")
                .and_then(|value| value.to_str().ok()),
            Some(manifest_digest.as_str()),
            "HEAD by {reference} must identify the stored manifest"
        );
        assert_eq!(
            head.headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some(expected_length.as_str()),
            "HEAD by {reference} must report the manifest byte length"
        );
        assert!(
            head.bytes().await.unwrap().is_empty(),
            "HEAD by {reference} must not send manifest bytes"
        );
    }

    let missing = client
        .head(format!(
            "{base}/v2/oci_wire/wire-image/manifests/does-not-exist"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let missing_status = missing.status();
    let missing_body = missing.bytes().await.unwrap();
    assert_eq!(
        missing_status, 404,
        "an absent manifest stays MANIFEST_UNKNOWN"
    );
    assert!(
        missing_body.is_empty(),
        "a missing manifest HEAD must not send its OCI error envelope"
    );

    // Pull by tag: the bytes a client verifies its digest against.
    let by_tag = client
        .get(format!("{base}/v2/oci_wire/wire-image/manifests/v1.0.0"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(by_tag.status(), 200);
    assert_eq!(
        by_tag
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(DOCKER_MANIFEST_V2),
    );
    assert_eq!(
        by_tag
            .headers()
            .get("Docker-Content-Digest")
            .and_then(|v| v.to_str().ok()),
        Some(manifest_digest.as_str()),
    );
    assert_eq!(
        by_tag.text().await.unwrap(),
        manifest,
        "the pulled manifest must be the pushed bytes"
    );

    // Pull by digest — the form `docker pull image@sha256:...` uses.
    let by_digest = client
        .get(format!(
            "{base}/v2/oci_wire/wire-image/manifests/{manifest_digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(by_digest.status(), 200);
    assert_eq!(by_digest.text().await.unwrap(), manifest);

    let tags: serde_json::Value = client
        .get(format!("{base}/v2/oci_wire/wire-image/tags/list"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tags["tags"], serde_json::json!(["v1.0.0"]));
}

/// One image, several names — the sequence every CI runs.
///
/// ```text
/// docker tag app:$SHA app:latest
/// docker push app:$SHA && docker push app:latest
/// ```
///
/// Both pushes carry byte-identical manifests, so the second one used to reach
/// the `UNIQUE(repository, digest)` key that the tag column shared with the
/// image row and come back `500 UNKNOWN / failed to record manifest`
/// (card_56f118bbe845). Nothing caught it because every tag fixture in this
/// suite gave each tag a manifest of its own. `docker push -a`, promoting
/// `staging → prod` by tag and any mirror copying a multi-tagged image all
/// break the same way.
#[tokio::test]
async fn one_image_pushed_under_several_tags_keeps_every_name() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_retag", "oci_retag@example.com").await;
    create_repo(&base, &token, "many-tags").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let layer = b"\x1f\x8b\x08\x00plombir-git-retagged-layer";
    let config_digest = push_blob(&base, &token, "oci_retag", "many-tags", config).await;
    let layer_digest = push_blob(&base, &token, "oci_retag", "many-tags", layer).await;
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": DOCKER_MANIFEST_V2,
        "config": {
            "mediaType": DOCKER_CONFIG_V1,
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [{
            "mediaType": DOCKER_LAYER_GZ,
            "size": layer.len(),
            "digest": layer_digest,
        }],
    })
    .to_string();
    let manifest_digest = sha256(manifest.as_bytes());

    let push_reference = |reference: String, body: String| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        async move {
            client
                .put(format!(
                    "{base}/v2/oci_retag/many-tags/manifests/{reference}"
                ))
                .bearer_auth(&token)
                .header(reqwest::header::CONTENT_TYPE, DOCKER_MANIFEST_V2)
                .body(body)
                .send()
                .await
                .unwrap()
        }
    };

    // The `$SHA` tag, the digest form a mirror uses, and `latest` — three
    // references, one set of bytes.
    for reference in [
        "1a2b3c4d".to_string(),
        manifest_digest.clone(),
        "latest".to_string(),
    ] {
        let pushed = push_reference(reference.clone(), manifest.clone()).await;
        let status = pushed.status();
        let body = pushed.text().await.unwrap();
        assert_eq!(
            status, 201,
            "pushing the same image under {reference} was refused: {body}"
        );
    }

    let tags: serde_json::Value = client
        .get(format!("{base}/v2/oci_retag/many-tags/tags/list"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        tags["tags"],
        serde_json::json!(["1a2b3c4d", "latest"]),
        "a push by digest must not invent a tag, and both real names must be listed"
    );

    for reference in ["1a2b3c4d", "latest", manifest_digest.as_str()] {
        let pulled = client
            .get(format!(
                "{base}/v2/oci_retag/many-tags/manifests/{reference}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(pulled.status(), 200, "pull of {reference} failed");
        assert_eq!(
            pulled
                .headers()
                .get("Docker-Content-Digest")
                .and_then(|value| value.to_str().ok()),
            Some(manifest_digest.as_str()),
            "{reference} resolved to a different image"
        );
        assert_eq!(pulled.text().await.unwrap(), manifest);
    }

    // Promoting a new build onto `latest` moves that one name and leaves the
    // release tag pointing at what it was pinned to.
    let next_layer = b"\x1f\x8b\x08\x00plombir-git-next-layer";
    let next_layer_digest = push_blob(&base, &token, "oci_retag", "many-tags", next_layer).await;
    let next_manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": DOCKER_MANIFEST_V2,
        "config": {
            "mediaType": DOCKER_CONFIG_V1,
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [{
            "mediaType": DOCKER_LAYER_GZ,
            "size": next_layer.len(),
            "digest": next_layer_digest,
        }],
    })
    .to_string();
    let moved = push_reference("latest".to_string(), next_manifest.clone()).await;
    assert_eq!(moved.status(), 201, "moving a tag onto a new image failed");

    let pinned = client
        .get(format!("{base}/v2/oci_retag/many-tags/manifests/1a2b3c4d"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pinned.status(), 200);
    assert_eq!(
        pinned.text().await.unwrap(),
        manifest,
        "moving `latest` dragged the release tag with it"
    );
    let promoted = client
        .get(format!("{base}/v2/oci_retag/many-tags/manifests/latest"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(promoted.status(), 200);
    assert_eq!(promoted.text().await.unwrap(), next_manifest);
}

/// OCI tag pagination is marker-based: every page starts strictly after the
/// previous page's last tag, in the same total order the unpaged response uses.
#[tokio::test]
async fn tag_pages_follow_links_without_duplicates_and_keep_the_unpaged_contract() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_tags", "oci_tags@example.com").await;
    create_repo(&base, &token, "tag-pages").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let config_digest = push_blob(&base, &token, "oci_tags", "tag-pages", config).await;

    // Deliberately not lexical. The routed assertion pins the external order;
    // the source guard in `rg-db` separately holds the explicit ORDER BY because
    // SQLite may happen to read the unique tag index in order without it.
    for tag in ["zeta", "alpha", "middle", "beta", "gamma"] {
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_V1,
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "size": config.len(),
                "digest": config_digest,
            },
            "layers": [],
            "annotations": {
                "org.opencontainers.image.ref.name": tag,
            },
        })
        .to_string();
        let pushed = client
            .put(format!("{base}/v2/oci_tags/tag-pages/manifests/{tag}"))
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
            .body(manifest)
            .send()
            .await
            .unwrap();
        let status = pushed.status();
        let body = pushed.text().await.unwrap();
        assert_eq!(status, 201, "fixture tag {tag} failed: {body}");
    }

    let expected = ["alpha", "beta", "gamma", "middle", "zeta"];
    let unpaged = client
        .get(format!("{base}/v2/oci_tags/tag-pages/tags/list"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(unpaged.status(), 200);
    assert!(
        unpaged.headers().get(reqwest::header::LINK).is_none(),
        "an unpaged request must remain a complete response"
    );
    let unpaged_body: serde_json::Value = unpaged.json().await.unwrap();
    assert_eq!(unpaged_body["tags"], serde_json::json!(expected));

    let mut next_path = Some("/v2/oci_tags/tag-pages/tags/list?n=2".to_string());
    let mut walked = Vec::new();
    let mut links = Vec::new();
    let mut requested_paths = std::collections::BTreeSet::new();
    while let Some(path) = next_path.take() {
        assert!(
            requested_paths.insert(path.clone()),
            "the pagination Link repeated {path} instead of advancing `last`"
        );
        let response = client
            .get(format!("{base}{path}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "page request failed: {path}");
        let link = response
            .headers()
            .get(reqwest::header::LINK)
            .map(|value| value.to_str().unwrap().to_string());
        let body: serde_json::Value = response.json().await.unwrap();
        walked.extend(
            body["tags"]
                .as_array()
                .expect("tags array")
                .iter()
                .map(|tag| tag.as_str().expect("tag string").to_string()),
        );

        next_path = link.as_deref().map(|link| {
            links.push(link.to_string());
            let (target, relation) = link
                .strip_prefix('<')
                .and_then(|link| link.split_once('>'))
                .expect("Link must contain one bracketed target");
            assert_eq!(relation, "; rel=\"next\"");
            target.to_string()
        });
    }

    assert_eq!(walked, expected);
    assert_eq!(
        walked
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        expected.len(),
        "the page walk returned a duplicate tag"
    );
    assert_eq!(
        links,
        [
            "</v2/oci_tags/tag-pages/tags/list?n=2&last=beta>; rel=\"next\"",
            "</v2/oci_tags/tag-pages/tags/list?n=2&last=middle>; rel=\"next\"",
        ]
    );

    let after_beta = client
        .get(format!("{base}/v2/oci_tags/tag-pages/tags/list?last=beta"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(after_beta.status(), 200);
    assert!(after_beta.headers().get(reqwest::header::LINK).is_none());
    assert_eq!(
        after_beta.json::<serde_json::Value>().await.unwrap()["tags"],
        serde_json::json!(["gamma", "middle", "zeta"])
    );

    let zero = client
        .get(format!("{base}/v2/oci_tags/tag-pages/tags/list?n=0"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(zero.status(), 200);
    assert!(zero.headers().get(reqwest::header::LINK).is_none());
    assert_eq!(
        zero.json::<serde_json::Value>().await.unwrap()["tags"],
        serde_json::json!([])
    );

    let invalid = client
        .get(format!("{base}/v2/oci_tags/tag-pages/tags/list?n=-1"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);
    assert_eq!(
        invalid.json::<serde_json::Value>().await.unwrap()["errors"][0]["code"],
        "PAGINATION_NUMBER_INVALID"
    );
}

/// A multi-arch push ends with an image index, which carries `manifests` and no
/// `layers` at all — the other half of the wire format.
#[tokio::test]
async fn an_oci_image_index_pushes_without_layers() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_index", "oci_index@example.com").await;
    create_repo(&base, &token, "wire-index").await;

    let config = br#"{"architecture":"arm64","os":"linux"}"#;
    let config_digest = push_blob(&base, &token, "oci_index", "wire-index", config).await;

    let child = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_V1,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
    })
    .to_string();
    let child_digest = sha256(child.as_bytes());

    let child_pushed = client
        .put(format!(
            "{base}/v2/oci_index/wire-index/manifests/{child_digest}"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
        .body(child.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(child_pushed.status(), 201, "child manifest push failed");

    // The index: no `config`, no `layers`, only sub-manifests.
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX_V1,
        "manifests": [{
            "mediaType": OCI_MANIFEST_V1,
            "size": child.len(),
            "digest": child_digest,
            "platform": { "architecture": "arm64", "os": "linux" },
        }],
    })
    .to_string();

    let pushed = client
        .put(format!("{base}/v2/oci_index/wire-index/manifests/latest"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_INDEX_V1)
        .body(index.clone())
        .send()
        .await
        .unwrap();
    let status = pushed.status();
    let body = pushed.text().await.unwrap();
    assert_eq!(
        status, 201,
        "an image index carries no layers and must still be accepted, got: {body}"
    );

    let pulled = client
        .get(format!("{base}/v2/oci_index/wire-index/manifests/latest"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pulled.status(), 200);
    assert_eq!(pulled.text().await.unwrap(), index);
}

/// A manifest whose `Content-Type` header and whose body `mediaType` disagree
/// is refused, and the type that gets stored is the one the body declares.
///
/// The two are the same claim written twice, and nothing compared them: the
/// header was the only value `oci_manifest.media_type` ever saw, so a client
/// could PUT bytes that declare `application/vnd.oci.image.manifest.v1+json`
/// under `application/vnd.docker.distribution.manifest.v2+json` and every
/// later pull was handed the header's type for a body that says otherwise.
/// Pullers dispatch on that type — a registry that returns a type no parser
/// checks turns a type-confusion into a parse a client did not agree to.
#[tokio::test]
async fn a_manifest_whose_body_media_type_contradicts_its_header_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_media", "oci_media@example.com").await;
    create_repo(&base, &token, "media-type").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let config_digest = push_blob(&base, &token, "oci_media", "media-type", config).await;

    // The header says Docker, the body says OCI. Both are valid on their own;
    // the pair is not.
    let contradictory = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_V1,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
    })
    .to_string();

    let refused = client
        .put(format!("{base}/v2/oci_media/media-type/manifests/contradiction"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, DOCKER_MANIFEST_V2)
        .body(contradictory.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        400,
        "a Content-Type that contradicts the body's mediaType must be refused"
    );
    let refusal: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(refusal["errors"][0]["code"], "MANIFEST_INVALID");
    let refusal_message = refusal["errors"][0]["message"].as_str().unwrap();
    assert!(
        refusal_message.contains(DOCKER_MANIFEST_V2)
            && refusal_message.contains(OCI_MANIFEST_V1),
        "the refusal must name both declared types, got: {refusal_message}"
    );

    // Nothing was published under the contradicting tag — the refusal has to be
    // a refusal, not a write the client is told about with a 400.
    let pulled = client
        .get(format!(
            "{base}/v2/oci_media/media-type/manifests/contradiction"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        pulled.status(),
        404,
        "the refused push must leave nothing behind"
    );

    // The honest push of the same bytes is accepted, and the type served back
    // is the document's own `mediaType`.
    let accepted = client
        .put(format!("{base}/v2/oci_media/media-type/manifests/agreed"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
        .body(contradictory.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 201);
    let served = client
        .get(format!("{base}/v2/oci_media/media-type/manifests/agreed"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(served.status(), 200);
    assert_eq!(
        served
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some(OCI_MANIFEST_V1),
        "the stored and served type must be the body's mediaType"
    );
    assert_eq!(served.text().await.unwrap(), contradictory);

    // A body that carries no `mediaType` at all still publishes under the
    // header it arrived with: older `docker push` builds omit the field, and
    // refusing them would be a compatibility break, not a check.
    let header_only = serde_json::json!({
        "schemaVersion": 2,
        "config": {
            "mediaType": DOCKER_CONFIG_V1,
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
    })
    .to_string();
    let accepted = client
        .put(format!(
            "{base}/v2/oci_media/media-type/manifests/header-only"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, DOCKER_MANIFEST_V2)
        .body(header_only)
        .send()
        .await
        .unwrap();
    assert_eq!(
        accepted.status(),
        201,
        "a body without mediaType must still publish under the header"
    );
    let served = client
        .get(format!(
            "{base}/v2/oci_media/media-type/manifests/header-only"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(served.status(), 200);
    assert_eq!(
        served
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some(DOCKER_MANIFEST_V2),
        "a body without mediaType is stored under its Content-Type"
    );
}

/// An image index is only meaningful if every child it names can be pulled.
///
/// The children are separate manifests pushed in advance, and a pull of the
/// index is followed by a pull of each child by digest. Nothing checked that
/// the children existed: an index naming a digest no push ever wrote was
/// accepted, and the failure only surfaced on the client, halfway through a
/// pull, as a 404 that reads like a broken registry. The digest in a
/// descriptor is a lookup here, and the declared size has to be the stored
/// manifest's own.
#[tokio::test]
async fn an_image_index_must_name_child_manifests_the_repository_holds() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_index_child", "oci_index_child@example.com")
        .await;
    create_repo(&base, &token, "child-check").await;

    let config = br#"{"architecture":"arm64","os":"linux"}"#;
    let config_digest = push_blob(&base, &token, "oci_index_child", "child-check", config).await;

    let child = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_V1,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
    })
    .to_string();
    let child_digest = sha256(child.as_bytes());

    // The index names the child's digest, but no push has written the child.
    let missing_child_index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX_V1,
        "manifests": [{
            "mediaType": OCI_MANIFEST_V1,
            "size": child.len(),
            "digest": child_digest,
            "platform": { "architecture": "arm64", "os": "linux" },
        }],
    })
    .to_string();

    let refused = client
        .put(format!("{base}/v2/oci_index_child/child-check/manifests/unbuilt"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_INDEX_V1)
        .body(missing_child_index.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        400,
        "an index naming a child the repository does not hold must be refused"
    );
    let refusal: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(refusal["errors"][0]["code"], "MANIFEST_BLOB_UNKNOWN");
    let refusal_message = refusal["errors"][0]["message"].as_str().unwrap();
    assert!(
        refusal_message.contains(&child_digest),
        "the refusal must name the missing child, got: {refusal_message}"
    );

    // Now publish the child, and prepare the same index with a size that
    // contradicts what was stored.
    let child_pushed = client
        .put(format!(
            "{base}/v2/oci_index_child/child-check/manifests/{child_digest}"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
        .body(child.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(child_pushed.status(), 201);

    let wrong_size_index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX_V1,
        "manifests": [{
            "mediaType": OCI_MANIFEST_V1,
            "size": child.len() + 1,
            "digest": child_digest,
            "platform": { "architecture": "arm64", "os": "linux" },
        }],
    })
    .to_string();
    let refused = client
        .put(format!("{base}/v2/oci_index_child/child-check/manifests/stale"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_INDEX_V1)
        .body(wrong_size_index)
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        400,
        "an index descriptor whose size is not the child's size must be refused"
    );
    let refusal: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(refusal["errors"][0]["code"], "MANIFEST_INVALID");

    // Neither refusal published anything, and the honest index over the child
    // that is now stored is accepted.
    for tag in ["unbuilt", "stale"] {
        let pulled = client
            .get(format!(
                "{base}/v2/oci_index_child/child-check/manifests/{tag}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(pulled.status(), 404, "refused index {tag} must not exist");
    }

    let honest_index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX_V1,
        "manifests": [{
            "mediaType": OCI_MANIFEST_V1,
            "size": child.len(),
            "digest": child_digest,
            "platform": { "architecture": "arm64", "os": "linux" },
        }],
    })
    .to_string();
    let pushed = client
        .put(format!("{base}/v2/oci_index_child/child-check/manifests/latest"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_INDEX_V1)
        .body(honest_index.clone())
        .send()
        .await
        .unwrap();
    let status = pushed.status();
    let body = pushed.text().await.unwrap();
    assert_eq!(status, 201, "the honest index must publish: {body}");

    let served = client
        .get(format!(
            "{base}/v2/oci_index_child/child-check/manifests/latest"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(served.status(), 200);
    assert_eq!(served.text().await.unwrap(), honest_index);

    // Every descriptor a client follows from the index resolves, which is what
    // the push-time check was for.
    let child_served = client
        .get(format!(
            "{base}/v2/oci_index_child/child-check/manifests/{child_digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(child_served.status(), 200);
}

/// A reference the registry cannot address is refused — it is never published
/// under a name no client can use.
///
/// `Reference::parse` used to call a reference a digest only when it began with
/// `sha256:`, and *everything else* a tag. One line, two holes
/// (card_413991c11c56):
///
/// * the digest verification added by card_11ea5daf5c07 runs on the `Digest`
///   arm only, so `PUT .../manifests/sha512:<128 hex>` took the tag path and
///   was stored without anything ever comparing the body to the address —
///   exactly the defect that verification closed, reachable sideways by
///   naming another algorithm;
/// * the spec's tag grammar is `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}` and has no
///   `:` in it, so `tags/list` then advertised `sha512:0000…` as a tag.
///   `docker pull repo:sha512:0000…` does not parse that back — the registry
///   was publishing a name it could not be asked for.
#[tokio::test]
async fn a_reference_that_is_neither_a_servable_tag_nor_a_verifiable_digest_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_refname", "oci_refname@example.com").await;
    create_repo(&base, &token, "reference-grammar").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let config_digest = push_blob(&base, &token, "oci_refname", "reference-grammar", config).await;
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_V1,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
    })
    .to_string();

    // Each refusal names the code a client can act on: an algorithm this
    // registry declines is `UNSUPPORTED`, a malformed digest `DIGEST_INVALID`,
    // a name that is not a tag `TAG_INVALID`. None of them is a 500.
    let sha512 = format!("sha512:{}", "0".repeat(128));
    for (reference, expected_code) in [
        (sha512.as_str(), "UNSUPPORTED"),
        ("sha256:not-a-hash", "DIGEST_INVALID"),
        ("not:a:digest", "DIGEST_INVALID"),
        ("not%20a%20tag!!", "TAG_INVALID"),
        (".leading-dot", "TAG_INVALID"),
    ] {
        let refused = client
            .put(format!(
                "{base}/v2/oci_refname/reference-grammar/manifests/{reference}"
            ))
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
            .body(manifest.clone())
            .send()
            .await
            .unwrap();
        let status = refused.status();
        let body = refused.text().await.unwrap();
        assert_eq!(
            status, 400,
            "push to {reference} must be refused, got {status}: {body}"
        );
        let refusal: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            refusal["errors"][0]["code"], expected_code,
            "push to {reference} answered the wrong code: {body}"
        );

        // A read of the same address is refused the same way, rather than
        // being looked up as a tag that could never have been written.
        let pulled = client
            .get(format!(
                "{base}/v2/oci_refname/reference-grammar/manifests/{reference}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            pulled.status(),
            400,
            "pull of {reference} must be refused rather than resolved"
        );
    }

    // Nothing was published, and above all nothing is advertised: the listing
    // is what a client reads to build its next URL.
    let tags: serde_json::Value = client
        .get(format!("{base}/v2/oci_refname/reference-grammar/tags/list"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        tags["tags"],
        serde_json::json!([]),
        "a refused reference must not reach the tag listing"
    );

    // The refusal is about the grammar, not about the path: the same body under
    // a legal tag and under its own digest is still accepted.
    let manifest_digest = sha256(manifest.as_bytes());
    for reference in ["v1.0.0", manifest_digest.as_str()] {
        let pushed = client
            .put(format!(
                "{base}/v2/oci_refname/reference-grammar/manifests/{reference}"
            ))
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
            .body(manifest.clone())
            .send()
            .await
            .unwrap();
        let status = pushed.status();
        let body = pushed.text().await.unwrap();
        assert_eq!(status, 201, "push to {reference} was refused: {body}");
    }
    let tags: serde_json::Value = client
        .get(format!("{base}/v2/oci_refname/reference-grammar/tags/list"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tags["tags"], serde_json::json!(["v1.0.0"]));
}

/// A blob address that is not a digest is the client's mistake on every blob
/// endpoint — and a blob store that cannot answer is still the registry's.
///
/// `HEAD`, `GET` and the `?mount=` branch of the upload start all take the
/// address straight out of the URL, and two of the three used to hand it to
/// storage and repeat whatever came back as `500 UNKNOWN` — with a body reading
/// `unsupported or invalid OCI digest`, so the registry named the request as the
/// culprit and reported its own failure in the same breath (card_8305dc17824e).
/// `5xx` is the retryable class: a mirroring tool re-sends a request that can
/// never become valid, and the operator reads it as the registry refusing.
/// `get_blob` had the mirror image and answered `400 DIGEST_INVALID` to
/// everything, storage failures included.
///
/// The second half is what keeps the first honest. A rule of "always blame the
/// client" would satisfy every assertion above, so the same endpoints are asked
/// again over a store that genuinely cannot answer, and must own that one.
#[tokio::test]
async fn a_blob_address_that_is_not_a_digest_is_refused_but_a_broken_store_is_ours() {
    let (base, repo_root, _oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_blobref", "oci_blobref@example.com").await;
    create_repo(&base, &token, "source-image").await;
    create_repo(&base, &token, "target-image").await;

    let payload = b"plombir-git-blob-address-grammar";
    let digest = push_blob(&base, &token, "oci_blobref", "source-image", payload).await;

    // The codes are the manifest endpoints': a blob has no tags, so `latest` is
    // a digest that was never written as one, while an algorithm this registry
    // cannot compute stays `UNSUPPORTED` — a distinction a mirroring client acts
    // on.
    let sha512 = format!("sha512:{}", "0".repeat(128));
    for (reference, expected_code) in [
        ("not-a-digest", "DIGEST_INVALID"),
        ("latest", "DIGEST_INVALID"),
        ("sha256:not-a-hash", "DIGEST_INVALID"),
        (sha512.as_str(), "UNSUPPORTED"),
    ] {
        let head = client
            .head(format!(
                "{base}/v2/oci_blobref/source-image/blobs/{reference}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            head.status(),
            400,
            "HEAD of {reference} is the client's mistake, not the registry's failure"
        );

        // The code only travels in a body, and a HEAD has none — the same
        // address through GET carries it.
        let pulled = client
            .get(format!(
                "{base}/v2/oci_blobref/source-image/blobs/{reference}"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        let status = pulled.status();
        let body = pulled.text().await.unwrap();
        assert_eq!(status, 400, "GET of {reference} was not refused: {body}");
        let refusal: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            refusal["errors"][0]["code"], expected_code,
            "GET of {reference} answered the wrong code: {body}"
        );

        let mounted = mount_blob(
            &base,
            &token,
            "oci_blobref",
            "target-image",
            reference,
            "oci_blobref/source-image",
        )
        .await;
        let status = mounted.status();
        let body = mounted.text().await.unwrap();
        assert_eq!(status, 400, "mount of {reference} was not refused: {body}");
        let refusal: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            refusal["errors"][0]["code"], expected_code,
            "mount of {reference} answered the wrong code: {body}"
        );
    }

    // The refusal is about the address, not about the endpoint: a real digest
    // still mounts.
    let mounted = mount_blob(
        &base,
        &token,
        "oci_blobref",
        "target-image",
        &digest,
        "oci_blobref/source-image",
    )
    .await;
    let status = mounted.status();
    let body = mounted.text().await.unwrap();
    assert_eq!(
        status, 201,
        "an honest cross-repository mount failed: {body}"
    );

    // `oci/` is the prefix every registry key hangs off. A file in its place
    // makes each lookup fail for a reason no digest can be blamed for, and the
    // same two endpoints must now answer for it themselves.
    let occupied = repo_root.join("oci");
    std::fs::remove_dir_all(&occupied).unwrap();
    std::fs::write(&occupied, b"not a directory").unwrap();

    let head = client
        .head(format!("{base}/v2/oci_blobref/source-image/blobs/{digest}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        head.status(),
        500,
        "a blob store that cannot answer is the registry's failure, not a bad digest"
    );

    let mounted = mount_blob(
        &base,
        &token,
        "oci_blobref",
        "target-image",
        &digest,
        "oci_blobref/source-image",
    )
    .await;
    let status = mounted.status();
    let body = mounted.text().await.unwrap();
    assert_eq!(
        status, 500,
        "a mount over an unreachable blob store is the registry's failure: {body}"
    );
}

/// A manifest larger than Axum's hidden 2 MiB default must reach the handler.
///
/// The route declared no limit at all, so its real ceiling was whatever
/// `DefaultBodyLimit` happened to be — 2 MiB, below the 4 MiB the distribution
/// spec allows, and nowhere in the route table (card_6cbde71c452d). An image
/// index with many platform entries and annotations reaches that size in
/// practice, and the refusal arrived as a bare framework `413` no registry
/// client can explain. Removing `Wrap::body_limit` from the PUT turns this
/// test red.
#[tokio::test]
async fn a_manifest_above_the_axum_default_is_accepted_below_the_spec_ceiling() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_fat", "oci_fat@example.com").await;
    create_repo(&base, &token, "fat-image").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let config_digest = push_blob(&base, &token, "oci_fat", "fat-image", config).await;

    // Legal by the spec and larger than the 2 MiB default: the annotation is
    // the padding, so the manifest stays a manifest the parser accepts.
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_V1,
        "config": {
            "mediaType": DOCKER_CONFIG_V1,
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
        "annotations": {
            "io.plombir-git.test.padding": "p".repeat(3 * 1024 * 1024),
        },
    })
    .to_string();
    assert!(
        manifest.len() > 2 * 1024 * 1024,
        "the fixture must cross Axum's default, got {} byte(s)",
        manifest.len()
    );
    let manifest_digest = sha256(manifest.as_bytes());

    let pushed = client
        .put(format!("{base}/v2/oci_fat/fat-image/manifests/v1.0.0"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    let status = pushed.status();
    let body = pushed.text().await.unwrap();
    assert_eq!(
        status, 201,
        "a manifest within the spec's 4 MiB ceiling must be published: {body}"
    );

    let pulled = client
        .get(format!(
            "{base}/v2/oci_fat/fat-image/manifests/{manifest_digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pulled.status(), 200);
    assert_eq!(
        pulled.text().await.unwrap(),
        manifest,
        "the stored manifest must be the bytes that were pushed"
    );
}

/// Above the declared ceiling the push is refused, and nothing is published.
#[tokio::test]
async fn a_manifest_above_the_declared_ceiling_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_huge", "oci_huge@example.com").await;
    create_repo(&base, &token, "huge-image").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let config_digest = push_blob(&base, &token, "oci_huge", "huge-image", config).await;

    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_V1,
        "config": {
            "mediaType": DOCKER_CONFIG_V1,
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
        "annotations": {
            "io.plombir-git.test.padding": "p".repeat(5 * 1024 * 1024),
        },
    })
    .to_string();
    let manifest_digest = sha256(manifest.as_bytes());

    let pushed = client
        .put(format!("{base}/v2/oci_huge/huge-image/manifests/v1.0.0"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    let status = pushed.status();
    let refusal = pushed.text().await.unwrap();
    assert_eq!(
        status, 413,
        "a manifest past the declared ceiling must be refused: {refusal}"
    );
    // The client declares `Content-Length`, so the transport ceiling answers
    // this one before any handler runs. It still has to answer as a registry:
    // `docker push` prints the envelope's message and nothing else.
    assert!(
        refusal.contains("SIZE_INVALID"),
        "the refusal must carry the registry error envelope, got: {refusal}"
    );

    let pulled = client
        .get(format!(
            "{base}/v2/oci_huge/huge-image/manifests/{manifest_digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        pulled.status(),
        404,
        "a refused push must publish nothing under its digest"
    );
}
