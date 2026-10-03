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
//! Plombir Git serves the directory listing at:
//!   `GET /api/v1/repos/{owner}/{repo}/packages/maven/{groupId}/{artifactId}/`

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};
use crate::package_registry::artifact::{read_manifest_to_string, PackageArtifact};

/// How much of an upload spelled `.pom` may be parsed as one.
///
/// A POM is the one Maven artifact whose entire payload *is* the manifest, so
/// it is the one that has to be read into memory to be understood. Bounding
/// that read is what keeps `PUT …/x.pom` from being a way to spend the whole
/// configured artifact ceiling of heap: the largest POMs published anywhere are
/// a few hundred kilobytes, and this is an order of magnitude above them.
const MAX_POM_BYTES: u64 = 4 * 1024 * 1024;

pub struct MavenAdapter;

impl PackageAdapter for MavenAdapter {
    fn package_type() -> &'static str {
        "maven"
    }

    fn extract_metadata(
        &self,
        filename: &str,
        artifact: &PackageArtifact,
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        let filename_lower = filename.to_lowercase();

        if filename_lower.ends_with(".pom") {
            extract_from_pom(artifact)
        } else if filename_lower.ends_with(".jar")
            || filename_lower.ends_with(".war")
            || filename_lower.ends_with(".aar")
            || filename_lower.ends_with(".ear")
        {
            // For binary files without an accompanying .pom, extract minimal
            // metadata from filename convention: {artifactId}-{version}.jar
            extract_from_filename(filename)
        } else if artifact.starts_with(&[0x1f, 0x8b])? {
            // May be a tar.gz bundle of Maven artifacts — try to find a .pom inside
            extract_from_tarball(artifact)
        } else {
            // Try POM XML detection
            let head = artifact.head(200)?;
            let preview = String::from_utf8_lossy(&head);
            if preview.contains("<project") || preview.contains("<project ") {
                extract_from_pom(artifact)
            } else {
                extract_from_filename(filename)
            }
        }
    }

    fn validate(&self, artifact: &PackageArtifact) -> Result<(), anyhow::Error> {
        // Check if it's a valid POM (XML), JAR (ZIP), or gzip
        if artifact.len() < 4 {
            anyhow::bail!("file too small to be a valid Maven artifact");
        }

        let head = artifact.head(200)?;
        let preview = String::from_utf8_lossy(&head);

        // POM XML
        if preview.contains("<project") || preview.contains("<?xml") {
            // A POM is the manifest, so finding its envelope is not enough:
            // publish may otherwise fall back to query/path coordinates after
            // metadata extraction fails. Binary Maven artifacts legitimately
            // have no manifest and continue through the magic-byte branches.
            extract_from_pom(artifact)?;
            return Ok(());
        }

        // ZIP magic (JAR/WAR/AAR)
        if artifact.starts_with(b"PK\x03\x04")? {
            return Ok(());
        }

        // gzip magic (tar.gz bundle)
        if artifact.starts_with(&[0x1f, 0x8b])? {
            return Ok(());
        }

        anyhow::bail!("unrecognized Maven artifact format")
    }

    /// A POM states coordinates, and Maven itself treats the repository layout
    /// as derived from them: a client fetches the POM by path and reads the
    /// `groupId:artifactId:version` inside it, so a POM stored somewhere other
    /// than its own coordinates breaks resolution at the client.
    ///
    /// This used to answer `false`, because one Maven version is several
    /// artifacts and only the `.pom` carries coordinates at all — the whole
    /// format was left permissive so `matrix-1.0.0-sources.jar`, which has no
    /// manifest, could still take its coordinates from the request. That is a
    /// property of the *extraction*, not of the format, and it is now recorded
    /// there: `extract_from_pom` sets `coordinates_from_manifest`,
    /// `extract_from_filename` does not (card_13cadc8a9d7a). The classifier
    /// artifact keeps working; the POM no longer publishes under a path that
    /// contradicts it.
    fn manifest_is_authoritative(&self) -> bool {
        true
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MavenPom {
    parent: Option<MavenParent>,
    group_id: Option<String>,
    artifact_id: Option<String>,
    version: Option<String>,
    description: Option<String>,
    url: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MavenParent {
    group_id: Option<String>,
    version: Option<String>,
}

/// Extract metadata from a POM (XML) file.
fn extract_from_pom(artifact: &PackageArtifact) -> Result<ExtractedMetadata, anyhow::Error> {
    if artifact.len() > MAX_POM_BYTES {
        anyhow::bail!(
            "invalid POM file: {} bytes is larger than the {MAX_POM_BYTES}-byte limit a POM is parsed under",
            artifact.len()
        );
    }
    let xml = String::from_utf8(artifact.to_bytes()?)
        .map_err(|e| anyhow::anyhow!("invalid POM file (not valid UTF-8): {e}"))?;
    parse_pom_xml(&xml)
}

/// Read the coordinates out of a POM document that is already in memory.
///
/// Split from [`extract_from_pom`] because a bundled POM arrives as a member of
/// a tarball rather than as the upload itself, and it has already been read
/// under the manifest bound by the time it gets here.
fn parse_pom_xml(xml: &str) -> Result<ExtractedMetadata, anyhow::Error> {
    let MavenPom {
        parent,
        group_id,
        artifact_id,
        version,
        description,
        url,
    } = quick_xml::de::from_str(xml).map_err(|e| anyhow::anyhow!("invalid POM XML: {e}"))?;

    // Only groupId and version are inherited by Maven. In particular, the
    // parent's artifactId names a different project and must never become the
    // package being published.
    let artifact_id = artifact_id.ok_or_else(|| anyhow::anyhow!("POM missing <artifactId>"))?;
    let group_id = group_id
        .or_else(|| parent.as_ref().and_then(|parent| parent.group_id.clone()))
        .ok_or_else(|| anyhow::anyhow!("POM missing <groupId>"))?;
    let version = version
        .or_else(|| parent.and_then(|parent| parent.version))
        .ok_or_else(|| anyhow::anyhow!("POM missing <version>"))?;

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
        protocol_metadata: None,
        // Read out of the POM, which is what makes it a manifest.
        coordinates_from_manifest: true,
    })
}

#[cfg(test)]
mod pom_tests {
    use super::parse_pom_xml;

    #[test]
    fn project_coordinates_win_over_parent_coordinates() {
        let metadata = parse_pom_xml(
            r#"<?xml version="1.0"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent>
    <groupId>org.parent</groupId>
    <artifactId>parent-bom</artifactId>
    <version>9.8.7</version>
  </parent>
  <groupId>com.example.tools</groupId>
  <artifactId>matrix-child</artifactId>
  <version>1.2.3</version>
</project>"#,
        )
        .expect("valid child POM");

        assert_eq!(metadata.name, "com.example.tools:matrix-child");
        assert_eq!(metadata.version, "1.2.3");
    }

    #[test]
    fn group_and_version_inherit_from_parent_but_artifact_id_does_not() {
        let metadata = parse_pom_xml(
            r#"<project>
  <parent>
    <groupId>org.parent</groupId>
    <artifactId>parent-bom</artifactId>
    <version>9.8.7</version>
  </parent>
  <artifactId>matrix-child</artifactId>
</project>"#,
        )
        .expect("parent supplies the inheritable coordinates");

        assert_eq!(metadata.name, "org.parent:matrix-child");
        assert_eq!(metadata.version, "9.8.7");

        let error = parse_pom_xml(
            r#"<project><parent>
  <groupId>org.parent</groupId>
  <artifactId>parent-bom</artifactId>
  <version>9.8.7</version>
</parent></project>"#,
        )
        .expect_err("a project cannot inherit its artifactId");
        assert!(error.to_string().contains("<artifactId>"), "{error:#}");
    }
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
        protocol_metadata: None,
        // Guessed from `{artifactId}-{version}.{ext}` — a naming convention,
        // not a declaration. A sources/javadoc jar has nothing else to go on.
        coordinates_from_manifest: false,
    })
}

/// Extract metadata from a tar.gz bundle (Maven assembly or reactor build).
fn extract_from_tarball(artifact: &PackageArtifact) -> Result<ExtractedMetadata, anyhow::Error> {
    let tar = flate2::read::GzDecoder::new(artifact.reader()?);
    let mut archive = tar::Archive::new(tar);

    let mut pom_content = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let path_str = path.to_string_lossy();

        // Look for .pom files
        if path_str.ends_with(".pom") && !path_str.contains("target/") {
            pom_content = Some(read_manifest_to_string(&mut entry, "the bundled .pom")?);
            break;
        }
    }

    let xml =
        pom_content.ok_or_else(|| anyhow::anyhow!("no .pom file found in Maven tar.gz bundle"))?;

    parse_pom_xml(&xml)
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

    // Maven defines `latest` as the last version added, including snapshots,
    // and `release` as the last non-snapshot added. The caller supplies the
    // deterministic newest-publication-first order from the database; these
    // are intentionally not ComparableVersion maxima.
    if let Some(latest) = versions.first() {
        xml.push_str(&format!(
            "    <latest>{}</latest>\n",
            escape_xml(&latest.version)
        ));
    }
    if let Some(release) = versions.iter().find(|v| !v.is_snapshot) {
        xml.push_str(&format!(
            "    <release>{}</release>\n",
            escape_xml(&release.version)
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

    if let Some(last) = versions
        .iter()
        .max_by(|left, right| left.updated.cmp(&right.updated))
    {
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

#[cfg(test)]
mod metadata_tests {
    use super::{build_maven_metadata_xml, MavenVersionEntry};

    fn entry(version: &str, is_snapshot: bool, updated: &str) -> MavenVersionEntry {
        MavenVersionEntry {
            version: version.into(),
            is_snapshot,
            updated: updated.into(),
        }
    }

    #[test]
    fn latest_and_release_follow_mavens_publication_contract() {
        let xml = build_maven_metadata_xml(
            "com.example",
            "matrix",
            &[
                entry("3.0-SNAPSHOT", true, "20260809120000"),
                entry("1.2.4", false, "20260809110000"),
                entry("2.0.0", false, "20260809100000"),
            ],
        );

        assert!(xml.contains("<latest>3.0-SNAPSHOT</latest>"), "{xml}");
        assert!(xml.contains("<release>1.2.4</release>"), "{xml}");
    }

    #[test]
    fn last_updated_is_the_maximum_timestamp_not_the_slice_tail() {
        let xml = build_maven_metadata_xml(
            "com.example",
            "matrix",
            &[
                entry("1.0.0", false, "20260809100000"),
                entry("2.0.0", false, "20260809130000"),
                entry("0.9.0", false, "20260809090000"),
            ],
        );

        assert!(
            xml.contains("<lastUpdated>20260809130000</lastUpdated>"),
            "{xml}"
        );
    }
}

/// The digest algorithms a Maven checksum sidecar carries.
///
/// `mvn deploy` uploads `<artifact>.sha1` and `<artifact>.md5` next to each
/// file, and a resolver fetches them back to verify what it downloaded. Neither
/// is stored: a checksum is a claim about bytes the registry already holds, so
/// it is verified on the way in and recomputed on the way out (card_11d8655a9cd8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MavenChecksum {
    Sha1,
    Md5,
}

impl MavenChecksum {
    /// The lowercase hex digest of `data`, spelled the way Maven writes it.
    pub fn hex(self, data: &[u8]) -> String {
        use sha1::Digest as _;
        match self {
            Self::Sha1 => hex::encode(sha1::Sha1::digest(data)),
            Self::Md5 => hex::encode(md5::Md5::digest(data)),
        }
    }

    /// The lowercase digest of a file, computed with a fixed-size read window.
    pub async fn hex_file(self, path: &std::path::Path) -> std::io::Result<String> {
        use tokio::io::AsyncReadExt as _;

        async fn digest_file<D>(path: &std::path::Path) -> std::io::Result<String>
        where
            D: sha1::Digest + Default,
        {
            let mut file = tokio::fs::File::open(path).await?;
            let mut digest = D::default();
            let mut buffer = vec![0_u8; 128 * 1024];
            loop {
                let read = file.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
            Ok(hex::encode(digest.finalize()))
        }

        match self {
            Self::Sha1 => digest_file::<sha1::Sha1>(path).await,
            Self::Md5 => digest_file::<md5::Md5>(path).await,
        }
    }

    /// `matrix-1.0.0.jar.sha1` → the file it describes and the algorithm.
    pub fn split_sidecar(filename: &str) -> Option<(&str, Self)> {
        for (suffix, algorithm) in [(".sha1", Self::Sha1), (".md5", Self::Md5)] {
            if let Some(target) = filename.strip_suffix(suffix) {
                if !target.is_empty() {
                    return Some((target, algorithm));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod checksum_tests {
    use super::MavenChecksum;

    #[test]
    fn sidecars_are_split_off_the_file_they_describe() {
        assert_eq!(
            MavenChecksum::split_sidecar("matrix-1.0.0.jar.sha1"),
            Some(("matrix-1.0.0.jar", MavenChecksum::Sha1))
        );
        assert_eq!(
            MavenChecksum::split_sidecar("matrix-1.0.0.pom.md5"),
            Some(("matrix-1.0.0.pom", MavenChecksum::Md5))
        );
        // An artifact is not a sidecar, and a bare suffix describes no file.
        assert_eq!(MavenChecksum::split_sidecar("matrix-1.0.0.jar"), None);
        assert_eq!(MavenChecksum::split_sidecar(".sha1"), None);
    }

    #[test]
    fn digests_are_the_ones_maven_writes() {
        // Known vectors for the empty input, so a swapped algorithm is visible.
        assert_eq!(
            MavenChecksum::Sha1.hex(b""),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            MavenChecksum::Md5.hex(b""),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
    }

    #[tokio::test]
    async fn file_digests_match_the_in_memory_algorithms_across_read_windows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("artifact.jar");
        let payload: Vec<u8> = (0..128 * 1024 * 2 + 37)
            .map(|index| (index % 251) as u8)
            .collect();
        tokio::fs::write(&path, &payload).await.unwrap();

        for algorithm in [MavenChecksum::Sha1, MavenChecksum::Md5] {
            assert_eq!(
                algorithm.hex_file(&path).await.unwrap(),
                algorithm.hex(&payload),
                "{algorithm:?} file digest diverged from its byte-slice contract"
            );
        }
    }
}
