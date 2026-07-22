//! Docker / OCI adapter for the package registry.
//!
//! Provides a minimal PackageAdapter implementation for the "docker" package type.
//! Actual Docker push/pull flows through the `/v2/` OCI API, not this adapter.

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct DockerAdapter;

impl PackageAdapter for DockerAdapter {
    fn package_type() -> &'static str {
        "docker"
    }

    fn extract_metadata(&self, _filename: &str, _data: &[u8]) -> anyhow::Result<ExtractedMetadata> {
        // Docker images are pushed via OCI Distribution API, not via the package upload flow.
        // This adapter serves as a registry type marker.
        anyhow::bail!(
            "Docker images must be pushed via the OCI v2 API, not the package upload endpoint"
        )
    }

    fn validate(&self, _data: &[u8]) -> anyhow::Result<()> {
        // OCI manifest validation happens in the /v2/ API handler
        Ok(())
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        if filename.ends_with(".tar.gz") || filename.ends_with(".tgz") {
            "application/vnd.docker.image.rootfs.diff.tar.gzip".into()
        } else if filename.ends_with(".json") {
            "application/json".into()
        } else {
            "application/octet-stream".into()
        }
    }

    fn default_content_type(&self) -> &'static str {
        "application/octet-stream"
    }

    fn has_protocol_endpoint(&self) -> bool {
        true // Docker uses the /v2/ protocol endpoints
    }
}
