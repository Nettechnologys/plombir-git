//! Composer (PHP) package adapter.
//!
//! Handles ZIP archives containing a `composer.json` manifest.
//!
//! ## Composer Repository Format
//!
//! Composer clients expect a `packages.json` root listing at:
//!   `GET /packages.json`
//!
//! Returns JSON with all available packages:
//! ```json
//! {
//!   "packages": {
//!     "vendor/pkg": {
//!       "1.0.0": { "name": "vendor/pkg", "version": "1.0.0", ... }
//!     }
//!   }
//! }
//! ```
//!
//! IronForge serves this at:
//!   `GET /api/v1/repos/{owner}/{repo}/packages/composer/packages.json`

use std::io::{Cursor, Read};

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};
use serde_json::Value;

pub struct ComposerAdapter;

impl PackageAdapter for ComposerAdapter {
    fn package_type() -> &'static str {
        "composer"
    }

    fn extract_metadata(&self, _filename: &str, data: &[u8]) -> anyhow::Result<ExtractedMetadata> {
        // Composer packages are ZIP archives with a composer.json at the root
        // or in a subdirectory named after the package
        let json_str = extract_composer_json(data)?;
        let json: Value = serde_json::from_str(&json_str)
            .map_err(|e| anyhow::anyhow!("invalid composer.json: {e}"))?;

        let name = json
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("composer.json missing 'name' field"))?
            .to_string();

        let version = json
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("composer.json missing 'version' field"))?
            .to_string();

        let description = json
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let homepage = json
            .get("homepage")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let license = json.get("license").and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else if let Some(arr) = v.as_array() {
                arr.iter()
                    .filter_map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
                    .into()
            } else {
                None
            }
        });
        let keywords = json.get("keywords").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        });

        // Repository URL from composer.json (support string or assoc array)
        let repository_url = json.get("repository").and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else if let Some(obj) = v.as_object() {
                obj.get("url")
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        });

        Ok(ExtractedMetadata {
            name,
            version,
            description,
            homepage,
            repository_url,
            keywords,
            license,
            semver: None,
        })
    }

    fn validate(&self, data: &[u8]) -> anyhow::Result<()> {
        // Check for ZIP magic bytes
        if data.len() < 4 || &data[0..4] != b"PK\x03\x04" {
            anyhow::bail!("invalid Composer package: not a valid ZIP archive");
        }
        // Try to extract and parse composer.json
        extract_composer_json(data)?;
        Ok(())
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        if filename.ends_with(".zip") {
            "application/zip".into()
        } else {
            self.default_content_type().into()
        }
    }

    fn has_protocol_endpoint(&self) -> bool {
        true // Serves packages.json for Composer metadata
    }
}

/// Extract the `composer.json` content from a ZIP archive.
///
/// Searches for `composer.json` at the archive root or in the first
/// subdirectory matching the Composer convention (`vendor/package/`).
fn extract_composer_json(data: &[u8]) -> anyhow::Result<String> {
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| anyhow::anyhow!("failed to open ZIP archive: {e}"))?;

    // First try: look for a top-level composer.json
    if let Ok(mut entry) = archive.by_name("composer.json") {
        let mut content = String::new();
        entry.read_to_string(&mut content)?;
        return Ok(content);
    }

    // Second try: look in a subdirectory (e.g., vendor/package/composer.json)
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| anyhow::anyhow!("failed to read ZIP entry: {e}"))?;
        let name = entry.name().to_string();

        // Match: any path ending with /composer.json
        if name.ends_with("/composer.json") {
            drop(entry);
            let mut entry = archive.by_index(i)?;
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            return Ok(content);
        }
    }

    anyhow::bail!("composer.json not found in archive (looked at root and subdirectories)")
}

/// Build the packages.json metadata for a list of Composer versions.
///
/// Returns a JSON string matching the Composer repository format:
/// ```json
/// { "packages": { "vendor/pkg": { "1.0.0": { ... }, ... } } }
/// ```
pub fn build_packages_json(
    package_name: &str,
    versions: &[ComposerVersionInfo],
    base_url: &str,
    owner: &str,
    repo: &str,
) -> String {
    let mut packages_map = serde_json::Map::new();
    let mut version_map = serde_json::Map::new();

    for v in versions {
        let download_url = format!(
            "{}/api/v1/repos/{}/{}/packages/composer/{}/{}/{}",
            base_url.trim_end_matches('/'),
            encode_path_segment(owner),
            encode_path_segment(repo),
            encode_path_segment(package_name),
            encode_path_segment(&v.version),
            encode_path_segment(&v.filename),
        );

        let mut entry = serde_json::Map::new();
        entry.insert("name".into(), package_name.to_string().into());
        entry.insert("version".into(), v.version.clone().into());
        entry.insert(
            "dist".into(),
            serde_json::json!({
                "type": "zip",
                "url": download_url,
                "reference": v.sha256.clone().unwrap_or_default(),
                "shasum": v.sha256.clone().unwrap_or_default(),
            }),
        );

        // Include download count as a custom metric
        entry.insert(
            "type".into(),
            v.package_type
                .clone()
                .unwrap_or_else(|| "library".to_string())
                .into(),
        );

        if let Some(desc) = &v.description {
            entry.insert("description".into(), desc.clone().into());
        }
        if let Some(license) = &v.license {
            entry.insert("license".into(), license.clone().into());
        }

        version_map.insert(v.version.clone(), Value::Object(entry));
    }

    packages_map.insert(package_name.to_string(), Value::Object(version_map));
    serde_json::to_string_pretty(&serde_json::json!({ "packages": Value::Object(packages_map) }))
        .unwrap_or_default()
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

/// Version info used by `build_packages_json`.
pub struct ComposerVersionInfo {
    pub version: String,
    pub filename: String,
    pub sha256: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub package_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reject_non_zip() {
        let adapter = ComposerAdapter;
        let err = adapter.validate(b"not a zip file").unwrap_err();
        assert!(err.to_string().contains("not a valid ZIP"));
    }

    #[test]
    fn test_extract_metadata_no_composer_json() {
        // Create an empty ZIP archive
        let cursor = Cursor::new(Vec::new());
        let zip = zip::ZipWriter::new(cursor);
        let finished = zip.finish().unwrap();

        let adapter = ComposerAdapter;
        let data = finished.into_inner();
        let err = adapter.extract_metadata("pkg.zip", &data).unwrap_err();
        assert!(err.to_string().contains("composer.json not found"));
    }

    #[test]
    fn test_build_packages_json() {
        let versions = vec![ComposerVersionInfo {
            version: "1.0.0".into(),
            filename: "vendor-pkg-1.0.0.zip".into(),
            sha256: Some("abc123".into()),
            description: Some("Test package".into()),
            license: Some("MIT".into()),
            package_type: Some("library".into()),
        }];
        let json = build_packages_json(
            "vendor/pkg",
            &versions,
            "https://forge.example",
            "owner",
            "repo",
        );
        assert!(json.contains("\"vendor/pkg\""));
        assert!(json.contains("\"1.0.0\""));
        assert!(json.contains("\"zip\""));
        assert!(json.contains("\"abc123\""));
    }
}
