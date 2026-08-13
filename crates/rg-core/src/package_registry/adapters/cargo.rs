//! Cargo (Rust) package adapter.
//!
//! Handles `.crate` files: tar.gz archives containing a `Cargo.toml` manifest.
//!
//! ## Sparse index protocol
//!
//! Cargo ≥ 1.68 uses the "sparse index" protocol (RFC 2789). The registry root
//! is the index URL a user writes into `.cargo/config.toml`; ForgeKeep's is
//!   `sparse+{base}/api/v1/repos/{owner}/{repo}/packages/cargo/index/`
//! and two shapes hang off it:
//!
//! - `{index}/config.json` — [`build_cargo_index_config`]. Cargo fetches this
//!   before any crate, so a registry without it is unusable.
//! - `{index}/{prefix…}/{crate}` — [`build_sparse_index`], line-delimited JSON,
//!   one entry per version. Cargo never asks for the bare name: it spells it
//!   out as a directory prefix, see [`cargo_index_prefix`].

use anyhow::{Context, Result};
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

        // `[package]` fills the package row; the dependency and feature tables
        // go into the sparse index, so the whole document is parsed.
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

        let protocol_metadata = Some(cargo_protocol_metadata(&doc, pkg)?);

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
        // Check that it's a valid gzip stream
        let mut decoder = GzDecoder::new(data);
        let mut buf = Vec::new();
        decoder
            .read_to_end(&mut buf)
            .map_err(|e| anyhow::anyhow!("invalid .crate file (not valid gzip): {e}"))?;

        // Parse the manifest, don't merely find it. `validate` is the only
        // unconditional gate publish runs — `extract_metadata` failing is not
        // fatal when the coordinates arrived as query parameters — so a crate
        // whose `Cargo.toml` is present but unparseable would otherwise be
        // stored, served from the sparse index, and break `cargo` at install.
        // The absence check is not lost: parsing reports it first, and more
        // precisely (`no Cargo.toml found in archive`).
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

/// One published version, in the terms the sparse index describes it.
#[derive(Debug, Clone, Copy)]
pub struct CargoIndexVersion<'a> {
    /// The version string, as published.
    pub version: &'a str,
    /// Checksum of the `.crate` file — cargo verifies the download against it.
    pub sha256: Option<&'a str>,
    /// Whether the version was yanked.
    pub yanked: bool,
    /// The index fields the adapter stored at publish, as a JSON object; see
    /// [`cargo_protocol_metadata`]. `None` for a version published before the
    /// adapter recorded them, and the entry then falls back to the dependency-
    /// free shape it always had.
    pub metadata: Option<&'a str>,
}

/// The keys a stored blob is allowed to contribute to an index entry.
///
/// The metadata column is written by [`CargoAdapter::extract_metadata`], but it
/// is still a free-form JSON column: whitelisting keeps anything else that ends
/// up there — a future field, a hand-edited row — out of the protocol response,
/// where cargo would reject the line as malformed rather than ignore it.
const INDEX_FIELDS: [&str; 6] = [
    "deps",
    "features",
    "features2",
    "v",
    "links",
    "rust_version",
];

/// Build a sparse-index line for a version entry.
///
/// Cargo expects one JSON object per line, like:
/// ```json
/// {"name":"mycrate","vers":"0.1.0","deps":[{"name":"serde","req":"^1.0","features":[],
///  "optional":false,"default_features":true,"target":null,"kind":"normal",
///  "registry":null,"package":null}],"cksum":"...","features":{},"yanked":false,"links":null}
/// ```
///
/// `deps` and `features` are the resolver's whole input: cargo resolves against
/// the index, never against the `.crate` file, so an entry that reports neither
/// is not a partial answer but a wrong one — it asserts the crate depends on
/// nothing, and resolution succeeds on that lie before the build fails at
/// `unresolved import`.
///
/// A stored blob that cannot be read is an error rather than an absent overlay.
/// The three outcomes are genuinely different and only two of them are fine:
///
/// * `None` — the version predates the adapter recording anything. The
///   placeholders above are the honest answer for it.
/// * a JSON object — the recorded facts, overlaid.
/// * anything else — the row is damaged, and the entry that would be served
///   instead is the lie this doc comment describes. Refuse to serve it.
pub fn build_sparse_index_entry(
    name: &str,
    version: &CargoIndexVersion<'_>,
) -> Result<serde_json::Value> {
    let mut entry = serde_json::json!({
        "name": name,
        "vers": version.version,
        "deps": [],
        "cksum": version.sha256.unwrap_or(""),
        "features": {},
        "yanked": version.yanked,
        "links": serde_json::Value::Null,
    });

    if let Some(blob) = version.metadata {
        let stored = serde_json::from_str::<serde_json::Value>(blob).map_err(|error| {
            tracing::error!(
                package = %name,
                version = %version.version,
                error = %error,
                "stored cargo index metadata is not valid JSON — refusing to serve an \
                 index entry that would claim the crate has no dependencies"
            );
            unreadable_metadata(name, version.version)
        })?;
        let serde_json::Value::Object(stored) = stored else {
            tracing::error!(
                package = %name,
                version = %version.version,
                "stored cargo index metadata is valid JSON but not an object — refusing to \
                 serve an index entry that would claim the crate has no dependencies"
            );
            return Err(unreadable_metadata(name, version.version));
        };
        for field in INDEX_FIELDS {
            if let Some(value) = stored.get(field) {
                entry[field] = value.clone();
            }
        }
    }

    Ok(entry)
}

/// Untyped on purpose: this is the registry's own row being unreadable, so it
/// must reach the client as a 5xx and never as "fix your request".
fn unreadable_metadata(name: &str, version: &str) -> anyhow::Error {
    anyhow::anyhow!("stored metadata for '{name}' {version} could not be read")
}

/// Build the full sparse-index response: one JSON line per version.
pub fn build_sparse_index(name: &str, versions: &[CargoIndexVersion<'_>]) -> Result<String> {
    let mut lines = String::new();
    for version in versions {
        let entry = build_sparse_index_entry(name, version)?;
        lines.push_str(&serde_json::to_string(&entry).context("serialize sparse index entry")?);
        lines.push('\n');
    }
    Ok(lines)
}

/// The `Cargo.toml` sections that have no package column of their own, in the
/// shape RFC 2789 spells them, ready to be pasted into an index entry.
///
/// This is written at publish rather than derived on read because the `.crate`
/// file is the only place the manifest exists, and the index route never opens
/// it. The object is always produced, so an empty `deps` is a recorded fact
/// about the crate instead of a missing one.
///
/// Fallible because a manifest can declare a feature table this shape cannot
/// carry; see [`index_features`] for why that is a refusal rather than a
/// best-effort translation.
fn cargo_protocol_metadata(doc: &toml::Value, pkg: &toml::Value) -> Result<String> {
    let mut out = serde_json::Map::new();

    out.insert("deps".into(), index_dependencies(doc).into());

    let (features, features2) = index_features(doc)?;
    out.insert("features".into(), features.into());
    if !features2.is_empty() {
        out.insert("features2".into(), features2.into());
        // Schema 2 is what tells a client `features2` may be present. Cargo
        // before 1.60 reads only `features`, so keeping the new syntax out of
        // it is what lets an old client read the rest of the entry at all.
        out.insert("v".into(), 2.into());
    }

    // `links` claims a native library, and cargo refuses a graph where two
    // crates claim the same one — a check it can only make from the index.
    if let Some(links) = pkg.get("links").and_then(|v| v.as_str()) {
        out.insert("links".into(), links.into());
    }
    // MSRV-aware resolution picks versions by this field; without it cargo
    // picks the newest and fails on a toolchain the crate never supported.
    if let Some(rust_version) = pkg.get("rust-version").and_then(|v| v.as_str()) {
        out.insert("rust_version".into(), rust_version.into());
    }

    Ok(serde_json::Value::Object(out).to_string())
}

/// The manifest's dependency tables and the `kind` each maps to in the index.
const DEP_SECTIONS: [(&str, &str); 3] = [
    ("dependencies", "normal"),
    ("dev-dependencies", "dev"),
    ("build-dependencies", "build"),
];

/// Every dependency a manifest declares, including the per-target ones.
///
/// Dev-dependencies are kept — unlike a gemspec's, they are what `cargo test`
/// of a *published* crate resolves against, and the index is where cargo reads
/// them from; the `kind` field is how a client tells them apart.
fn index_dependencies(doc: &toml::Value) -> Vec<serde_json::Value> {
    let mut deps = Vec::new();

    for (section, kind) in DEP_SECTIONS {
        collect_dependency_section(doc.get(section), kind, None, &mut deps);
    }

    // `[target.'cfg(unix)'.dependencies]` — the same three tables again, once
    // per target expression. The expression is not ours to interpret: it goes
    // into the entry verbatim and the client decides whether it applies.
    if let Some(targets) = doc.get("target").and_then(|v| v.as_table()) {
        for (target, sections) in targets {
            for (section, kind) in DEP_SECTIONS {
                collect_dependency_section(sections.get(section), kind, Some(target), &mut deps);
            }
        }
    }

    deps
}

fn collect_dependency_section(
    section: Option<&toml::Value>,
    kind: &str,
    target: Option<&str>,
    out: &mut Vec<serde_json::Value>,
) {
    let Some(table) = section.and_then(|v| v.as_table()) else {
        return;
    };
    for (alias, spec) in table {
        out.push(index_dependency(alias, spec, kind, target));
    }
}

/// One dependency, in the form RFC 2789 gives it.
///
/// `name` is the name the manifest imports the crate under; a renamed
/// dependency (`foo = { package = "bar" }`) keeps the alias here and names the
/// real crate in `package`, which is the split cargo's resolver expects — swap
/// them and it fetches a crate that does not exist.
fn index_dependency(
    alias: &str,
    spec: &toml::Value,
    kind: &str,
    target: Option<&str>,
) -> serde_json::Value {
    // The short form is nothing but the requirement: `serde = "1.0"`.
    let short_form = spec.as_str().map(String::from);

    let req = short_form.clone().unwrap_or_else(|| {
        spec.get("version")
            .and_then(|v| v.as_str())
            // A dependency that reached the index without a requirement (a
            // bare `path` / `git` entry that survived packaging) matches
            // anything, rather than vanishing from the entry.
            .unwrap_or("*")
            .to_string()
    });

    let features: Vec<serde_json::Value> = spec
        .get("features")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| f.as_str())
                .map(Into::into)
                .collect()
        })
        .unwrap_or_default();

    let optional = spec
        .get("optional")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Absent means enabled — the opposite default from the JSON field's `false`.
    let default_features = spec
        .get("default-features")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let package = spec
        .get("package")
        .and_then(|v| v.as_str())
        .map(String::from);
    // A dependency from another registry names it by index URL; one from this
    // registry leaves the field null.
    let registry = spec
        .get("registry-index")
        .and_then(|v| v.as_str())
        .map(String::from);

    serde_json::json!({
        "name": alias,
        "req": req,
        "features": features,
        "optional": optional,
        "default_features": default_features,
        "target": target,
        "kind": kind,
        "registry": registry,
        "package": package,
    })
}

/// The `[features]` table, split the way the index requires.
///
/// A feature whose value mentions `dep:foo` or `foo?/bar` uses syntax cargo
/// only learned in 1.60. Listing it under `features` makes an older client fail
/// on the whole entry; `features2` is the key such a client does not read, so
/// the split is what keeps both able to resolve the crate.
///
/// card_4df8ddf63daa: a value the index cannot carry is refused rather than
/// dropped. The feature table is resolver input exactly as `deps` is, and the
/// index is the only place cargo reads it from — so a manifest declaring
/// `default = ["std", 42]` used to publish as `default = ["std"]`, and one
/// declaring `default = 42` as `default = []`. Both are entries that resolve
/// cleanly against a feature graph the published `Cargo.toml` does not have,
/// and the client learns of it only at `unknown feature` or at a missing
/// optional dependency, far from the publish that caused it. Naming the feature
/// and the offending element at publish is the only point where the answer is
/// still cheap.
fn index_features(
    doc: &toml::Value,
) -> Result<(
    serde_json::Map<String, serde_json::Value>,
    serde_json::Map<String, serde_json::Value>,
)> {
    let mut features = serde_json::Map::new();
    let mut features2 = serde_json::Map::new();

    let Some(declared) = doc.get("features") else {
        return Ok((features, features2));
    };
    let table = declared.as_table().ok_or_else(|| {
        anyhow::anyhow!(
            "Cargo.toml `[features]` must be a table of feature lists, found {}",
            declared.type_str()
        )
    })?;

    for (name, declared) in table {
        let array = declared.as_array().ok_or_else(|| {
            anyhow::anyhow!(
                "Cargo.toml feature '{name}' must be an array of strings, found {}",
                declared.type_str()
            )
        })?;
        let values: Vec<&str> = array
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value.as_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Cargo.toml feature '{name}' entry {index} must be a string, found {}",
                        value.type_str()
                    )
                })
            })
            .collect::<Result<_>>()?;

        let needs_schema_2 = values
            .iter()
            .any(|v| v.starts_with("dep:") || v.contains("?/"));

        let values: Vec<serde_json::Value> = values.into_iter().map(Into::into).collect();
        if needs_schema_2 {
            features2.insert(name.clone(), values.into());
        } else {
            features.insert(name.clone(), values.into());
        }
    }

    Ok((features, features2))
}

/// The directory segments Cargo puts a crate under in the index.
///
/// A client never requests a crate by its bare name — the name is spelled out
/// as a prefix, so that no directory of the index ever holds more entries than
/// a filesystem is comfortable with:
///
/// | name length | path            | example        |
/// |-------------|-----------------|----------------|
/// | 1           | `1/{name}`      | `1/a`          |
/// | 2           | `2/{name}`      | `2/ab`         |
/// | 3           | `3/{c1}/{name}` | `3/a/abc`      |
/// | 4 and up    | `{c1c2}/{c3c4}/{name}` | `se/rd/serde` |
///
/// Returned as the segments *before* the name, lowercased the way Cargo spells
/// them; an empty name has no prefix. Non-ASCII names are not a real case
/// (Cargo rejects them at publish), but the split is by `char` so one cannot
/// panic here on a byte boundary.
pub fn cargo_index_prefix(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.to_lowercase().chars().collect();
    match chars.len() {
        0 => Vec::new(),
        1 => vec!["1".to_string()],
        2 => vec!["2".to_string()],
        3 => vec!["3".to_string(), chars[0].to_string()],
        _ => vec![chars[..2].iter().collect(), chars[2..4].iter().collect()],
    }
}

/// Build the sparse index's `config.json` (RFC 2789).
///
/// Cargo fetches this first, before any crate, and refuses the registry without
/// it. Only `dl` is required — where to download a `.crate` from. The markers
/// `{crate}` and `{version}` are substituted by Cargo, which lets the URL point
/// straight at ForgeKeep's existing package-download route instead of needing a
/// redirect endpoint of its own; the filename is the one `cargo package`
/// produces, `{crate}-{version}.crate`.
///
/// `api` is deliberately absent. It is the base for `cargo publish` / `yank` /
/// `owner`, and ForgeKeep serves none of those (publishing goes through
/// `POST .../packages/cargo/publish`). Advertising an `api` we do not implement
/// would turn `cargo publish` into an unexplained failure; without the key
/// Cargo says outright that the registry does not support the command.
pub fn build_cargo_index_config(base_url: &str, owner: &str, repo: &str) -> serde_json::Value {
    let base = base_url.trim_end_matches('/');
    serde_json::json!({
        "dl": format!(
            "{base}/api/v1/repos/{owner}/{repo}/packages/cargo/{{crate}}/{{version}}/{{crate}}-{{version}}.crate",
        ),
        // The base cargo appends its write API to: `{api}/api/v1/crates/new`,
        // `{api}/api/v1/crates/{crate}/{version}/yank`. Withheld until those
        // routes existed — a registry that advertises `api` without serving it
        // turns cargo's honest "registry does not support API commands" into a
        // silent 404 (card_5a790cc6ac35).
        "api": format!("{base}/api/v1/repos/{owner}/{repo}/packages/cargo"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    /// A `.crate` file is a gzipped tar carrying `{name}-{version}/Cargo.toml`.
    fn make_crate(manifest: &str) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        let bytes = manifest.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "matrix-crate-1.0.0/Cargo.toml", bytes)
            .unwrap();
        let tar = tar.into_inner().unwrap();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, &tar).unwrap();
        encoder.finish().unwrap()
    }

    /// The stored index fields of a manifest, as the adapter records them.
    fn stored_metadata(manifest: &str) -> serde_json::Value {
        let meta = CargoAdapter
            .extract_metadata("matrix-crate-1.0.0.crate", &make_crate(manifest))
            .unwrap();
        serde_json::from_str(&meta.protocol_metadata.expect("no protocol metadata")).unwrap()
    }

    /// The entry a client would read for that manifest.
    fn index_entry(manifest: &str) -> serde_json::Value {
        let stored = stored_metadata(manifest).to_string();
        build_sparse_index_entry(
            "matrix-crate",
            &CargoIndexVersion {
                version: "1.0.0",
                sha256: Some("deadbeef"),
                yanked: false,
                metadata: Some(&stored),
            },
        )
        .expect("the manifest's own metadata is readable")
    }

    #[test]
    fn index_entry_carries_the_manifest_dependencies() {
        let entry = index_entry(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"

[dependencies]
serde = "1.0"
rand = { version = "0.8", features = ["small_rng"], optional = true, default-features = false }
"#,
        );

        assert_eq!(
            entry["deps"],
            serde_json::json!([
                {
                    "name": "rand",
                    "req": "0.8",
                    "features": ["small_rng"],
                    "optional": true,
                    "default_features": false,
                    "target": null,
                    "kind": "normal",
                    "registry": null,
                    "package": null,
                },
                {
                    "name": "serde",
                    "req": "1.0",
                    "features": [],
                    "optional": false,
                    "default_features": true,
                    "target": null,
                    "kind": "normal",
                    "registry": null,
                    "package": null,
                },
            ]),
            "RFC 2789 dependency shape: {entry}"
        );
        // The rest of the entry is untouched by the overlay.
        assert_eq!(entry["cksum"], "deadbeef");
        assert_eq!(entry["vers"], "1.0.0");
        assert_eq!(entry["yanked"], false);
    }

    #[test]
    fn dev_and_build_and_target_dependencies_keep_their_kind() {
        let entry = index_entry(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"

[dev-dependencies]
tempfile = "3"

[build-dependencies]
cc = "1"

[target.'cfg(unix)'.dependencies]
nix = "0.27"
"#,
        );

        let deps = entry["deps"].as_array().unwrap();
        let find = |name: &str| {
            deps.iter()
                .find(|d| d["name"] == name)
                .unwrap_or_else(|| panic!("{name} missing from {entry}"))
        };
        assert_eq!(find("tempfile")["kind"], "dev");
        assert_eq!(find("cc")["kind"], "build");
        assert_eq!(find("nix")["kind"], "normal");
        assert_eq!(
            find("nix")["target"],
            "cfg(unix)",
            "the target expression travels verbatim"
        );
    }

    /// A renamed dependency keeps the alias as `name`; the crate actually
    /// fetched is the one in `package`. Swapped, cargo asks for a crate that
    /// does not exist.
    #[test]
    fn a_renamed_dependency_names_both_the_alias_and_the_crate() {
        let entry = index_entry(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"

[dependencies]
json = { version = "1.0", package = "serde_json", registry-index = "https://other.example/index" }
"#,
        );

        let dep = &entry["deps"][0];
        assert_eq!(dep["name"], "json");
        assert_eq!(dep["package"], "serde_json");
        assert_eq!(dep["registry"], "https://other.example/index");
    }

    /// A dependency that reached the index without a version requirement still
    /// belongs in the entry — dropping it would understate the crate's needs.
    #[test]
    fn a_dependency_without_a_requirement_matches_anything() {
        let entry = index_entry(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"

[dependencies]
local = { path = "../local" }
"#,
        );

        assert_eq!(entry["deps"][0]["name"], "local");
        assert_eq!(entry["deps"][0]["req"], "*");
    }

    #[test]
    fn features_reach_the_entry_and_new_syntax_goes_to_features2() {
        let entry = index_entry(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"

[dependencies]
rand = { version = "0.8", optional = true }

[features]
default = ["std"]
std = []
fast = ["dep:rand"]
maybe = ["rand?/small_rng"]
"#,
        );

        assert_eq!(
            entry["features"],
            serde_json::json!({ "default": ["std"], "std": [] }),
            "plain features stay where cargo before 1.60 reads them: {entry}"
        );
        assert_eq!(
            entry["features2"],
            serde_json::json!({ "fast": ["dep:rand"], "maybe": ["rand?/small_rng"] }),
        );
        assert_eq!(entry["v"], 2, "features2 is only legible under schema 2");
    }

    /// card_4df8ddf63daa: the feature table is resolver input, so a value the
    /// index cannot carry has to stop the publish. Dropping it publishes a
    /// feature graph the crate does not have — `["std", 42]` became `["std"]`
    /// and `42` became `[]`, both of which resolve before failing at the
    /// client. The refusal names the feature, and the element when there is one.
    #[test]
    fn a_feature_value_the_index_cannot_carry_refuses_the_manifest() {
        for (features, expected) in [
            ("default = [\"std\", 42]", "feature 'default' entry 1"),
            ("default = [42, \"std\"]", "feature 'default' entry 0"),
            ("default = 42", "feature 'default' must be an array"),
            ("default = \"std\"", "feature 'default' must be an array"),
            ("default = [[\"std\"]]", "feature 'default' entry 0"),
            ("std = []\nfast = { dep = \"rand\" }", "feature 'fast'"),
        ] {
            let manifest = format!(
                r#"[package]
name = "matrix-crate"
version = "1.0.0"

[features]
{features}
"#
            );
            let error = CargoAdapter
                .extract_metadata("matrix-crate-1.0.0.crate", &make_crate(&manifest))
                .err()
                .unwrap_or_else(|| panic!("`{features}` must not publish"));
            let error = format!("{error:#}");
            assert!(
                error.contains(expected),
                "`{features}` must name what it refused, got: {error}"
            );
            // The same manifest through the gate every publish runs.
            assert!(
                CargoAdapter.validate(&make_crate(&manifest)).is_err(),
                "`{features}` reached the registry through `validate`"
            );
        }
    }

    /// A `[features]` key that is not a table at all is the same lie one level
    /// up: every feature the crate declares would vanish at once.
    #[test]
    fn a_features_key_that_is_not_a_table_refuses_the_manifest() {
        // A root key has to precede the first table header to stay a root key —
        // written under `[package]` it would be `package.features` instead.
        let manifest = r#"features = "std"

[package]
name = "matrix-crate"
version = "1.0.0"
"#;

        let error = CargoAdapter
            .extract_metadata("matrix-crate-1.0.0.crate", &make_crate(manifest))
            .expect_err("a scalar `features` must not publish");
        let error = format!("{error:#}");
        assert!(
            error.contains("`[features]` must be a table"),
            "got: {error}"
        );
    }

    /// The discrimination: an empty list, an empty table and a table of proper
    /// string arrays are all legitimate and must still publish unchanged.
    #[test]
    fn a_well_formed_feature_table_still_publishes() {
        let stored = stored_metadata(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"

[features]
default = []
std = ["alloc"]
alloc = []
"#,
        );

        assert_eq!(
            stored["features"],
            serde_json::json!({ "default": [], "std": ["alloc"], "alloc": [] }),
        );
    }

    /// No `dep:` syntax anywhere means no `features2` and no `v` — an entry an
    /// old client reads exactly as a new one does.
    #[test]
    fn a_plain_manifest_stays_on_schema_1() {
        let entry = index_entry(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"

[features]
std = []
"#,
        );

        assert!(entry.get("features2").is_none(), "{entry}");
        assert!(entry.get("v").is_none(), "{entry}");
    }

    #[test]
    fn links_and_rust_version_come_from_the_package_section() {
        let entry = index_entry(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"
links = "openssl"
rust-version = "1.70"
"#,
        );

        assert_eq!(entry["links"], "openssl");
        assert_eq!(entry["rust_version"], "1.70");
    }

    /// A manifest with no dependency table records an empty list, so "no
    /// dependencies" is something the registry knows rather than something it
    /// failed to look up.
    #[test]
    fn a_manifest_without_dependencies_records_an_empty_list() {
        let stored = stored_metadata(
            r#"[package]
name = "matrix-crate"
version = "1.0.0"
"#,
        );

        assert_eq!(stored["deps"], serde_json::json!([]));
        assert_eq!(stored["features"], serde_json::json!({}));
        assert!(stored.get("links").is_none());
    }

    /// Versions published before the adapter stored index fields still serve a
    /// well-formed entry: nothing was ever recorded for them, so the
    /// placeholders are the honest answer rather than a claim.
    #[test]
    fn an_entry_without_stored_metadata_keeps_the_old_shape() {
        let entry = build_sparse_index_entry(
            "matrix-crate",
            &CargoIndexVersion {
                version: "1.0.0",
                sha256: None,
                yanked: true,
                metadata: None,
            },
        )
        .expect("a version with no stored metadata still has an entry");

        assert_eq!(entry["deps"], serde_json::json!([]));
        assert_eq!(entry["features"], serde_json::json!({}));
        assert_eq!(entry["cksum"], "");
        assert_eq!(entry["yanked"], true);
        assert!(entry["links"].is_null());
    }

    /// card_49d7caba8e4f: a row that *has* metadata which cannot be read is a
    /// different thing entirely. The placeholders would state, in cargo's own
    /// resolver input, that the crate depends on nothing — resolution succeeds
    /// on that and the build dies later at `unresolved import`. Refusing to
    /// serve the index is the smaller failure, and the only honest one.
    #[test]
    fn an_entry_whose_stored_metadata_cannot_be_read_is_refused() {
        for metadata in ["not json", "[1,2]", "null", "\"deps\""] {
            let refused = build_sparse_index_entry(
                "matrix-crate",
                &CargoIndexVersion {
                    version: "1.0.0",
                    sha256: None,
                    yanked: false,
                    metadata: Some(metadata),
                },
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

        // And the whole index refuses with it — one damaged version must not be
        // quietly dropped from a listing the client reads as complete.
        assert!(
            build_sparse_index(
                "matrix-crate",
                &[
                    CargoIndexVersion {
                        version: "1.0.0",
                        sha256: Some("aa"),
                        yanked: false,
                        metadata: Some(r#"{"deps":[]}"#),
                    },
                    CargoIndexVersion {
                        version: "1.1.0",
                        sha256: Some("bb"),
                        yanked: false,
                        metadata: Some("not json"),
                    },
                ],
            )
            .is_err(),
            "a damaged version must fail the index it belongs to"
        );
    }

    /// The metadata column is free-form JSON; only the index's own keys may
    /// reach a protocol response.
    #[test]
    fn a_stray_key_in_the_stored_blob_stays_out_of_the_entry() {
        let entry = build_sparse_index_entry(
            "matrix-crate",
            &CargoIndexVersion {
                version: "1.0.0",
                sha256: None,
                yanked: false,
                metadata: Some(r#"{"deps":[],"summary":"leaked"}"#),
            },
        )
        .expect("a readable blob is served");

        assert!(entry.get("summary").is_none(), "{entry}");
    }

    #[test]
    fn sparse_index_writes_one_line_per_version() {
        let body = build_sparse_index(
            "matrix-crate",
            &[
                CargoIndexVersion {
                    version: "1.0.0",
                    sha256: Some("aa"),
                    yanked: false,
                    metadata: Some(r#"{"deps":[{"name":"serde","req":"1.0"}]}"#),
                },
                CargoIndexVersion {
                    version: "1.1.0",
                    sha256: Some("bb"),
                    yanked: true,
                    metadata: None,
                },
            ],
        )
        .expect("both versions are readable");

        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "{body}");
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["deps"][0]["name"], "serde");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["deps"], serde_json::json!([]));
        assert_eq!(second["yanked"], true);
    }

    #[test]
    fn index_prefix_follows_the_name_length() {
        assert_eq!(cargo_index_prefix("a"), ["1"]);
        assert_eq!(cargo_index_prefix("ab"), ["2"]);
        assert_eq!(cargo_index_prefix("abc"), ["3", "a"]);
        assert_eq!(cargo_index_prefix("serde"), ["se", "rd"]);
        assert_eq!(cargo_index_prefix("matrix-cargo"), ["ma", "tr"]);
        // Cargo lowercases the path even though the manifest name may not be.
        assert_eq!(cargo_index_prefix("SerDe"), ["se", "rd"]);
        assert!(cargo_index_prefix("").is_empty());
    }

    #[test]
    fn index_config_carries_a_substitutable_download_url() {
        let config = build_cargo_index_config("https://forge.example/", "acme", "tools");
        let dl = config["dl"].as_str().unwrap();
        assert_eq!(
            dl,
            "https://forge.example/api/v1/repos/acme/tools/packages/cargo/{crate}/{version}/{crate}-{version}.crate"
        );
        // `api` is the base cargo appends its write API to, and it may only be
        // present while those routes answer — advertising it without them turns
        // cargo's honest "registry does not support API commands" into a silent
        // 404. It became honest in card_5a790cc6ac35; the round trip is proved
        // by `cargo_publishes_and_yanks_through_the_api_its_index_advertises`.
        assert_eq!(
            config["api"].as_str().unwrap(),
            "https://forge.example/api/v1/repos/acme/tools/packages/cargo"
        );
    }
}
