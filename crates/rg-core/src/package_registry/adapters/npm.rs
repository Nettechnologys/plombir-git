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
//!       "dependencies": { "left-pad": "^1.3.0" },
//!       "dist": {
//!         "shasum": "...",
//!         "tarball": "https://..."
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! This document — the "abbreviated" one, `application/vnd.npm.install-v1+json`
//! — is the only thing npm's dependency resolver reads. It never opens a
//! tarball to find out what a package needs, so a version object without
//! `dependencies` is not an incomplete answer but a wrong one: it states the
//! package depends on nothing, `npm install` succeeds on that, and what lands
//! in `node_modules` is a package that cannot run. See
//! [`npm_protocol_metadata`] for what the adapter lifts out of `package.json`
//! to keep that from happening.
//!
//! ForgeKeep serves this at:
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

        // The dependency tables have no package column of their own, and the
        // metadata route never opens the tarball — so they are lifted here,
        // once, at publish.
        let protocol_metadata = Some(npm_protocol_metadata(&doc));

        Ok(ExtractedMetadata {
            name,
            version: version.clone(),
            description,
            homepage,
            repository_url,
            keywords,
            license,
            semver: Some(version),
            protocol_metadata,
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

/// The `package.json` keys an abbreviated version object carries.
///
/// This is both what the adapter stores at publish and the whitelist a stored
/// blob is read back through. `name`, `version` and `dist` are deliberately
/// absent: those are the registry's own answer, not the manifest's, and a
/// free-form JSON column that could contribute them would let a hand-edited row
/// re-point a tarball.
const ABBREVIATED_FIELDS: [&str; 13] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "peerDependenciesMeta",
    "optionalDependencies",
    "bundleDependencies",
    "bin",
    "engines",
    "os",
    "cpu",
    "directories",
    "deprecated",
    "hasInstallScript",
];

/// The fields npm reads as a table of name → requirement.
///
/// Kept apart from the rest because a manifest that spelled one of them as
/// something other than an object would put a value in the protocol response
/// that npm cannot parse at all — the resolver would fail on the whole
/// document rather than on the one odd package.
const DEPENDENCY_TABLES: [&str; 5] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "peerDependenciesMeta",
    "optionalDependencies",
];

/// Fields that travel exactly as `package.json` spells them.
///
/// Their shape is the protocol's business, not ours: `bin` is a string or a
/// table, `bundleDependencies` a list, `engines` / `directories` tables, `os`
/// and `cpu` lists, `deprecated` the message npm prints when the version is
/// installed.
const VERBATIM_FIELDS: [&str; 7] = [
    "bundleDependencies",
    "bin",
    "engines",
    "os",
    "cpu",
    "directories",
    "deprecated",
];

/// The `package.json` sections the abbreviated document is expected to carry,
/// as a JSON object ready to be pasted into a version entry.
///
/// This is recorded at publish rather than derived on read because the manifest
/// exists only inside the `.tgz`, which the metadata route never opens.
/// `dependencies` is always written, so an empty table is a fact the registry
/// knows about the package rather than one it failed to look up.
fn npm_protocol_metadata(doc: &serde_json::Value) -> String {
    let mut out = serde_json::Map::new();

    for field in DEPENDENCY_TABLES {
        if let Some(serde_json::Value::Object(table)) = doc.get(field) {
            out.insert(field.into(), serde_json::Value::Object(table.clone()));
        }
    }
    if !out.contains_key("dependencies") {
        out.insert("dependencies".into(), serde_json::json!({}));
    }

    for field in VERBATIM_FIELDS {
        if let Some(value) = doc.get(field).filter(|v| !v.is_null()) {
            out.insert(field.into(), value.clone());
        }
    }

    // npm has accepted `bundledDependencies` since long before the current
    // spelling and still does; a registry publishes only the one npm reads.
    if !out.contains_key("bundleDependencies") {
        if let Some(value) = doc.get("bundledDependencies").filter(|v| !v.is_null()) {
            out.insert("bundleDependencies".into(), value.clone());
        }
    }

    // Not a manifest key — the registry derives it. npm uses it to decide
    // whether a package needs a build step at all, and the answer is only
    // interesting when it is yes.
    if has_install_script(doc) {
        out.insert("hasInstallScript".into(), true.into());
    }

    serde_json::Value::Object(out).to_string()
}

/// Whether installing the package runs anything of its own.
fn has_install_script(doc: &serde_json::Value) -> bool {
    let Some(scripts) = doc.get("scripts").and_then(|v| v.as_object()) else {
        return false;
    };
    ["preinstall", "install", "postinstall"].iter().any(|hook| {
        scripts
            .get(*hook)
            .and_then(|v| v.as_str())
            .is_some_and(|command| !command.trim().is_empty())
    })
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
        // A version published before the adapter recorded anything still gets
        // the key: to npm an absent table and an empty one say the same thing,
        // and saying it outright keeps the response self-describing.
        ver_obj.insert("dependencies".into(), serde_json::json!({}));

        let stored = vi
            .metadata
            .as_deref()
            .and_then(|blob| serde_json::from_str::<serde_json::Value>(blob).ok());
        if let Some(serde_json::Value::Object(stored)) = stored {
            for field in ABBREVIATED_FIELDS {
                if let Some(value) = stored.get(field) {
                    ver_obj.insert(field.into(), value.clone());
                }
            }
        }

        // Last, and after the overlay: where the tarball is and what it hashes
        // to is the registry's own answer about its own storage.
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
    /// The abbreviated-document fields the adapter stored at publish, as a JSON
    /// object; see [`npm_protocol_metadata`]. `None` for a version published
    /// before the adapter recorded them, and the version object then falls back
    /// to the dependency-free shape it always had.
    pub metadata: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    /// An npm package is a gzipped tar carrying `package/package.json`.
    fn make_tgz(manifest: &str) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        let bytes = manifest.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "package/package.json", bytes)
            .unwrap();
        let tar = tar.into_inner().unwrap();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, &tar).unwrap();
        encoder.finish().unwrap()
    }

    /// The abbreviated fields of a manifest, as the adapter records them.
    fn stored_metadata(manifest: &str) -> serde_json::Value {
        let meta = NpmAdapter
            .extract_metadata("matrix-npm-1.0.0.tgz", &make_tgz(manifest))
            .unwrap();
        serde_json::from_str(&meta.protocol_metadata.expect("no protocol metadata")).unwrap()
    }

    /// The version object a client would read for that manifest.
    fn version_object(manifest: &str) -> serde_json::Value {
        let stored = stored_metadata(manifest).to_string();
        let document = build_npm_metadata(
            "matrix-npm",
            &[NpmVersionInfo {
                version: "1.0.0".into(),
                description: Some("matrix package".into()),
                sha256: Some("deadbeef".into()),
                filename: Some("matrix-npm-1.0.0.tgz".into()),
                yanked: false,
                metadata: Some(stored),
            }],
            "https://forge.example",
            "acme",
            "tools",
        );
        document["versions"]["1.0.0"].clone()
    }

    #[test]
    fn the_version_object_carries_the_manifest_dependencies() {
        let version = version_object(
            r#"{
  "name": "matrix-npm",
  "version": "1.0.0",
  "dependencies": { "left-pad": "^1.3.0", "lodash": "4.17.21" },
  "devDependencies": { "jest": "^29.0.0" }
}"#,
        );

        assert_eq!(
            version["dependencies"],
            serde_json::json!({ "left-pad": "^1.3.0", "lodash": "4.17.21" }),
            "version object: {version}"
        );
        assert_eq!(version["devDependencies"], serde_json::json!({ "jest": "^29.0.0" }));
        // The rest of the object is untouched by the overlay.
        assert_eq!(version["version"], "1.0.0");
        assert_eq!(version["dist"]["shasum"], "deadbeef");
        assert_eq!(
            version["dist"]["tarball"],
            "https://forge.example/api/v1/repos/acme/tools/packages/npm/matrix-npm/1.0.0/matrix-npm-1.0.0.tgz"
        );
    }

    /// Peer and optional dependencies decide whether an install warns, errors
    /// or quietly skips a package — all three are read from this document.
    #[test]
    fn peer_and_optional_dependencies_reach_the_version_object() {
        let version = version_object(
            r#"{
  "name": "matrix-npm",
  "version": "1.0.0",
  "peerDependencies": { "react": ">=17" },
  "peerDependenciesMeta": { "react": { "optional": true } },
  "optionalDependencies": { "fsevents": "^2.3.0" }
}"#,
        );

        assert_eq!(version["peerDependencies"], serde_json::json!({ "react": ">=17" }));
        assert_eq!(
            version["peerDependenciesMeta"],
            serde_json::json!({ "react": { "optional": true } }),
        );
        assert_eq!(
            version["optionalDependencies"],
            serde_json::json!({ "fsevents": "^2.3.0" }),
        );
    }

    /// `bin` is what puts a command on `PATH`, `engines` / `os` / `cpu` are
    /// what let a client refuse a package it cannot run, and `deprecated` is
    /// the warning npm prints. None of them survive a version object that only
    /// carries a tarball URL.
    #[test]
    fn the_platform_fields_travel_as_published() {
        let version = version_object(
            r#"{
  "name": "matrix-npm",
  "version": "1.0.0",
  "bin": { "matrix": "./cli.js" },
  "engines": { "node": ">=18" },
  "os": ["linux", "darwin"],
  "cpu": ["x64"],
  "directories": { "lib": "lib" },
  "deprecated": "use matrix-npm-next"
}"#,
        );

        assert_eq!(version["bin"], serde_json::json!({ "matrix": "./cli.js" }));
        assert_eq!(version["engines"], serde_json::json!({ "node": ">=18" }));
        assert_eq!(version["os"], serde_json::json!(["linux", "darwin"]));
        assert_eq!(version["cpu"], serde_json::json!(["x64"]));
        assert_eq!(version["directories"], serde_json::json!({ "lib": "lib" }));
        assert_eq!(version["deprecated"], "use matrix-npm-next");
    }

    /// `hasInstallScript` is not a manifest key: the registry derives it, and
    /// npm reads it to decide the package needs a build step.
    #[test]
    fn an_install_script_is_announced_and_its_absence_is_not() {
        let with_script = stored_metadata(
            r#"{
  "name": "matrix-npm",
  "version": "1.0.0",
  "scripts": { "postinstall": "node build.js", "test": "jest" }
}"#,
        );
        assert_eq!(with_script["hasInstallScript"], true);

        let without = stored_metadata(
            r#"{
  "name": "matrix-npm",
  "version": "1.0.0",
  "scripts": { "test": "jest", "install": "   " }
}"#,
        );
        assert!(
            without.get("hasInstallScript").is_none(),
            "a blank hook is not a build step: {without}"
        );
    }

    /// npm accepted `bundledDependencies` first; a registry publishes the
    /// spelling npm's resolver reads.
    #[test]
    fn the_legacy_bundled_spelling_is_normalised() {
        let stored = stored_metadata(
            r#"{
  "name": "matrix-npm",
  "version": "1.0.0",
  "bundledDependencies": ["left-pad"]
}"#,
        );

        assert_eq!(stored["bundleDependencies"], serde_json::json!(["left-pad"]));
        assert!(stored.get("bundledDependencies").is_none(), "{stored}");
    }

    /// A manifest with no dependency table records an empty one, so "depends on
    /// nothing" is something the registry knows rather than something it failed
    /// to look up.
    #[test]
    fn a_manifest_without_dependencies_records_an_empty_table() {
        let stored = stored_metadata(
            r#"{ "name": "matrix-npm", "version": "1.0.0" }"#,
        );

        assert_eq!(stored["dependencies"], serde_json::json!({}));
        assert!(stored.get("peerDependencies").is_none(), "{stored}");
    }

    /// A `dependencies` that is not a table would make the whole document
    /// unparseable for npm, so it never reaches one.
    #[test]
    fn a_malformed_dependency_table_is_not_republished() {
        let stored = stored_metadata(
            r#"{ "name": "matrix-npm", "version": "1.0.0", "dependencies": "left-pad" }"#,
        );

        assert_eq!(stored["dependencies"], serde_json::json!({}));
    }

    /// Versions published before the adapter stored anything — and rows whose
    /// metadata is not the JSON object this expects — still serve a well-formed
    /// version object rather than one npm cannot read.
    #[test]
    fn a_version_without_stored_metadata_keeps_the_old_shape() {
        for metadata in [None, Some("not json".to_string()), Some("[1,2]".to_string())] {
            let document = build_npm_metadata(
                "matrix-npm",
                &[NpmVersionInfo {
                    version: "1.0.0".into(),
                    description: None,
                    sha256: None,
                    filename: None,
                    yanked: false,
                    metadata: metadata.clone(),
                }],
                "https://forge.example",
                "acme",
                "tools",
            );
            let version = &document["versions"]["1.0.0"];

            assert_eq!(version["dependencies"], serde_json::json!({}), "{metadata:?}");
            assert_eq!(version["version"], "1.0.0", "{metadata:?}");
            assert_eq!(
                version["dist"]["tarball"],
                "https://forge.example/api/v1/repos/acme/tools/packages/npm/matrix-npm/1.0.0/package.tgz",
                "{metadata:?}",
            );
        }
    }

    /// The metadata column is free-form JSON; only the abbreviated document's
    /// own keys may reach a protocol response. `dist` above all: a row that
    /// could contribute one would re-point the tarball a client downloads.
    #[test]
    fn a_stray_key_in_the_stored_blob_stays_out_of_the_version_object() {
        let document = build_npm_metadata(
            "matrix-npm",
            &[NpmVersionInfo {
                version: "1.0.0".into(),
                description: None,
                sha256: Some("deadbeef".into()),
                filename: Some("matrix-npm-1.0.0.tgz".into()),
                yanked: false,
                metadata: Some(
                    r#"{"dependencies":{},"summary":"leaked",
                        "dist":{"tarball":"https://evil.example/payload.tgz"},
                        "version":"9.9.9"}"#
                        .into(),
                ),
            }],
            "https://forge.example",
            "acme",
            "tools",
        );
        let version = &document["versions"]["1.0.0"];

        assert!(version.get("summary").is_none(), "{version}");
        assert_eq!(version["version"], "1.0.0", "{version}");
        assert_eq!(
            version["dist"]["tarball"],
            "https://forge.example/api/v1/repos/acme/tools/packages/npm/matrix-npm/1.0.0/matrix-npm-1.0.0.tgz",
            "{version}",
        );
    }

    /// `dist-tags.latest` is what a bare `npm install <pkg>` resolves to, and a
    /// yanked version is not it.
    #[test]
    fn the_latest_tag_skips_yanked_versions() {
        let document = build_npm_metadata(
            "matrix-npm",
            &[
                NpmVersionInfo {
                    version: "2.0.0".into(),
                    description: None,
                    sha256: None,
                    filename: None,
                    yanked: true,
                    metadata: None,
                },
                NpmVersionInfo {
                    version: "1.0.0".into(),
                    description: None,
                    sha256: None,
                    filename: None,
                    yanked: false,
                    metadata: Some(r#"{"dependencies":{"left-pad":"^1.3.0"}}"#.into()),
                },
            ],
            "https://forge.example",
            "acme",
            "tools",
        );

        assert_eq!(document["dist-tags"]["latest"], "1.0.0", "{document}");
        assert_eq!(
            document["versions"]["1.0.0"]["dependencies"],
            serde_json::json!({ "left-pad": "^1.3.0" }),
        );
        assert_eq!(
            document["versions"]["2.0.0"]["dependencies"],
            serde_json::json!({}),
        );
    }
}
