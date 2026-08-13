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
//! ForgeKeep serves this at:
//!   `GET /api/v1/repos/{owner}/{repo}/packages/composer/packages.json`

use std::io::{Cursor, Read};

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};
use crate::package_registry::url_path::encode_path_segment;
use rg_db::package_version_key::composer_version_normalized;
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

        // Everything a Composer client resolves and installs by lives inside
        // the zip and nowhere else — `packages.json` never opens an archive —
        // so it is lifted here, once, at publish.
        let protocol_metadata = Some(composer_protocol_metadata(&json, license.as_deref()));

        Ok(ExtractedMetadata {
            name,
            version,
            description,
            homepage,
            repository_url,
            keywords,
            license,
            semver: None,
            protocol_metadata,
        })
    }

    fn validate(&self, data: &[u8]) -> anyhow::Result<()> {
        // Check for ZIP magic bytes
        if data.len() < 4 || &data[0..4] != b"PK\x03\x04" {
            anyhow::bail!("invalid Composer package: not a valid ZIP archive");
        }
        // Read the manifest, don't merely pull the bytes out: `extract_composer_json`
        // returns a string and never parses it, so a `composer.json` of pure
        // garbage passed this gate the same way the other adapters' presence
        // checks did.
        //
        // The gate stops at "is this JSON" rather than running the full
        // extraction, unlike the other adapters: `composer.json` legitimately
        // omits `version` — Composer derives it from the VCS tag — and such a
        // package is published with the coordinates in the query string. A
        // stricter check here would reject the normal case.
        let json_str = extract_composer_json(data)?;
        serde_json::from_str::<Value>(&json_str)
            .map_err(|e| anyhow::anyhow!("invalid composer.json: {e}"))?;
        Ok(())
    }

    /// Extraction only succeeds when `composer.json` declares both `name` and
    /// `version`; an archive that leaves the version to the VCS tag fails it and
    /// takes the query-parameter path instead.
    fn manifest_is_authoritative(&self) -> bool {
        true
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

/// The `composer.json` keys a repository entry has to carry, because a Composer
/// client reads them from the repository and never from the archive.
///
/// `require` and its solver siblings (`conflict` / `replace` / `provide`) are
/// what the 2.x SAT solver resolves against — a package published without them
/// installs alone, and `composer require vendor/pkg` succeeds while leaving
/// every dependency out. `autoload` is what makes the installed files loadable
/// at all: Composer builds the autoloader from the repository metadata, so a
/// package whose entry has no `autoload` lands on disk with none of its classes
/// reachable. `type` picks the installer, and with it the directory the package
/// is unpacked into.
const PROTOCOL_FIELDS: [&str; 11] = [
    "type",
    "require",
    "require-dev",
    "conflict",
    "replace",
    "provide",
    "suggest",
    "autoload",
    "autoload-dev",
    "bin",
    "extra",
];

/// The `composer.json` sections a repository entry is expected to carry, as a
/// JSON object recorded at publish.
///
/// `require` is always written, so an empty table is a fact the registry knows
/// about the package rather than one it failed to look up. `license` is folded
/// in already normalized (`composer.json` allows a string or a list, and the
/// adapter joins the list) because there is no license column on a version row
/// to read it back from.
fn composer_protocol_metadata(doc: &Value, license: Option<&str>) -> String {
    let mut out = serde_json::Map::new();

    for field in PROTOCOL_FIELDS {
        if let Some(value) = doc.get(field) {
            if !value.is_null() {
                out.insert(field.to_string(), value.clone());
            }
        }
    }
    out.entry("require")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));

    if let Some(license) = license {
        out.insert("license".into(), license.into());
    }

    Value::Object(out).to_string()
}

/// Build the packages.json metadata for a list of Composer versions.
///
/// Returns a JSON string matching the Composer repository format:
/// ```json
/// { "packages": { "vendor/pkg": { "1.0.0": { ... }, ... } } }
/// ```
///
/// A stored metadata blob that cannot be read is an error rather than an absent
/// overlay: serving an entry with no `require` would not tell the client
/// "unknown", it would tell it the package depends on nothing — a resolution
/// that succeeds and an install that is missing every dependency.
pub fn build_packages_json(
    package_name: &str,
    versions: &[ComposerVersionInfo],
    base_url: &str,
    owner: &str,
    repo: &str,
) -> anyhow::Result<String> {
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
        // `ArrayLoader` otherwise normalizes `version` itself. Publishing the
        // result of the same parser that owns the DB identity makes the server's
        // uniqueness claim explicit and prevents client/server drift. Invalid
        // historical spellings have no verified normal form and keep the old
        // raw-only entry rather than receiving a fabricated identity.
        if let Some(normalized) = composer_version_normalized(&v.version) {
            entry.insert("version_normalized".into(), normalized.into());
        }

        let mut dist = serde_json::Map::new();
        dist.insert("type".into(), "zip".into());
        dist.insert("url".into(), download_url.into());
        // `reference` names nothing in particular — Composer treats it as an
        // opaque identity for the archive and builds its cache path from it —
        // so any stable per-file value will do.
        dist.insert(
            "reference".into(),
            v.sha256.clone().unwrap_or_default().into(),
        );
        // `shasum`, unlike `reference`, is a promise about an algorithm:
        // Composer's `FileDownloader` compares it against `hash_file('sha1')`
        // of what it downloaded and throws "The checksum verification of the
        // file failed" on a mismatch. A SHA-256 there fails every install. When
        // there is no SHA-1 on record the field is left out — Composer skips
        // the check when the key is absent, and a skipped check beats a
        // guaranteed-failing one.
        if let Some(sha1) = v.sha1.as_deref().filter(|value| !value.is_empty()) {
            dist.insert("shasum".into(), sha1.into());
        }
        entry.insert("dist".into(), Value::Object(dist));

        // `type` dispatches Composer's installer, and with it the directory the
        // package is unpacked into: `composer-plugin`, `wordpress-plugin` and
        // `drupal-module` each travel their own way, and `metapackage` has no
        // archive at all. Defaulting a plugin to `library` puts it in
        // `vendor/`, where nothing picks it up — so the manifest's own answer
        // wins, and the fallback applies only to a package published before the
        // adapter recorded one.
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

        // The manifest sections lifted at publish. They are applied last and
        // overwrite the fallbacks above — `type` and `license` recorded from
        // the package's own `composer.json` are the answer, the entries above
        // are only what the registry can say without one.
        if let Some(raw) = v.metadata.as_deref().filter(|raw| !raw.trim().is_empty()) {
            let stored: Value = serde_json::from_str(raw).map_err(|e| {
                anyhow::anyhow!(
                    "stored composer metadata for {package_name} {} is unreadable: {e}",
                    v.version
                )
            })?;
            let Value::Object(stored) = stored else {
                anyhow::bail!(
                    "stored composer metadata for {package_name} {} is not an object",
                    v.version
                );
            };
            for (key, value) in stored {
                entry.insert(key, value);
            }
        }
        // Never absent, whatever the row carried: to Composer a missing
        // `require` reads as "depends on nothing", not as "not recorded".
        entry
            .entry("require")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));

        version_map.insert(v.version.clone(), Value::Object(entry));
    }

    packages_map.insert(package_name.to_string(), Value::Object(version_map));
    Ok(serde_json::to_string_pretty(
        &serde_json::json!({ "packages": Value::Object(packages_map) }),
    )?)
}

/// Version info used by `build_packages_json`.
pub struct ComposerVersionInfo {
    pub version: String,
    pub filename: String,
    /// Hex SHA-256 of the archive. Published only as the opaque `dist.reference`
    /// — never as `shasum`, which Composer verifies as SHA-1.
    pub sha256: Option<String>,
    /// Hex SHA-1 of the archive, published as `dist.shasum`. `None` for a file
    /// stored before the registry recorded it, and the field is then omitted.
    pub sha1: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub package_type: Option<String>,
    /// The `composer.json` sections recorded at publish, as the JSON object
    /// `composer_protocol_metadata` wrote. `None` for a version stored before
    /// the adapter recorded any — its entry then falls back to the fields
    /// above and an empty `require`.
    pub metadata: Option<String>,
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
            sha1: Some("def456".into()),
            description: Some("Test package".into()),
            license: Some("MIT".into()),
            package_type: Some("library".into()),
            metadata: None,
        }];
        let json = build_packages_json(
            "vendor/pkg",
            &versions,
            "https://forge.example",
            "owner",
            "repo",
        )
        .unwrap();
        assert!(json.contains("\"vendor/pkg\""));
        assert!(json.contains("\"1.0.0\""));
        assert!(json.contains("\"version_normalized\": \"1.0.0.0\""));
        assert!(json.contains("\"zip\""));
        assert!(json.contains("\"abc123\""));
    }

    #[test]
    fn an_invalid_legacy_version_keeps_its_raw_index_identity() {
        let versions = vec![ComposerVersionInfo {
            version: "legacy row".into(),
            filename: "legacy.zip".into(),
            sha256: None,
            sha1: None,
            description: None,
            license: None,
            package_type: None,
            metadata: None,
        }];
        let json = build_packages_json(
            "vendor/pkg",
            &versions,
            "https://forge.example",
            "owner",
            "repo",
        )
        .unwrap();
        let document: Value = serde_json::from_str(&json).unwrap();
        let entry = &document["packages"]["vendor/pkg"]["legacy row"];

        assert_eq!(entry["version"], "legacy row");
        assert!(entry.get("version_normalized").is_none(), "entry: {entry}");
    }

    /// The `dist` block a version entry carries, as a Composer client reads it.
    fn dist_of(version: ComposerVersionInfo) -> Value {
        let json = build_packages_json(
            "vendor/pkg",
            &[version],
            "https://forge.example",
            "owner",
            "repo",
        )
        .unwrap();
        let document: Value = serde_json::from_str(&json).unwrap();
        document["packages"]["vendor/pkg"]["1.0.0"]["dist"].clone()
    }

    /// `dist.shasum` is verified as SHA-1 by `FileDownloader`. Publishing the
    /// SHA-256 there made every `composer install` fail with "The checksum
    /// verification of the file failed" — after the download had succeeded.
    #[test]
    fn shasum_is_the_archives_sha1() {
        let dist = dist_of(ComposerVersionInfo {
            version: "1.0.0".into(),
            filename: "vendor-pkg-1.0.0.zip".into(),
            sha256: Some("a".repeat(64)),
            sha1: Some("b".repeat(40)),
            description: None,
            license: None,
            package_type: None,
            metadata: None,
        });

        assert_eq!(dist["shasum"], "b".repeat(40), "dist: {dist}");
        // `reference` names no algorithm — Composer treats it as an opaque id.
        assert_eq!(dist["reference"], "a".repeat(64), "dist: {dist}");
    }

    /// Composer skips the checksum check when the key is absent, so a file with
    /// no recorded SHA-1 is published without one rather than with a digest
    /// that cannot match.
    #[test]
    fn a_file_without_a_sha1_publishes_no_shasum() {
        let dist = dist_of(ComposerVersionInfo {
            version: "1.0.0".into(),
            filename: "vendor-pkg-1.0.0.zip".into(),
            sha256: Some("a".repeat(64)),
            sha1: None,
            description: None,
            license: None,
            package_type: None,
            metadata: None,
        });

        assert!(dist.get("shasum").is_none(), "dist: {dist}");
        assert!(
            dist["url"]
                .as_str()
                .unwrap()
                .ends_with("vendor-pkg-1.0.0.zip"),
            "dist: {dist}"
        );
    }
}
