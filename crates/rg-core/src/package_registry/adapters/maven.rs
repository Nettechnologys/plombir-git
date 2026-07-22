//! Maven (Java) package adapter.
//!
//! Handles Maven artifact uploads.  A Maven artifact consists of:
//! - A `.pom` file (Maven Project Object Model) with metadata
//! - One or more binary files (`.jar`, `.war`, `.aar`, `.ear`, etc.)
//! - Optional side artifacts (sources `.jar`, javadoc `.jar`)
//!
//! ## Maven metadata format
//!
//! The adapter parses `.pom` files to extract:
//! - `groupId` (from `<groupId>` or parent `<groupId>`)
//! - `artifactId` (from `<artifactId>`)
//! - `version` (from `<version>` or parent `<version>`)
//! - `name`, `description`, `url`, `licenses`, etc.
//!
//! ## Maven repository layout
//!
//! Maven clients expect a specific directory layout:
//!   `{groupId}/{artifactId}/{version}/`
//!
//! With files:
//!   `{artifactId}-{version}.pom`
//!   `{artifactId}-{version}.jar`
//!   `maven-metadata.xml`
//!
//! IronForge serves the directory listing at:
//!   `GET /api/v1/repos/{owner}/{repo}/packages/maven/{groupId}/{artifactId}/`

use std::io::Read;

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct MavenAdapter;

impl PackageAdapter for MavenAdapter {
    fn package_type() -> &'static str {
        "maven"
    }

    fn extract_metadata(
        &self,
        filename: &str,
        data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        let filename_lower = filename.to_lowercase();

        if filename_lower.ends_with(".pom") {
            extract_from_pom(data)
        } else if filename_lower.ends_with(".jar")
            || filename_lower.ends_with(".war")
            || filename_lower.ends_with(".aar")
            || filename_lower.ends_with(".ear")
        {
            // For binary files without an accompanying .pom, extract minimal
            // metadata from filename convention: {artifactId}-{version}.jar
            extract_from_filename(filename)
        } else if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
            // May be a tar.gz bundle of Maven artifacts — try to find a .pom inside
            extract_from_tarball(data)
        } else {
            // Try POM XML detection
            let preview =
                String::from_utf8_lossy(if data.len() > 200 { &data[..200] } else { data });
            if preview.contains("<project") || preview.contains("<project ") {
                extract_from_pom(data)
            } else {
                extract_from_filename(filename)
            }
        }
    }

    fn validate(&self, data: &[u8]) -> Result<(), anyhow::Error> {
        // Check if it's a valid POM (XML), JAR (ZIP), or gzip
        if data.len() < 4 {
            anyhow::bail!("file too small to be a valid Maven artifact");
        }

        let preview = String::from_utf8_lossy(if data.len() > 200 { &data[..200] } else { data });

        // POM XML
        if preview.contains("<project") || preview.contains("<?xml") {
            if !preview.contains("<artifactId>") {
                anyhow::bail!("invalid POM: missing <artifactId>");
            }
            return Ok(());
        }

        // ZIP magic (JAR/WAR/AAR)
        if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
            return Ok(());
        }

        // gzip magic (tar.gz bundle)
        if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
            return Ok(());
        }

        anyhow::bail!("unrecognized Maven artifact format")
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        let lower = filename.to_lowercase();
        if lower.ends_with(".pom") {
            "application/xml".into()
        } else if lower.ends_with(".jar") {
            "application/java-archive".into()
        } else if lower.ends_with(".war") {
            "application/x-webarchive".into()
        } else if lower.ends_with(".aar") {
            "application/octet-stream".into()
        } else if lower.ends_with(".ear") {
            "application/x-ear".into()
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

/// Quick XML tag extraction.  Not a full XML parser — handles Maven POM files.
fn xml_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);

    if let Some(start) = xml.find(&open) {
        let start = start + open.len();
        if let Some(end) = xml[start..].find(&close) {
            return Some(xml[start..start + end].trim().to_string());
        }
    }
    None
}

/// Extract metadata from a POM (XML) file.
fn extract_from_pom(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let xml = String::from_utf8(data.to_vec())
        .map_err(|e| anyhow::anyhow!("invalid POM file (not valid UTF-8): {e}"))?;

    let artifact_id = xml_tag_value(&xml, "artifactId")
        .ok_or_else(|| anyhow::anyhow!("POM missing <artifactId>"))?;

    let group_id = xml_tag_value(&xml, "groupId").unwrap_or_else(|| {
        // Fallback: try from <parent>
        xml_tag_value(&xml[..xml.find("</parent>").unwrap_or(0)], "groupId")
            .unwrap_or_else(|| "unknown".to_string())
    });

    let version = xml_tag_value(&xml, "version")
        .or_else(|| {
            // Try from <parent>
            xml_tag_value(&xml[..xml.find("</parent>").unwrap_or(0)], "version")
        })
        .ok_or_else(|| anyhow::anyhow!("POM missing <version>"))?;

    let _name = xml_tag_value(&xml, "name").or_else(|| Some(artifact_id.clone()));

    let description = xml_tag_value(&xml, "description");
    let url = xml_tag_value(&xml, "url");

    // Maven uses {groupId}:{artifactId} as the package name
    let pkg_name = format!("{}:{}", group_id, artifact_id);

    Ok(ExtractedMetadata {
        name: pkg_name,
        version: version.clone(),
        description,
        homepage: url,
        repository_url: None, // Maven POMs have <scm><url> — skip for now
        keywords: None,
        license: None,
        semver: Some(version),
    })
}

/// Extract minimal metadata from Maven filename convention.
///
/// Maven artifacts follow: `{artifactId}-{version}.{ext}` or
/// `{artifactId}-{version}-{classifier}.{ext}`.
fn extract_from_filename(filename: &str) -> Result<ExtractedMetadata, anyhow::Error> {
    // Strip known extensions
    let stem = filename
        .strip_suffix(".jar")
        .or_else(|| filename.strip_suffix(".war"))
        .or_else(|| filename.strip_suffix(".aar"))
        .or_else(|| filename.strip_suffix(".ear"))
        .or_else(|| filename.strip_suffix(".pom"))
        .or_else(|| filename.strip_suffix(".module"))
        .unwrap_or(filename);

    // Try to find the version separator: the last `-{digits}`
    let name_parts: Vec<&str> = stem.rsplitn(2, '-').collect();
    if name_parts.len() < 2 {
        anyhow::bail!(
            "cannot parse Maven artifact name from '{}': expected {{name}}-{{version}}.{{ext}}",
            filename
        );
    }

    let version_candidate = name_parts[0];
    let name_candidate = name_parts[1];

    // Check if version part looks like a version (starts with digit or contains dots)
    let looks_like_version = version_candidate
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
        || version_candidate.contains('.')
        || version_candidate.contains("-SNAPSHOT");

    if !looks_like_version {
        anyhow::bail!("cannot determine version from filename '{}'", filename);
    }

    let artifact_id = name_candidate.to_string();
    let version = version_candidate.to_string();

    Ok(ExtractedMetadata {
        name: artifact_id,
        version: version.clone(),
        description: None,
        homepage: None,
        repository_url: None,
        keywords: None,
        license: None,
        semver: Some(version),
    })
}

/// Extract metadata from a tar.gz bundle (Maven assembly or reactor build).
fn extract_from_tarball(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let tar = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(tar);

    let mut pom_content = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let path_str = path.to_string_lossy();

        // Look for .pom files
        if path_str.ends_with(".pom") && !path_str.contains("target/") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            pom_content = Some(content);
            break;
        }
    }

    let xml =
        pom_content.ok_or_else(|| anyhow::anyhow!("no .pom file found in Maven tar.gz bundle"))?;

    extract_from_pom(xml.as_bytes())
}

// ── Maven directory listing API helpers ───────────────────

/// Generate `maven-metadata.xml` for a version directory.
///
/// This is the standard Maven metadata format used by Gradle and Maven
/// to resolve artifact versions.
pub fn build_maven_metadata_xml(
    group_id: &str,
    artifact_id: &str,
    versions: &[MavenVersionEntry],
) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<metadata>\n");
    xml.push_str(&format!("  <groupId>{}</groupId>\n", escape_xml(group_id)));
    xml.push_str(&format!(
        "  <artifactId>{}</artifactId>\n",
        escape_xml(artifact_id)
    ));
    xml.push_str("  <versioning>\n");

    if let Some(latest) = versions.iter().find(|v| !v.is_snapshot) {
        xml.push_str(&format!(
            "    <latest>{}</latest>\n",
            escape_xml(&latest.version)
        ));
        xml.push_str(&format!(
            "    <release>{}</release>\n",
            escape_xml(&latest.version)
        ));
    }

    xml.push_str("    <versions>\n");
    for entry in versions {
        xml.push_str(&format!(
            "      <version>{}</version>\n",
            escape_xml(&entry.version)
        ));
    }
    xml.push_str("    </versions>\n");

    if let Some(last) = versions.last() {
        xml.push_str(&format!(
            "    <lastUpdated>{}</lastUpdated>\n",
            escape_xml(&last.updated)
        ));
    }

    xml.push_str("  </versioning>\n");
    xml.push_str("</metadata>\n");
    xml
}

/// Info for each version entry in maven-metadata.xml.
pub struct MavenVersionEntry {
    pub version: String,
    pub is_snapshot: bool,
    pub updated: String,
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
