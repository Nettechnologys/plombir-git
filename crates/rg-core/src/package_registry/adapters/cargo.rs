//! Cargo (Rust) package adapter.
//!
//! Handles `.crate` files: tar.gz archives containing a `Cargo.toml` manifest.
//!
//! ## Sparse index protocol
//!
//! Cargo ≥ 1.68 uses the "sparse index" protocol: a GET to
//! `{registry}/index/{name}` returns line-delimited JSON with one entry
//! per version.  IronForge serves this at:
//!   `GET /api/v1/repos/{owner}/{repo}/packages/cargo/index/{pkg_name}`

use flate2::read::GzDecoder;
use std::io::Read;
use tar::Archive;

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct CargoAdapter;

impl PackageAdapter for CargoAdapter {
    fn package_type() -> &'static str {
        "cargo"
    }

    fn extract_metadata(
        &self,
        _filename: &str,
        data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        let tar = GzDecoder::new(data);
        let mut archive = Archive::new(tar);

        let mut cargo_toml = None;

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_path_buf();

            // The .crate file contains `{name}-{version}/Cargo.toml`
            if path.file_name().map(|n| n == "Cargo.toml").unwrap_or(false) {
                let mut contents = String::new();
                entry.read_to_string(&mut contents)?;
                cargo_toml = Some(contents);
                break;
            }
        }

        let toml_str = cargo_toml.ok_or_else(|| {
            anyhow::anyhow!("invalid .crate file: no Cargo.toml found in archive")
        })?;

        // Parse minimal TOML — we only need [package] fields.
        let doc: toml::Value =
            toml::from_str(&toml_str).map_err(|e| anyhow::anyhow!("invalid Cargo.toml: {e}"))?;

        let pkg = doc
            .get("package")
            .ok_or_else(|| anyhow::anyhow!("Cargo.toml missing [package] section"))?;

        let name = pkg
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Cargo.toml missing package.name"))?
            .to_string();

        let version = pkg
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Cargo.toml missing package.version"))?
            .to_string();

        let description = pkg
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from);
        let homepage = pkg
            .get("homepage")
            .and_then(|v| v.as_str())
            .map(String::from);
        let repository_url = pkg
            .get("repository")
            .and_then(|v| v.as_str())
            .map(String::from);
        let license = pkg
            .get("license")
            .and_then(|v| v.as_str())
            .map(String::from);
        let keywords = pkg.get("keywords").and_then(|v| {
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
        // Check that it's a valid gzip stream
        let mut decoder = GzDecoder::new(data);
        let mut buf = Vec::new();
        decoder
            .read_to_end(&mut buf)
            .map_err(|e| anyhow::anyhow!("invalid .crate file (not valid gzip): {e}"))?;

        // Check that Cargo.toml exists
        let tar = GzDecoder::new(data);
        let mut archive = Archive::new(tar);
        let mut found = false;
        for entry in archive.entries()? {
            let entry = entry?;
            if entry
                .path()?
                .file_name()
                .map(|n| n == "Cargo.toml")
                .unwrap_or(false)
            {
                found = true;
                break;
            }
        }
        if !found {
            anyhow::bail!("invalid .crate file: Cargo.toml not found");
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

/// Build a sparse-index line for a version entry.
///
/// Cargo expects one JSON object per line, like:
/// ```json
/// {"name":"mycrate","vers":"0.1.0","deps":[],"cksum":"...","features":{},"yanked":false,"links":null}
/// ```
pub fn build_sparse_index_entry(
    name: &str,
    version: &str,
    sha256: Option<&str>,
    yanked: bool,
) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "vers": version,
        "deps": [],
        "cksum": sha256.unwrap_or(""),
        "features": {},
        "yanked": yanked,
        "links": serde_json::Value::Null,
    })
}

/// Build the full sparse-index response: one JSON line per version.
pub fn build_sparse_index(name: &str, versions: &[(&str, Option<&str>, bool)]) -> String {
    let mut lines = String::new();
    for (ver, sha256, yanked) in versions {
        let entry = build_sparse_index_entry(name, ver, *sha256, *yanked);
        lines.push_str(&serde_json::to_string(&entry).unwrap_or_default());
        lines.push('\n');
    }
    lines
}
