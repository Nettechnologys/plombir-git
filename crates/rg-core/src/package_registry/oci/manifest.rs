//! OCI Manifest parsing and validation.
//!
//! Supports Docker V2 Schema 2 and OCI Image Spec v1 manifests.
//! Extracts layer digests for blob reference tracking.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Parsed manifest — generic over Docker V2 and OCI spec formats.
///
/// The wire format is camelCase (`schemaVersion`, `mediaType`) in both the
/// Docker V2 Schema 2 and the OCI Image Spec — every field rename here is
/// load-bearing, not cosmetic: without it no real `docker push` parses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Schema version (1 or 2)
    pub schema_version: u32,
    /// Media type.  OPTIONAL in OCI Image Spec v1.0 — older tooling omits it,
    /// and the HTTP layer has already validated the `Content-Type` header by
    /// the time we get here, so a missing value is not a client error.
    #[serde(default)]
    pub media_type: Option<String>,
    /// Config layer digest + size
    pub config: Option<ManifestLayer>,
    /// Image/variant layers — absent in an image index.
    #[serde(default)]
    pub layers: Vec<ManifestLayer>,
    /// For manifest lists / image indexes: sub-manifests — absent in an image
    /// manifest.
    #[serde(default)]
    pub manifests: Vec<ManifestDescriptor>,
    /// Annotations (OCI spec)
    #[serde(default)]
    pub annotations: std::collections::HashMap<String, String>,
}

/// A layer reference (config, layer, or sub-manifest).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestLayer {
    pub media_type: String,
    pub size: u64,
    pub digest: String,
}

/// A sub-manifest in an image index (manifest list).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestDescriptor {
    pub media_type: String,
    pub size: u64,
    pub digest: String,
    #[serde(default)]
    pub platform: Option<Platform>,
}

/// Platform descriptor for multi-arch images.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub variant: Option<String>,
}

/// Raw parsed result including the original JSON and computed digest.
#[derive(Debug, Clone)]
pub struct ParsedManifest {
    /// The parsed manifest structure
    pub manifest: Manifest,
    /// Computed digest (sha256:...)
    pub digest: String,
    /// Raw JSON bytes
    pub raw_json: Vec<u8>,
    /// JSON size in bytes
    pub size: u64,
}

impl ParsedManifest {
    /// Parse and validate manifest JSON bytes.
    /// Computes the canonical digest from the raw JSON.
    pub fn parse(data: &[u8]) -> anyhow::Result<Self> {
        // Compute digest from raw bytes before deserialization
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(data)));

        let manifest: Manifest = serde_json::from_slice(data)?;

        // Validate schema version
        if manifest.schema_version != 2 {
            anyhow::bail!("unsupported schema version: {}", manifest.schema_version);
        }

        // Validate media type when the manifest carries one.
        if let Some(media_type) = manifest.media_type.as_deref() {
            if !super::types::media_types::MANIFEST_TYPES.contains(&media_type) {
                anyhow::bail!("unsupported manifest media type: {}", media_type);
            }
        }

        let size = data.len() as u64;

        Ok(Self {
            manifest,
            digest,
            raw_json: data.to_vec(),
            size,
        })
    }

    /// Collect all blob digests referenced by this manifest.
    /// Returns digests for config + all layers (not sub-manifests).
    pub fn referenced_blobs(&self) -> Vec<String> {
        let mut blobs = Vec::new();
        if let Some(ref config) = self.manifest.config {
            blobs.push(config.digest.clone());
        }
        for layer in &self.manifest.layers {
            blobs.push(layer.digest.clone());
        }
        blobs
    }

    /// `true` if this is a manifest list / image index.
    pub fn is_manifest_list(&self) -> bool {
        !self.manifest.manifests.is_empty()
    }

    /// Get the config layer digest if present.
    pub fn config_digest(&self) -> Option<&str> {
        self.manifest.config.as_ref().map(|c| c.digest.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Docker V2 Schema 2 image manifest, verbatim in the shape `docker push`
    /// sends it — camelCase keys, no `manifests` array.
    const DOCKER_V2S2_IMAGE: &str = r#"{
  "schemaVersion": 2,
  "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
  "config": {
    "mediaType": "application/vnd.docker.container.image.v1+json",
    "size": 1469,
    "digest": "sha256:2b8fd9751c4c0f5dd266fcae00707e67c2c19f0b16b8e9e1b6d8b6d7d3c6a1b2"
  },
  "layers": [
    {
      "mediaType": "application/vnd.docker.image.rootfs.diff.tar.gzip",
      "size": 2789742,
      "digest": "sha256:31603596830fc7e56753139f9c2c6bd3759e48a850659506ebfb885d0c2a3a1e"
    }
  ]
}"#;

    /// An OCI Image Spec v1 image manifest — same layout, OCI media types, plus
    /// the annotations block real builders emit.
    const OCI_IMAGE: &str = r#"{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "config": {
    "mediaType": "application/vnd.oci.image.config.v1+json",
    "size": 581,
    "digest": "sha256:9a2b5f5b0c9d0a0f7bd1d9f4b7cba1d1a09ef2fdc1f2a0a2a9c7f0e1d2c3b4a5"
  },
  "layers": [
    {
      "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
      "size": 3372,
      "digest": "sha256:8c5f2a3b0d1e4f6a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c"
    }
  ],
  "annotations": {
    "org.opencontainers.image.source": "https://example.invalid/repo"
  }
}"#;

    /// An OCI image index — carries `manifests`, and carries **no** `layers`
    /// and no `config` at all.
    const OCI_INDEX: &str = r#"{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.index.v1+json",
  "manifests": [
    {
      "mediaType": "application/vnd.oci.image.manifest.v1+json",
      "size": 1247,
      "digest": "sha256:0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0",
      "platform": { "architecture": "amd64", "os": "linux" }
    },
    {
      "mediaType": "application/vnd.oci.image.manifest.v1+json",
      "size": 1247,
      "digest": "sha256:1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f809",
      "platform": { "architecture": "arm64", "os": "linux", "variant": "v8" }
    }
  ]
}"#;

    #[test]
    fn a_docker_push_image_manifest_parses() {
        let parsed = ParsedManifest::parse(DOCKER_V2S2_IMAGE.as_bytes())
            .expect("the wire format of a real `docker push` must parse");

        assert_eq!(parsed.manifest.schema_version, 2);
        assert_eq!(
            parsed.manifest.media_type.as_deref(),
            Some(super::super::types::media_types::MANIFEST_V2)
        );
        assert!(!parsed.is_manifest_list());
        assert_eq!(
            parsed.config_digest(),
            Some("sha256:2b8fd9751c4c0f5dd266fcae00707e67c2c19f0b16b8e9e1b6d8b6d7d3c6a1b2")
        );
        // Config + every layer must be reachable, or `put_manifest` would let a
        // manifest through that references blobs it never checked.
        assert_eq!(
            parsed.referenced_blobs(),
            vec![
                "sha256:2b8fd9751c4c0f5dd266fcae00707e67c2c19f0b16b8e9e1b6d8b6d7d3c6a1b2",
                "sha256:31603596830fc7e56753139f9c2c6bd3759e48a850659506ebfb885d0c2a3a1e",
            ]
        );
    }

    #[test]
    fn an_oci_image_manifest_parses() {
        let parsed =
            ParsedManifest::parse(OCI_IMAGE.as_bytes()).expect("OCI image manifest must parse");

        assert_eq!(
            parsed.manifest.media_type.as_deref(),
            Some(super::super::types::media_types::OCI_MANIFEST_V1)
        );
        assert!(!parsed.is_manifest_list());
        assert_eq!(parsed.manifest.layers.len(), 1);
        assert_eq!(
            parsed
                .manifest
                .annotations
                .get("org.opencontainers.image.source"),
            Some(&"https://example.invalid/repo".to_string())
        );
    }

    #[test]
    fn an_oci_index_parses_without_layers_or_config() {
        let parsed =
            ParsedManifest::parse(OCI_INDEX.as_bytes()).expect("OCI image index must parse");

        assert!(parsed.is_manifest_list());
        assert_eq!(parsed.manifest.manifests.len(), 2);
        assert!(parsed.manifest.layers.is_empty());
        assert_eq!(parsed.config_digest(), None);
        // An index references sub-manifests, not blobs — nothing for the blob
        // existence check to demand.
        assert!(parsed.referenced_blobs().is_empty());

        let arm = &parsed.manifest.manifests[1];
        let platform = arm.platform.as_ref().expect("platform is present");
        assert_eq!(platform.architecture, "arm64");
        assert_eq!(platform.os, "linux");
        assert_eq!(platform.variant.as_deref(), Some("v8"));
    }

    #[test]
    fn a_snake_case_body_is_not_the_wire_format() {
        // The shape this parser used to demand. No client ever sends it; keep
        // it rejected so a future rename can't silently swap the format back.
        let body = br#"{"schema_version":2,"media_type":"application/vnd.docker.distribution.manifest.v2+json","layers":[],"manifests":[]}"#;

        assert!(ParsedManifest::parse(body).is_err());
    }

    #[test]
    fn a_manifest_without_a_media_type_is_accepted() {
        // OCI Image Spec v1.0 makes `mediaType` optional; the HTTP layer has
        // already validated the Content-Type header.
        let body = br#"{"schemaVersion":2,"config":{"mediaType":"application/vnd.oci.image.config.v1+json","size":3,"digest":"sha256:aa"},"layers":[]}"#;

        let parsed =
            ParsedManifest::parse(body).expect("a missing mediaType is not a client error");
        assert_eq!(parsed.manifest.media_type, None);
    }

    #[test]
    fn an_unsupported_media_type_is_rejected() {
        let body = br#"{"schemaVersion":2,"mediaType":"application/vnd.example.nonsense+json","layers":[]}"#;

        let err =
            ParsedManifest::parse(body).expect_err("unknown manifest media types are refused");
        assert!(
            err.to_string().contains("unsupported manifest media type"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn an_unsupported_schema_version_is_rejected() {
        let body = br#"{"schemaVersion":1,"mediaType":"application/vnd.docker.distribution.manifest.v2+json","layers":[]}"#;

        let err = ParsedManifest::parse(body).expect_err("only schema version 2 is supported");
        assert!(
            err.to_string().contains("unsupported schema version"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn the_digest_is_computed_over_the_raw_bytes() {
        let parsed = ParsedManifest::parse(DOCKER_V2S2_IMAGE.as_bytes()).unwrap();
        let expected = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(DOCKER_V2S2_IMAGE.as_bytes()))
        );

        assert_eq!(parsed.digest, expected);
        assert_eq!(parsed.size, DOCKER_V2S2_IMAGE.len() as u64);
        assert_eq!(parsed.raw_json, DOCKER_V2S2_IMAGE.as_bytes());
    }

    #[test]
    fn serialization_round_trips_through_the_wire_format() {
        // `Manifest` also derives Serialize; it must emit the same camelCase
        // names it reads, not the Rust field names.
        let parsed = ParsedManifest::parse(DOCKER_V2S2_IMAGE.as_bytes()).unwrap();
        let json = serde_json::to_string(&parsed.manifest).unwrap();

        assert!(json.contains("\"schemaVersion\""), "got: {json}");
        assert!(json.contains("\"mediaType\""), "got: {json}");
        assert!(!json.contains("schema_version"), "got: {json}");

        ParsedManifest::parse(json.as_bytes()).expect("our own output must parse back");
    }
}
