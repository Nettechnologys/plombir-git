//! OCI Manifest parsing and validation.
//!
//! Supports Docker V2 Schema 2 and OCI Image Spec v1 manifests.
//! Extracts layer digests for blob reference tracking.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Parsed manifest — generic over Docker V2 and OCI spec formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version (1 or 2)
    pub schema_version: u32,
    /// Media type
    pub media_type: String,
    /// Config layer digest + size
    pub config: Option<ManifestLayer>,
    /// Image/variant layers
    pub layers: Vec<ManifestLayer>,
    /// For manifest lists / image indexes: sub-manifests
    pub manifests: Vec<ManifestDescriptor>,
    /// Annotations (OCI spec)
    #[serde(default)]
    pub annotations: std::collections::HashMap<String, String>,
}

/// A layer reference (config, layer, or sub-manifest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestLayer {
    pub media_type: String,
    pub size: u64,
    pub digest: String,
}

/// A sub-manifest in an image index (manifest list).
#[derive(Debug, Clone, Serialize, Deserialize)]
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

        // Validate media type
        if !super::types::media_types::MANIFEST_TYPES.contains(&manifest.media_type.as_str()) {
            anyhow::bail!("unsupported manifest media type: {}", manifest.media_type);
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
