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
//!         "integrity": "sha512-...",
//!         "shasum": "...",
//!         "tarball": "https://..."
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! ## What `dist` promises about the tarball
//!
//! Both checksum fields are the client's integrity check, and both name their
//! algorithm: `shasum` is the tarball's **SHA-1** in hex — pacote turns it into
//! `sha1-<base64>` when there is nothing better — and `integrity` is a
//! Subresource Integrity string, `<algorithm>-<base64 of the raw digest>`,
//! conventionally SHA-512. Putting some other digest in either one does not
//! make the answer stronger, it makes it false: pacote verifies what it
//! downloaded, gets a different digest and aborts with `EINTEGRITY` — after the
//! resolver already built the whole tree, so the install fails at the last
//! step. A digest the registry does not have is therefore left out entirely
//! (npm skips a check it was not given) rather than filled in from whatever is
//! at hand.
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

use anyhow::Result;
use flate2::read::GzDecoder;
use std::io::Read;
use tar::Archive;

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};
use crate::package_registry::url_path::encode_path_segment;

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

        // Parse the manifest, don't merely find it — see `CargoAdapter::validate`
        // for why presence is not enough: this is the only gate publish always
        // runs, and a `package.json` that is present but unreadable would be
        // stored and then served to npm as a packument built from nothing.
        self.extract_metadata("", data)?;
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

/// A hex digest as a Subresource Integrity string, `<algorithm>-<base64>`.
///
/// npm compares the digest of what it downloaded against this string, so the
/// base64 is of the raw digest bytes — not of its hex spelling, which is the
/// mistake that produces an integrity string of the right shape and the wrong
/// value. A digest that is missing or not valid hex yields `None`: the field is
/// then left out, and npm skips a check rather than failing one.
fn sri(algorithm: &str, hex_digest: Option<&str>) -> Option<String> {
    use base64::Engine as _;

    let raw = hex::decode(hex_digest?.trim()).ok()?;
    if raw.is_empty() {
        return None;
    }
    Some(format!(
        "{algorithm}-{}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    ))
}

/// Pick the highest live npm version by SemVer precedence.
///
/// The input order is the deterministic publication order supplied by
/// `package_version_ops::list_by_package`. It is only a fallback for historical
/// rows whose version is not parseable as SemVer; once at least one live SemVer
/// exists, an invalid spelling cannot displace it from `latest`.
pub(crate) fn latest_live_semver<'a>(
    versions: impl IntoIterator<Item = (&'a str, bool)>,
) -> Option<&'a str> {
    let mut fallback = None;
    let mut latest: Option<(semver::Version, &'a str)> = None;

    for (version, is_yanked) in versions {
        if is_yanked {
            continue;
        }
        fallback.get_or_insert(version);

        let Ok(parsed) = semver::Version::parse(version) else {
            continue;
        };
        if latest.as_ref().is_none_or(|(current, _)| parsed > *current) {
            latest = Some((parsed, version));
        }
    }

    latest.map(|(_, version)| version).or(fallback)
}

/// Build the npm registry "abbreviated" metadata JSON response.
///
/// This is the format npm expects when querying a registry.
///
/// A stored metadata blob that cannot be read is an error rather than an absent
/// overlay: `dependencies` is seeded with `{}` above so a version published
/// before the adapter recorded anything still answers, and serving that
/// placeholder for a *damaged* row would tell the client the package depends on
/// nothing — a resolution that succeeds and an install that is wrong.
pub fn build_npm_metadata(
    name: &str,
    versions: &[NpmVersionInfo],
    base_url: &str,
    owner: &str,
    repo: &str,
) -> Result<serde_json::Value> {
    let latest_version = latest_live_semver(
        versions
            .iter()
            .map(|version| (version.version.as_str(), version.yanked)),
    )
    .map(str::to_string);
    let dist_tags = latest_version
        .map(|latest| [("latest".to_string(), latest)].into())
        .unwrap_or_default();
    build_npm_metadata_with_dist_tags(name, versions, &dist_tags, base_url, owner, repo)
}

/// Build npm metadata with the canonical persisted selector map.
///
/// The wrapper above retains derived `latest` only for legacy callers and old
/// package rows. Once a package's tag set is initialized, this function is the
/// protocol authority: an empty map stays empty and `beta` never becomes
/// `latest` merely because it has the highest SemVer.
pub fn build_npm_metadata_with_dist_tags(
    name: &str,
    versions: &[NpmVersionInfo],
    dist_tags: &std::collections::BTreeMap<String, String>,
    base_url: &str,
    owner: &str,
    repo: &str,
) -> Result<serde_json::Value> {
    let mut versions_map = serde_json::Map::new();

    for vi in versions {
        // Every component is percent-encoded into ONE segment. A scoped name
        // (`@scope/name`) carries a literal slash, and pasted raw it turns the
        // four-segment download route into six: the router would read
        // `pkg_name=@scope`, `version=name`, and answer the client's own
        // `dist.tarball` with a 404. Encoded, it arrives at the handler
        // decoded and matches the stored row — the same shape the metadata
        // route already receives from npm itself (`@scope%2Fname`).
        let tarball_url = format!(
            "{}/api/v1/repos/{}/{}/packages/npm/{}/{}/{}",
            base_url.trim_end_matches('/'),
            encode_path_segment(owner),
            encode_path_segment(repo),
            encode_path_segment(name),
            encode_path_segment(&vi.version),
            encode_path_segment(vi.filename.as_deref().unwrap_or("package.tgz")),
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

        if let Some(blob) = vi.metadata.as_deref() {
            let stored = serde_json::from_str::<serde_json::Value>(blob).map_err(|error| {
                tracing::error!(
                    package = %name,
                    version = %vi.version,
                    error = %error,
                    "stored npm metadata is not valid JSON — refusing to serve a packument \
                     that would claim the version has no dependencies"
                );
                unreadable_metadata(name, &vi.version)
            })?;
            let serde_json::Value::Object(stored) = stored else {
                tracing::error!(
                    package = %name,
                    version = %vi.version,
                    "stored npm metadata is valid JSON but not an object — refusing to serve \
                     a packument that would claim the version has no dependencies"
                );
                return Err(unreadable_metadata(name, &vi.version));
            };
            for field in ABBREVIATED_FIELDS {
                if let Some(value) = stored.get(field) {
                    ver_obj.insert(field.into(), value.clone());
                }
            }
        }

        // Last, and after the overlay: where the tarball is and what it hashes
        // to is the registry's own answer about its own storage.
        let mut dist = serde_json::Map::new();
        // Preferred by pacote over `shasum`, and the only field here that can
        // carry a modern digest — SHA-512 when the file has one, SHA-256 for a
        // version stored before the registry recorded it. Both are valid SRI.
        if let Some(integrity) =
            sri("sha512", vi.sha512.as_deref()).or_else(|| sri("sha256", vi.sha256.as_deref()))
        {
            dist.insert("integrity".into(), integrity.into());
        }
        // SHA-1 or nothing: see the module docs. A file published before the
        // registry recorded a SHA-1 simply has no `shasum`, and npm falls back
        // to `integrity` — which is the field it prefers anyway.
        if let Some(sha1) = vi.sha1.as_deref().filter(|value| !value.is_empty()) {
            dist.insert("shasum".into(), sha1.into());
        }
        dist.insert("tarball".into(), tarball_url.into());
        ver_obj.insert("dist".into(), serde_json::Value::Object(dist));

        versions_map.insert(vi.version.clone(), serde_json::Value::Object(ver_obj));
    }

    let mut document = serde_json::Map::new();
    document.insert("name".into(), name.into());
    if !dist_tags.is_empty() {
        document.insert("dist-tags".into(), serde_json::to_value(dist_tags)?);
    }
    document.insert("versions".into(), serde_json::Value::Object(versions_map));
    Ok(serde_json::Value::Object(document))
}

/// Untyped on purpose: this is the registry's own row being unreadable, so it
/// must reach the client as a 5xx and never as "fix your request".
fn unreadable_metadata(name: &str, version: &str) -> anyhow::Error {
    anyhow::anyhow!("stored metadata for '{name}' {version} could not be read")
}

/// Info needed for each version in the npm metadata response.
pub struct NpmVersionInfo {
    pub version: String,
    pub description: Option<String>,
    /// Hex SHA-256 of the tarball. Only ever published as an SRI `integrity`
    /// fallback — never as `shasum`, which the protocol defines as SHA-1.
    pub sha256: Option<String>,
    /// Hex SHA-1 of the tarball, published verbatim as `dist.shasum`. `None`
    /// for a version stored before the registry recorded it, and the field is
    /// then omitted.
    pub sha1: Option<String>,
    /// Hex SHA-512 of the tarball, published as `dist.integrity`. `None` for a
    /// version stored before the registry recorded it, and `integrity` then
    /// falls back to SHA-256.
    pub sha512: Option<String>,
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
    use sha1::Digest as _;

    /// Bytes standing in for a published tarball. What they are does not
    /// matter — that every digest in `dist` is a digest of *these* bytes does.
    const TARBALL: &[u8] = b"matrix-npm-1.0.0.tgz contents";

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
                sha256: Some(hex::encode(sha2::Sha256::digest(TARBALL))),
                sha1: Some(hex::encode(sha1::Sha1::digest(TARBALL))),
                sha512: Some(hex::encode(sha2::Sha512::digest(TARBALL))),
                filename: Some("matrix-npm-1.0.0.tgz".into()),
                yanked: false,
                metadata: Some(stored),
            }],
            "https://forge.example",
            "acme",
            "tools",
        )
        .expect("the manifest's own metadata is readable");
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
        assert_eq!(
            version["devDependencies"],
            serde_json::json!({ "jest": "^29.0.0" })
        );
        // The rest of the object is untouched by the overlay.
        assert_eq!(version["version"], "1.0.0");
        assert_eq!(
            version["dist"]["shasum"],
            serde_json::json!(hex::encode(sha1::Sha1::digest(TARBALL)))
        );
        assert_eq!(
            version["dist"]["tarball"],
            "https://forge.example/api/v1/repos/acme/tools/packages/npm/matrix-npm/1.0.0/matrix-npm-1.0.0.tgz"
        );
    }

    /// The two checksum fields name their algorithms, and pacote verifies the
    /// tarball against both. `shasum` used to carry the SHA-256, which is a
    /// SHA-1 field: the check failed on every download, and it failed at the
    /// end of the install, after the resolver had already built the tree.
    #[test]
    fn the_dist_block_publishes_each_digest_under_its_own_algorithm() {
        use base64::Engine as _;

        let version = version_object(r#"{ "name": "matrix-npm", "version": "1.0.0" }"#);
        let dist = &version["dist"];

        assert_eq!(
            dist["shasum"],
            serde_json::json!(hex::encode(sha1::Sha1::digest(TARBALL))),
            "shasum is the tarball's SHA-1, in hex: {dist}"
        );
        assert_eq!(
            dist["integrity"],
            serde_json::json!(format!(
                "sha512-{}",
                base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(TARBALL))
            )),
            "integrity is an SRI string over the raw SHA-512 bytes: {dist}"
        );
    }

    /// A version stored before the registry recorded a SHA-1 has no honest
    /// `shasum` to publish, and the SHA-256 it does have is still a valid SRI
    /// digest — so the answer is an `integrity` without a `shasum`, not a
    /// `shasum` npm is guaranteed to reject.
    #[test]
    fn a_version_without_a_sha1_publishes_integrity_and_no_shasum() {
        use base64::Engine as _;

        let document = build_npm_metadata(
            "matrix-npm",
            &[NpmVersionInfo {
                version: "1.0.0".into(),
                description: None,
                sha256: Some(hex::encode(sha2::Sha256::digest(TARBALL))),
                sha1: None,
                sha512: None,
                filename: Some("matrix-npm-1.0.0.tgz".into()),
                yanked: false,
                metadata: None,
            }],
            "https://forge.example",
            "acme",
            "tools",
        )
        .expect("a version with no stored metadata still has a packument");
        let dist = &document["versions"]["1.0.0"]["dist"];

        assert!(
            dist.get("shasum").is_none(),
            "no SHA-1 on record means no shasum at all: {dist}"
        );
        assert_eq!(
            dist["integrity"],
            serde_json::json!(format!(
                "sha256-{}",
                base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(TARBALL))
            )),
            "SHA-256 is a valid SRI algorithm and says so: {dist}"
        );
        // The tarball URL is the one thing that must never go missing.
        assert_eq!(
            dist["tarball"],
            "https://forge.example/api/v1/repos/acme/tools/packages/npm/matrix-npm/1.0.0/matrix-npm-1.0.0.tgz"
        );
    }

    /// A version with no digests at all is a version npm downloads without
    /// verifying — which it does happily. An empty or malformed digest must not
    /// turn into an integrity string of the right shape and the wrong value.
    #[test]
    fn a_version_without_digests_publishes_no_checksum_fields() {
        let document = build_npm_metadata(
            "matrix-npm",
            &[NpmVersionInfo {
                version: "1.0.0".into(),
                description: None,
                sha256: Some(String::new()),
                sha1: Some(String::new()),
                sha512: Some("not hex".into()),
                filename: Some("matrix-npm-1.0.0.tgz".into()),
                yanked: false,
                metadata: None,
            }],
            "https://forge.example",
            "acme",
            "tools",
        )
        .expect("a version with no stored metadata still has a packument");
        let dist = &document["versions"]["1.0.0"]["dist"];

        assert!(dist.get("shasum").is_none(), "dist: {dist}");
        assert!(dist.get("integrity").is_none(), "dist: {dist}");
        assert!(dist.get("tarball").is_some(), "dist: {dist}");
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

        assert_eq!(
            version["peerDependencies"],
            serde_json::json!({ "react": ">=17" })
        );
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

        assert_eq!(
            stored["bundleDependencies"],
            serde_json::json!(["left-pad"])
        );
        assert!(stored.get("bundledDependencies").is_none(), "{stored}");
    }

    /// A manifest with no dependency table records an empty one, so "depends on
    /// nothing" is something the registry knows rather than something it failed
    /// to look up.
    #[test]
    fn a_manifest_without_dependencies_records_an_empty_table() {
        let stored = stored_metadata(r#"{ "name": "matrix-npm", "version": "1.0.0" }"#);

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

    /// Versions published before the adapter stored anything still serve a
    /// well-formed version object: nothing was ever recorded for them, so the
    /// empty `dependencies` table is the honest answer rather than a claim.
    #[test]
    fn a_version_without_stored_metadata_keeps_the_old_shape() {
        let document = build_npm_metadata(
            "matrix-npm",
            &[NpmVersionInfo {
                version: "1.0.0".into(),
                description: None,
                sha256: None,
                sha1: None,
                sha512: None,
                filename: None,
                yanked: false,
                metadata: None,
            }],
            "https://forge.example",
            "acme",
            "tools",
        )
        .expect("a version with no stored metadata still has a packument");
        let version = &document["versions"]["1.0.0"];

        assert_eq!(version["dependencies"], serde_json::json!({}));
        assert_eq!(version["version"], "1.0.0");
        assert_eq!(
            version["dist"]["tarball"],
            "https://forge.example/api/v1/repos/acme/tools/packages/npm/matrix-npm/1.0.0/package.tgz",
        );
    }

    /// card_49d7caba8e4f: a row that *has* metadata which cannot be read is a
    /// different thing entirely. Serving the placeholder would tell npm the
    /// version depends on nothing — the install then succeeds and is wrong.
    #[test]
    fn a_version_whose_stored_metadata_cannot_be_read_is_refused() {
        for metadata in ["not json", "[1,2]", "null", "\"dependencies\""] {
            let refused = build_npm_metadata(
                "matrix-npm",
                &[NpmVersionInfo {
                    version: "1.0.0".into(),
                    description: None,
                    sha256: None,
                    sha1: None,
                    sha512: None,
                    filename: None,
                    yanked: false,
                    metadata: Some(metadata.to_string()),
                }],
                "https://forge.example",
                "acme",
                "tools",
            );

            let error = refused
                .err()
                .unwrap_or_else(|| panic!("damaged metadata {metadata:?} must not be served"));
            assert!(
                error
                    .downcast_ref::<crate::error::InvalidRequest>()
                    .is_none(),
                "an unreadable row of ours is not the client's bad request: {error:#}"
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
                sha1: None,
                sha512: None,
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
        )
        .expect("a readable blob is served");
        let version = &document["versions"]["1.0.0"];

        assert!(version.get("summary").is_none(), "{version}");
        assert_eq!(version["version"], "1.0.0", "{version}");
        assert_eq!(
            version["dist"]["tarball"],
            "https://forge.example/api/v1/repos/acme/tools/packages/npm/matrix-npm/1.0.0/matrix-npm-1.0.0.tgz",
            "{version}",
        );
    }

    /// `dist-tags.latest` is what a bare `npm install <pkg>` resolves to. A
    /// later backport must not pull it off the highest live SemVer, and a yanked
    /// version is not a candidate even when it is higher still.
    #[test]
    fn the_latest_tag_uses_the_highest_live_semver() {
        let document = build_npm_metadata(
            "matrix-npm",
            &[
                NpmVersionInfo {
                    version: "3.0.0".into(),
                    description: None,
                    sha256: None,
                    sha1: None,
                    sha512: None,
                    filename: None,
                    yanked: true,
                    metadata: None,
                },
                NpmVersionInfo {
                    // Published after 2.0.0: list_by_package puts this first.
                    version: "1.2.4".into(),
                    description: None,
                    sha256: None,
                    sha1: None,
                    sha512: None,
                    filename: None,
                    yanked: false,
                    metadata: Some(r#"{"dependencies":{"left-pad":"^1.3.0"}}"#.into()),
                },
                NpmVersionInfo {
                    version: "2.0.0".into(),
                    description: None,
                    sha256: None,
                    sha1: None,
                    sha512: None,
                    filename: None,
                    yanked: false,
                    metadata: None,
                },
            ],
            "https://forge.example",
            "acme",
            "tools",
        )
        .expect("all versions are readable");

        assert_eq!(document["dist-tags"]["latest"], "2.0.0", "{document}");
        assert_eq!(
            document["versions"]["1.2.4"]["dependencies"],
            serde_json::json!({ "left-pad": "^1.3.0" }),
        );
        assert_eq!(
            document["versions"]["3.0.0"]["dependencies"],
            serde_json::json!({}),
        );
    }

    #[test]
    fn persisted_named_tags_are_not_rewritten_as_latest() {
        let versions = [
            NpmVersionInfo {
                version: "2.0.0-beta.1".into(),
                description: None,
                sha256: None,
                sha1: None,
                sha512: None,
                filename: None,
                yanked: false,
                metadata: None,
            },
            NpmVersionInfo {
                version: "1.0.0".into(),
                description: None,
                sha256: None,
                sha1: None,
                sha512: None,
                filename: None,
                yanked: false,
                metadata: None,
            },
        ];
        let tags = [("beta".to_string(), "2.0.0-beta.1".to_string())].into();
        let document = build_npm_metadata_with_dist_tags(
            "matrix-npm",
            &versions,
            &tags,
            "https://forge.example",
            "acme",
            "tools",
        )
        .unwrap();

        assert_eq!(document["dist-tags"]["beta"], "2.0.0-beta.1");
        assert!(
            document["dist-tags"].get("latest").is_none(),
            "an explicit beta-only tag set must stay beta-only: {document}"
        );
    }

    #[test]
    fn unparsable_historical_versions_keep_the_deterministic_input_order() {
        assert_eq!(
            latest_live_semver([
                ("legacy-backport", false),
                ("legacy-main", false),
                ("legacy-withdrawn", true),
            ]),
            Some("legacy-backport")
        );
    }

    /// If every version is yanked, there is no `latest` tag to publish. A fake
    /// `0.0.0` points npm at a version the registry does not have, making the
    /// server's state look like a client-side resolution mistake.
    #[test]
    fn all_yanked_versions_publish_no_latest_tag() {
        let document = build_npm_metadata(
            "matrix-npm",
            &[NpmVersionInfo {
                version: "1.0.0".into(),
                description: None,
                sha256: None,
                sha1: None,
                sha512: None,
                filename: None,
                yanked: true,
                metadata: None,
            }],
            "https://forge.example",
            "acme",
            "tools",
        )
        .expect("a yanked version with no stored metadata still has a packument");

        assert!(
            document.get("dist-tags").is_none(),
            "an all-yanked package must not invent dist-tags: {document}"
        );
        assert!(
            document["versions"].get("1.0.0").is_some(),
            "the historical version still exists in metadata: {document}"
        );
        assert!(
            !document.to_string().contains("0.0.0"),
            "metadata must not point at a fabricated version: {document}"
        );
    }
}
