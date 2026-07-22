//! OCI common types — shared between storage, manifest, and HTTP API.

use serde::{Deserialize, Serialize};

/// Parsed OCI manifest reference — either a tag or a digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Tag(String),
    Digest(String), // "sha256:..."
}

impl Reference {
    /// Parse a reference string. Returns `Tag` if it looks like a tag,
    /// `Digest` if it starts with "sha256:".
    pub fn parse(s: &str) -> Self {
        if s.starts_with("sha256:") {
            Reference::Digest(s.to_string())
        } else {
            Reference::Tag(s.to_string())
        }
    }

    /// `true` if this is a tag reference.
    pub fn is_tag(&self) -> bool {
        matches!(self, Reference::Tag(_))
    }

    /// `true` if this is a digest reference.
    pub fn is_digest(&self) -> bool {
        matches!(self, Reference::Digest(_))
    }
}

/// OCI tag listing response (RFC 7153).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagListResponse {
    pub name: String,
    pub tags: Vec<String>,
}

/// Error response body (RFC 7807 — Problem Details).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub errors: Vec<ErrorDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ErrorResponse {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            errors: vec![ErrorDetail {
                code: code.to_string(),
                message: message.to_string(),
                detail: None,
            }],
        }
    }
}

/// OCI Distribution API version check.
pub const API_VERSION_HEADER: &str = "Docker-Distribution-API-Version";
pub const API_VERSION: &str = "registry/2.0";

/// OCI mediatype constants.
pub mod media_types {
    // Docker V2 Schema 2
    pub const MANIFEST_V2: &str = "application/vnd.docker.distribution.manifest.v2+json";
    pub const MANIFEST_LIST_V2: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
    pub const CONFIG_V1: &str = "application/vnd.docker.container.image.v1+json";
    pub const LAYER_TAR_GZ: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

    // OCI Image Spec
    pub const OCI_MANIFEST_V1: &str = "application/vnd.oci.image.manifest.v1+json";
    pub const OCI_INDEX_V1: &str = "application/vnd.oci.image.index.v1+json";
    pub const OCI_CONFIG_V1: &str = "application/vnd.oci.image.config.v1+json";
    pub const OCI_LAYER_TAR_GZ: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

    /// Known manifest media types (for accept header validation).
    pub const MANIFEST_TYPES: &[&str] =
        &[MANIFEST_V2, MANIFEST_LIST_V2, OCI_MANIFEST_V1, OCI_INDEX_V1];
}

/// OCI error codes (per distribution spec).
pub mod error_codes {
    pub const BLOB_UNKNOWN: &str = "BLOB_UNKNOWN";
    pub const BLOB_UPLOAD_INVALID: &str = "BLOB_UPLOAD_INVALID";
    pub const BLOB_UPLOAD_UNKNOWN: &str = "BLOB_UPLOAD_UNKNOWN";
    pub const DIGEST_INVALID: &str = "DIGEST_INVALID";
    pub const MANIFEST_BLOB_UNKNOWN: &str = "MANIFEST_BLOB_UNKNOWN";
    pub const MANIFEST_INVALID: &str = "MANIFEST_INVALID";
    pub const MANIFEST_UNKNOWN: &str = "MANIFEST_UNKNOWN";
    pub const NAME_INVALID: &str = "NAME_INVALID";
    pub const NAME_UNKNOWN: &str = "NAME_UNKNOWN";
    pub const SIZE_INVALID: &str = "SIZE_INVALID";
    pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
    pub const UNSUPPORTED: &str = "UNSUPPORTED";
}
