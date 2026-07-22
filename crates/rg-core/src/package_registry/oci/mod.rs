//! OCI Container Registry module.
//!
//! Implements OCI Distribution Specification v1.0:
//! - Content-addressed blob storage
//! - Docker V2 Schema 2 and OCI Image Spec manifest handling
//! - Chunked upload support

pub mod manifest;
pub mod storage;
pub mod types;

pub use manifest::{Manifest, ManifestDescriptor, ManifestLayer, ParsedManifest};
pub use storage::OciStorage;
pub use types::{
    error_codes, media_types, ErrorDetail, ErrorResponse, Reference, TagListResponse, API_VERSION,
    API_VERSION_HEADER,
};
