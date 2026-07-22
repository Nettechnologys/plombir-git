//! npm (Node.js) package adapter.
//!
//! Handles `.tgz` / `.tar.gz` archives containing a `package/package.json`.
//!
//! ## npm registry API
//!
//! npm expects a JSON response at `GET /{pkg_name}` with at minimum:
//! ```json
//! {
//!   "name": "my-pkg",
//!   "dist-tags": { "latest": "1.0.0" },
//!   "versions": {
//!     "1.0.0": {
//!       "name": "my-pkg",
//!       "version": "1.0.0",
//!       "dist": {
//!         "shasum": "...",
//!         "tarball": "https://..."
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! IronForge serves this at:
//!   `GET /api/v1/repos/{owner}/{repo}/packages/npm/{pkg_name}`

use flate2::read::GzDecoder;
use std::io::Read;
use tar::Archive;

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct NpmAdapter;

impl PackageAdapter for NpmAdapter {
    fn package_type() -> &'static str {
        "npm"
    }

    fn extract_metadata(
        &self,
        _filename: &str,
        data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        let tar = GzDecoder::new(data);
        let mut archive = Archive::new(tar);

        let mut package_json = None;

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_path_buf();

            // npm packs as `package/package.json`
            let is_package_json = path.components().any(|c| c.as_os_str() == "package")
                && path
                    .file_name()
                    .map(|n| n == "package.json")
                    .unwrap_or(false);

            // Also support top-level package.json (less common)
            let is_top_level = path
                .file_name()
                .map(|n| n == "package.json")
                .unwrap_or(false);

            if is_package_json || is_top_level {
                let mut contents = String::new();
                entry.read_to_string(&mut contents)?;
                package_json = Some(contents);
                // Prefer the nested one; if we found it, stop.
                if is_package_json {
                    break;
                }
            }
        }

        let json_str = package_json.ok_or_else(|| {
            anyhow::anyhow!("invalid npm package: no package.json found in archive")
        })?;

        let doc: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| anyhow::anyhow!("invalid package.json: {e}"))?;

        let name = doc
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("package.json missing name"))?
            .to_string();

        let version = doc
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("package.json missing version"))?
            .to_string();

        let description = doc
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from);
        let homepage = doc
            .get("homepage")
            .and_then(|v| v.as_str())
            .map(String::from);

        // npm uses `repository` as an object or string
        let repository_url = doc.get("repository").and_then(|v| {
            v.as_str()
                .map(String::from)
                .or_else(|| v.get("url").and_then(|u| u.as_str()).map(String::from))
        });

        let license = doc.get("license").and_then(|v| {
            v.as_str()
                .map(String::from)
                .or_else(|| v.get("type").and_then(|t| t.as_str()).map(String::from))
        });

        let keywords = doc.get("keywords").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
        });

        Ok(ExtractedMetadata {
            name,
            version: version.clone(),
            description,
            homepage,
            repository_url,
            keywords,
            license,
            semver: Some(version),
        })
    }

    fn validate(&self, data: &[u8]) -> Result<(), anyhow::Error> {
        // Check gzip
        let mut decoder = GzDecoder::new(data);
        let mut buf = Vec::new();
        decoder
            .read_to_end(&mut buf)
            .map_err(|e| anyhow::anyhow!("invalid npm package (not valid gzip): {e}"))?;

        // Check package.json presence
        let tar = GzDecoder::new(data);
        let mut archive = Archive::new(tar);
        let mut found = false;
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?;
            let is_pkg_json = path
                .file_name()
                .map(|n| n == "package.json")
                .unwrap_or(false);
            if is_pkg_json {
                found = true;
                break;
            }
        }
        if !found {
            anyhow::bail!("invalid npm package: package.json not found");
        }
        Ok(())
    }

    fn content_type_for_file(&self, _filename: &str) -> String {
        "application/gzip".into()
    }

    fn default_content_type(&self) -> &'static str {
        "application/gzip"
    }

    fn has_protocol_endpoint(&self) -> bool {
        true
    }
}

/// Build the npm registry "abbreviated" metadata JSON response.
///
/// This is the format npm expects when querying a registry.
pub fn build_npm_metadata(
    name: &str,
    versions: &[NpmVersionInfo],
    base_url: &str,
    owner: &str,
    repo: &str,
) -> serde_json::Value {
    let mut versions_map = serde_json::Map::new();
    let mut latest_version: Option<String> = None;

    for vi in versions {
        if latest_version.is_none() && !vi.yanked {
            latest_version = Some(vi.version.clone());
        }

        let tarball_url = format!(
            "{}/api/v1/repos/{}/{}/packages/npm/{}/{}/{}",
            base_url.trim_end_matches('/'),
            owner,
            repo,
            name,
            vi.version,
            vi.filename.as_deref().unwrap_or("package.tgz"),
        );

        let mut ver_obj = serde_json::Map::new();
        ver_obj.insert("name".into(), name.into());
        ver_obj.insert("version".into(), vi.version.clone().into());
        ver_obj.insert(
            "description".into(),
            vi.description.clone().unwrap_or_default().into(),
        );
        ver_obj.insert(
            "dist".into(),
            serde_json::json!({
                "shasum": vi.sha256.clone().unwrap_or_default(),
                "tarball": tarball_url,
            }),
        );

        versions_map.insert(vi.version.clone(), serde_json::Value::Object(ver_obj));
    }

    let latest = latest_version.unwrap_or_else(|| "0.0.0".into());

    serde_json::json!({
        "name": name,
        "dist-tags": { "latest": latest },
        "versions": versions_map,
    })
}

/// Info needed for each version in the npm metadata response.
pub struct NpmVersionInfo {
    pub version: String,
    pub description: Option<String>,
    pub sha256: Option<String>,
    pub filename: Option<String>,
    pub yanked: bool,
}
