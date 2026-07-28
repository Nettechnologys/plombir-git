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
            protocol_metadata: None,
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
    serde_json::json!({
        "dl": format!(
            "{}/api/v1/repos/{}/{}/packages/cargo/{{crate}}/{{version}}/{{crate}}-{{version}}.crate",
            base_url.trim_end_matches('/'),
            owner,
            repo,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // No `api`: `cargo publish` must fail loudly, not against a dead URL.
        assert!(config.get("api").is_none());
    }
}
