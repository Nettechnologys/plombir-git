//! PyPI (Python) package adapter.
//!
//! Handles two package formats:
//! - **Wheel (.whl)**: ZIP archive containing `{name}-{version}.dist-info/METADATA`
//! - **Source distribution (.tar.gz)**: tar.gz containing `{name}-{version}/PKG-INFO`
//!
//! Metadata is in RFC 822-style email header format (PEP 314 / PEP 566).
//!
//! ## Simple Repository API (PEP 503)
//!
//! PyPI clients (pip, poetry, uv) expect HTML at:
//!   `GET /simple/{package}/`
//!
//! Returns an HTML page with `<a>` links to each version's download URL:
//! ```html
//! <!DOCTYPE html>
//! <html><body>
//!   <a href="https://.../mypkg-1.0.0.tar.gz#sha256=...">mypkg-1.0.0.tar.gz</a>
//!   <a href="https://.../mypkg-1.1.0-py3-none-any.whl#sha256=...">mypkg-1.1.0-py3-none-any.whl</a>
//! </body></html>
//! ```
//!
//! IronForge serves this at:
//!   `GET /api/v1/repos/{owner}/{repo}/packages/pypi/simple/{pkg_name}`

use flate2::read::GzDecoder;
use std::io::{Cursor, Read};

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct PyPIAdapter;

impl PackageAdapter for PyPIAdapter {
    fn package_type() -> &'static str {
        "pypi"
    }

    fn extract_metadata(
        &self,
        filename: &str,
        data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        let filename_lower = filename.to_lowercase();

        if filename_lower.ends_with(".whl") {
            extract_from_whl(data)
        } else if filename_lower.ends_with(".tar.gz") || filename_lower.ends_with(".tgz") {
            extract_from_sdist(data)
        } else {
            // Try wheel first (ZIP magic), then sdist
            if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
                extract_from_whl(data)
            } else if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
                extract_from_sdist(data)
            } else {
                anyhow::bail!(
                    "unrecognized PyPI package format: expected .whl (ZIP) or .tar.gz (gzip); got '{}'",
                    filename
                )
            }
        }
    }

    fn validate(&self, data: &[u8]) -> Result<(), anyhow::Error> {
        // Check for wheel (ZIP magic: PK\x03\x04)
        if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
            validate_whl(data)
        } else if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
            validate_sdist(data)
        } else {
            anyhow::bail!("invalid PyPI package: not a recognized format (expect .whl or .tar.gz)")
        }
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        let lower = filename.to_lowercase();
        if lower.ends_with(".whl") {
            "application/zip".into()
        } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            "application/gzip".into()
        } else {
            self.default_content_type().into()
        }
    }

    fn default_content_type(&self) -> &'static str {
        "application/octet-stream"
    }

    fn has_protocol_endpoint(&self) -> bool {
        true
    }
}

/// Parse RFC 822-style metadata (e.g., METADATA / PKG-INFO).
/// Fields: Name, Version, Summary, Home-page, License, Keywords, etc.
fn parse_rfc822_meta(content: &str) -> Result<ExtractedMetadata, anyhow::Error> {
    let mut name = String::new();
    let mut version = String::new();
    let mut description = None;
    let mut homepage = None;
    let mut license = None;
    let mut keywords = None;
    let mut current_field = String::new();
    let mut current_value = String::new();
    let mut in_continuation = false;

    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation line (folded header per RFC 822)
            if in_continuation {
                current_value.push(' ');
                current_value.push_str(line.trim());
            }
            continue;
        }

        // Save previous field if any
        if !current_field.is_empty() {
            save_field(
                &current_field,
                &current_value,
                &mut name,
                &mut version,
                &mut description,
                &mut homepage,
                &mut license,
                &mut keywords,
            );
            current_field.clear();
            current_value.clear();
        }

        // Parse new header line
        if let Some(colon_pos) = line.find(':') {
            current_field = line[..colon_pos].trim().to_lowercase();
            current_value = line[colon_pos + 1..].trim().to_string();
            in_continuation = true;
        } else if line.is_empty() {
            in_continuation = false;
        }
    }

    // Save last field
    if !current_field.is_empty() {
        save_field(
            &current_field,
            &current_value,
            &mut name,
            &mut version,
            &mut description,
            &mut homepage,
            &mut license,
            &mut keywords,
        );
    }

    if name.is_empty() {
        anyhow::bail!("metadata missing 'Name' field");
    }
    if version.is_empty() {
        anyhow::bail!("metadata missing 'Version' field");
    }

    Ok(ExtractedMetadata {
        name,
        version: version.clone(),
        description,
        homepage,
        repository_url: None, // PyPI metadata has no standard repository field
        keywords,
        license,
        semver: Some(version),
    })
}

#[allow(clippy::too_many_arguments)]
fn save_field(
    field: &str,
    value: &str,
    name: &mut String,
    version: &mut String,
    description: &mut Option<String>,
    homepage: &mut Option<String>,
    license: &mut Option<String>,
    keywords: &mut Option<String>,
) {
    match field {
        "name" => *name = value.to_string(),
        "version" => *version = value.to_string(),
        "summary" => *description = Some(value.to_string()),
        "description" => {
            // If we already have a summary, keep it (summary is shorter/better)
            if description.is_none() {
                // Truncate long description
                let desc = if value.len() > 500 {
                    format!("{}...", &value[..500])
                } else {
                    value.to_string()
                };
                *description = Some(desc);
            }
        }
        "home-page" | "homepage" | "project-url" | "url" => {
            *homepage = Some(value.to_string());
        }
        "license" => *license = Some(value.to_string()),
        "keywords" => *keywords = Some(value.to_string()),
        _ => {}
    }
}

/// Extract metadata from a .whl (ZIP) file.
fn extract_from_whl(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| anyhow::anyhow!("invalid .whl file (not a valid ZIP): {e}"))?;

    let mut metadata_content = None;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| anyhow::anyhow!("failed to read .whl entry {}: {e}", i))?;
        let path = entry.name().to_lowercase();

        // Look for *.dist-info/METADATA
        if path.ends_with(".dist-info/metadata") || path.ends_with(".dist-info\\metadata") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            metadata_content = Some(content);
            break;
        }
    }

    let content = metadata_content
        .ok_or_else(|| anyhow::anyhow!("invalid .whl file: no .dist-info/METADATA found"))?;

    parse_rfc822_meta(&content)
}

/// Extract metadata from a source distribution (.tar.gz).
fn extract_from_sdist(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let tar = GzDecoder::new(data);
    let mut archive = tar::Archive::new(tar);

    let mut pkg_info = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();

        // PKG-INFO is in the top-level directory: {name}-{version}/PKG-INFO
        if path.file_name().map(|n| n == "PKG-INFO").unwrap_or(false) {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            pkg_info = Some(content);
            break;
        }
    }

    let content = pkg_info
        .ok_or_else(|| anyhow::anyhow!("invalid source distribution: no PKG-INFO found"))?;

    parse_rfc822_meta(&content)
}

/// Validate a .whl file (check ZIP structure and METADATA presence).
fn validate_whl(data: &[u8]) -> Result<(), anyhow::Error> {
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| anyhow::anyhow!("invalid .whl file (not a valid ZIP): {e}"))?;

    let mut found_metadata = false;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| anyhow::anyhow!("failed to read .whl entry: {e}"))?;
        let path = entry.name().to_lowercase();
        if path.ends_with(".dist-info/metadata") || path.ends_with(".dist-info\\metadata") {
            found_metadata = true;
            break;
        }
    }

    if !found_metadata {
        anyhow::bail!("invalid .whl file: no .dist-info/METADATA found");
    }
    Ok(())
}

/// Validate a source distribution (check tar.gz + PKG-INFO presence).
fn validate_sdist(data: &[u8]) -> Result<(), anyhow::Error> {
    // Check gzip
    let mut decoder = GzDecoder::new(data);
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .map_err(|e| anyhow::anyhow!("invalid source distribution (not valid gzip): {e}"))?;

    // Check PKG-INFO presence
    let tar = GzDecoder::new(data);
    let mut archive = tar::Archive::new(tar);
    let mut found = false;
    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        if path.file_name().map(|n| n == "PKG-INFO").unwrap_or(false) {
            found = true;
            break;
        }
    }
    if !found {
        anyhow::bail!("invalid source distribution: PKG-INFO not found");
    }
    Ok(())
}

// ── Simple Repository API helpers ─────────────────────────

/// Generate the Simple Repository API HTML page (PEP 503).
///
/// `versions` is a list of (version, filename, sha256, download_url).
pub fn build_simple_repository_html(package_name: &str, versions: &[PyPIVersionEntry]) -> String {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n");
    html.push_str(&format!(
        "<title>Simple index for {}</title>\n",
        package_name
    ));
    html.push_str("<meta name=\"api-version\" content=\"2\" />\n");
    html.push_str("</head>\n<body>\n");

    for entry in versions {
        let sha_frag = entry
            .sha256
            .as_ref()
            .map(|s| format!("#sha256={}", s))
            .unwrap_or_default();
        html.push_str(&format!(
            "  <a href=\"{}{}\">{}</a><br/>\n",
            entry.download_url, sha_frag, entry.filename,
        ));
    }

    html.push_str("</body>\n</html>\n");
    html
}

/// Info for each version entry in the Simple Repository HTML.
pub struct PyPIVersionEntry {
    pub version: String,
    pub filename: String,
    pub sha256: Option<String>,
    pub download_url: String,
}
