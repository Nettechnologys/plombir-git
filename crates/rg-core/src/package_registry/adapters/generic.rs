//! Generic / fallback package adapter.
//!
//! Handles any package type without special parsing.  Metadata must be
//! supplied by the publisher (via query params / CLI flags).  The adapter
//! performs no format-specific validation beyond checking that data is
//! non-empty.

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct GenericAdapter;

impl PackageAdapter for GenericAdapter {
    fn package_type() -> &'static str {
        "generic"
    }

    fn extract_metadata(
        &self,
        _filename: &str,
        _data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        // Generic packages don't have embedded metadata — the caller
        // provides it.  Return empty/defaults so the service falls
        // back to query-param-provided values.
        Ok(ExtractedMetadata {
            name: String::new(),
            version: String::new(),
            description: None,
            homepage: None,
            repository_url: None,
            keywords: None,
            license: None,
            semver: None,
        })
    }

    fn validate(&self, data: &[u8]) -> Result<(), anyhow::Error> {
        if data.is_empty() {
            anyhow::bail!("empty file body");
        }
        Ok(())
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "json" => "application/json".into(),
            "xml" => "application/xml".into(),
            "html" | "htm" => "text/html".into(),
            "txt" => "text/plain".into(),
            "gz" | "tgz" => "application/gzip".into(),
            "tar" => "application/x-tar".into(),
            "zip" => "application/zip".into(),
            "wasm" => "application/wasm".into(),
            _ => "application/octet-stream".into(),
        }
    }
}
