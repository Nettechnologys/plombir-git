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

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
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

    let payload = b"forgekeep-cross-repository-layer";
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

    let forgekeep_repo =
        rg_core::repo::service::find_repo_by_owner_name(&db, "oci_mount", "target-image")
            .await
            .unwrap()
            .unwrap();
    let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&db, forgekeep_repo.id)
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

/// A `docker push` of an image manifest must be accepted, and pull back byte
/// for byte.
#[tokio::test]
async fn a_docker_image_manifest_pushes_and_pulls_back_unchanged() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "oci_wire", "oci_wire@example.com").await;
    create_repo(&base, &token, "wire-image").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let layer = b"\x1f\x8b\x08\x00forgekeep-layer";
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
