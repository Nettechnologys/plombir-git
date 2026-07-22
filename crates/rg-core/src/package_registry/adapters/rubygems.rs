//! RubyGems (Ruby) package adapter.
//!
//! Handles `.gem` files, which are tar archives containing:
//! - `metadata.gz` — gzipped YAML metadata (required)
//! - `data.tar.gz` — the actual gem contents
//! - `checksums.yaml.gz` — SHA-256 checksums
//!
//! ## RubyGems API
//!
//! RubyGems clients expect:
//! - `GET /api/v1/dependencies?gems={name}` — dependency resolution (Marshall/JSON)
//! - `GET /api/v1/gems/{name}.json` — gem metadata
//! - `GET /gems/{name}-{version}.gem` — gem download
//! - `POST /api/v1/gems` — gem push
//!
//! IronForge serves these at:
//! - Dependencies: `GET /api/v1/repos/{owner}/{repo}/packages/rubygems/api/v1/dependencies?gems={name}`
//! - Gem info:     `GET /api/v1/repos/{owner}/{repo}/packages/rubygems/api/v1/gems/{name}.json`
//! - Download:     (standard package download endpoint)

use flate2::read::GzDecoder;
use std::io::Read;

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct RubyGemsAdapter;

impl PackageAdapter for RubyGemsAdapter {
    fn package_type() -> &'static str {
        "rubygems"
    }

    fn extract_metadata(
        &self,
        _filename: &str,
        data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        extract_from_gem(data)
    }

    fn validate(&self, data: &[u8]) -> Result<(), anyhow::Error> {
        // .gem is a tar archive
        if data.len() < 512 {
            anyhow::bail!("file too small to be a valid RubyGem");
        }

        // Try to find metadata.gz inside the tar
        let mut archive = tar::Archive::new(data);
        let mut found_metadata = false;
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?;
            if path
                .file_name()
                .map(|n| n == "metadata.gz")
                .unwrap_or(false)
            {
                found_metadata = true;
                break;
            }
        }

        if !found_metadata {
            anyhow::bail!("invalid RubyGem: no metadata.gz found");
        }
        Ok(())
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        if filename.ends_with(".gem") {
            "application/octet-stream".into()
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

/// Extract metadata from a .gem file (tar containing metadata.gz).
fn extract_from_gem(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let mut archive = tar::Archive::new(data);

    let mut metadata_gz = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path
            .file_name()
            .map(|n| n == "metadata.gz")
            .unwrap_or(false)
        {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            metadata_gz = Some(buf);
            break;
        }
    }

    let gz_data =
        metadata_gz.ok_or_else(|| anyhow::anyhow!("invalid .gem: no metadata.gz found"))?;

    // Decompress metadata.gz
    let mut decoder = GzDecoder::new(&gz_data[..]);
    let mut yaml_str = String::new();
    decoder
        .read_to_string(&mut yaml_str)
        .map_err(|e| anyhow::anyhow!("invalid .gem metadata.gz: {e}"))?;

    parse_gemspec_yaml(&yaml_str)
}

/// Parse RubyGems metadata (YAML format).
///
/// The metadata.gz contains a YAML document with keys:
/// name, version, summary, description, homepage, licenses, authors, etc.
fn parse_gemspec_yaml(yaml: &str) -> Result<ExtractedMetadata, anyhow::Error> {
    let doc: serde_yaml::Value = serde_yaml::from_str(yaml)
        .map_err(|e| anyhow::anyhow!("invalid RubyGems metadata (not valid YAML): {e}"))?;

    let name = doc
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("RubyGems metadata missing 'name'"))?
        .to_string();

    let version = doc
        .get("version")
        .and_then(|v| {
            // version can be a string or a nested object with "version" key
            v.as_str().map(str::to_string).or_else(|| {
                v.get("version")
                    .and_then(|iv| iv.as_str())
                    .map(str::to_string)
            })
        })
        .ok_or_else(|| anyhow::anyhow!("RubyGems metadata missing 'version'"))?;

    let description = doc
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| doc.get("summary").and_then(|v| v.as_str()))
        .map(|s| {
            // Truncate long descriptions
            if s.len() > 500 {
                format!("{}...", &s[..500])
            } else {
                s.to_string()
            }
        });

    let homepage = doc
        .get("homepage")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let repository_url = doc
        .get("metadata")
        .and_then(|v| {
            v.get("source_code_uri")
                .or_else(|| v.get("homepage_uri"))
                .or_else(|| v.get("changelog_uri"))
                .and_then(|u| u.as_str())
        })
        .map(String::from);

    let license = doc
        .get("licenses")
        .and_then(|v| {
            v.as_sequence()
                .map(|seq| {
                    seq.iter()
                        .filter_map(|l| l.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            doc.get("license")
                .and_then(|v| v.as_str())
                .map(String::from)
        });

    let keywords = doc
        .get("metadata")
        .and_then(|v| v.get("tags").or_else(|| v.get("keywords")))
        .and_then(|v| {
            v.as_sequence()
                .map(|seq| {
                    seq.iter()
                        .filter_map(|t| t.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|s| !s.is_empty())
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

// ── RubyGems API helpers ──────────────────────────────────

/// Info for one gem version used in dependencies API response.
pub struct RubyGemsDependencyEntry {
    pub name: String,
    pub number: String,
    pub platform: String,
    pub dependencies: Vec<RubyGemsDep>,
}

/// Single dependency specification.
pub struct RubyGemsDep {
    pub name: String,
    pub requirements: String,
}

/// Build the RubyGems dependencies API JSON response.
///
/// This is the format expected by `gem install` / Bundler for dependency resolution.
/// Example response:
/// ```json
/// [{"name":"rack","number":"2.2.0","platform":"ruby","dependencies":[]}]
/// ```
pub fn build_dependencies_json(entries: &[RubyGemsDependencyEntry]) -> serde_json::Value {
    let deps: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            let deps_list: Vec<serde_json::Value> = e
                .dependencies
                .iter()
                .map(|d| serde_json::json!([d.name, d.requirements]))
                .collect();

            serde_json::json!({
                "name": e.name,
                "number": e.number,
                "platform": e.platform,
                "dependencies": deps_list,
            })
        })
        .collect();

    serde_json::Value::Array(deps)
}

/// Gem info entry for the version info API.
pub struct RubyGemsVersionEntry {
    pub number: String,
    pub platform: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub sha256: Option<String>,
    pub download_url: String,
    pub gem_uri: String,
    pub created_at: String,
}

/// Build the RubyGems gem info JSON response.
pub fn build_gem_info_json(name: &str, entries: &[RubyGemsVersionEntry]) -> serde_json::Value {
    let mut version_map = serde_json::Map::new();
    for e in entries {
        let mut ver = serde_json::Map::new();
        ver.insert("name".into(), name.into());
        ver.insert("number".into(), e.number.clone().into());
        ver.insert("platform".into(), e.platform.clone().into());
        if let Some(ref s) = e.summary {
            ver.insert("summary".into(), s.clone().into());
        }
        if let Some(ref d) = e.description {
            ver.insert("description".into(), d.clone().into());
        }
        if let Some(ref h) = e.homepage {
            ver.insert("homepage_uri".into(), h.clone().into());
        }
        if let Some(ref l) = e.license {
            ver.insert("licenses".into(), vec![l.clone()].into());
        }
        if let Some(ref sha) = e.sha256 {
            ver.insert("sha".into(), sha.clone().into());
        }
        ver.insert("downloads".into(), 0.into());
        ver.insert("version_downloads".into(), 0.into());
        ver.insert("gem_uri".into(), e.gem_uri.clone().into());

        version_map.insert(e.number.clone(), serde_json::Value::Object(ver));
    }

    serde_json::json!({
        "name": name,
        "version": entries.first().map(|e| e.number.clone()).unwrap_or_default(),
        "version_downloads": 0,
        "downloads": 0,
        "versions": version_map,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    /// Create a minimal .gem file for testing.
    /// A .gem is a tar archive containing metadata.gz (gzipped YAML).
    fn make_gem(metadata_yaml: &str) -> Vec<u8> {
        // Gzip the YAML metadata
        let mut gz_buf = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut gz_buf, Compression::default());
            encoder.write_all(metadata_yaml.as_bytes()).unwrap();
            encoder.finish().unwrap();
        }

        // Create a tar archive with metadata.gz entry
        let mut tar_buf = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_path("metadata.gz").unwrap();
            header.set_size(gz_buf.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append(&header, &gz_buf[..]).unwrap();
            tar.finish().unwrap();
        }

        tar_buf
    }

    #[test]
    fn test_extract_gem_basic() {
        let yaml = r#"--- !ruby/object:Gem::Specification
name: rack
version: !ruby/object:Gem::Version
  version: "2.2.4"
summary: A modular Ruby webserver interface
description: Rack provides a minimal interface between webservers and Ruby frameworks.
homepage: https://github.com/rack/rack
licenses:
- MIT
metadata:
  source_code_uri: https://github.com/rack/rack
"#;

        let data = make_gem(yaml);
        let adapter = RubyGemsAdapter;

        let meta = adapter.extract_metadata("rack-2.2.4.gem", &data).unwrap();
        assert_eq!(meta.name, "rack");
        assert_eq!(meta.version, "2.2.4");
        assert!(meta.description.unwrap().contains("Rack provides"));
        assert_eq!(meta.homepage.unwrap(), "https://github.com/rack/rack");
        assert_eq!(meta.license.unwrap(), "MIT");
    }

    #[test]
    fn test_validate_valid_gem() {
        let yaml = "name: foo\nversion: 1.0.0\n";
        let data = make_gem(yaml);
        let adapter = RubyGemsAdapter;
        assert!(adapter.validate(&data).is_ok());
    }

    #[test]
    fn test_validate_rejects_small_file() {
        let adapter = RubyGemsAdapter;
        let err = adapter.validate(b"tiny").unwrap_err();
        assert!(err.to_string().contains("too small"));
    }

    #[test]
    fn test_validate_rejects_no_metadata() {
        // Create a tar without metadata.gz
        let mut tar_buf = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_path("data.tar.gz").unwrap();
            header.set_size(10);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append(&header, &b"0123456789"[..]).unwrap();
            tar.finish().unwrap();
        }

        let adapter = RubyGemsAdapter;
        let err = adapter.validate(&tar_buf).unwrap_err();
        assert!(err.to_string().contains("no metadata.gz"));
    }

    #[test]
    fn test_extract_gem_with_summary_fallback() {
        let yaml = r#"---
name: mygem
version: "0.1.0"
summary: Short summary
description: ""
"#;

        let data = make_gem(yaml);
        let adapter = RubyGemsAdapter;
        let meta = adapter.extract_metadata("mygem-0.1.0.gem", &data).unwrap();
        // Empty description should fall back to summary
        assert_eq!(meta.description.unwrap(), "Short summary");
    }

    #[test]
    fn test_build_dependencies_json() {
        let entries = vec![RubyGemsDependencyEntry {
            name: "rack".into(),
            number: "2.2.4".into(),
            platform: "ruby".into(),
            dependencies: vec![RubyGemsDep {
                name: "activesupport".into(),
                requirements: ">= 5.0".into(),
            }],
        }];

        let json = build_dependencies_json(&entries);
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "rack");
        assert_eq!(arr[0]["number"], "2.2.4");
        let deps = arr[0]["dependencies"].as_array().unwrap();
        assert_eq!(deps[0][0], "activesupport");
        assert_eq!(deps[0][1], ">= 5.0");
    }

    #[test]
    fn test_build_gem_info_json() {
        let entries = vec![RubyGemsVersionEntry {
            number: "1.0.0".into(),
            platform: "ruby".into(),
            summary: Some("A gem".into()),
            description: Some("Full desc".into()),
            homepage: Some("https://example.com".into()),
            license: Some("MIT".into()),
            sha256: Some("abc123".into()),
            download_url: "https://example.com/dl".into(),
            gem_uri: "https://example.com/gems/x-1.0.0.gem".into(),
            created_at: "2024-01-01".into(),
        }];

        let json = build_gem_info_json("mygem", &entries);
        assert_eq!(json["name"], "mygem");
        assert_eq!(json["version"], "1.0.0");
        let versions = json["versions"].as_object().unwrap();
        assert!(versions.contains_key("1.0.0"));

        let v1 = &versions["1.0.0"];
        assert_eq!(v1["number"], "1.0.0");
        assert_eq!(v1["summary"], "A gem");
        assert_eq!(v1["sha"], "abc123");
    }
}
