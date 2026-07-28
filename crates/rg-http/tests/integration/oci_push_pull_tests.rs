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
    assert_eq!(finish.status(), 201, "complete upload failed");

    digest
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
