//! NuGet (.NET) package adapter.
//!
//! Handles `.nupkg` files, which are ZIP archives containing:
//! - `{name}.nuspec` — XML metadata (required)
//! - `lib/` — assemblies
//! - `content/` — content files
//!
//! ## NuGet API v3
//!
//! NuGet clients expect the Service Index at:
//!   `GET /api/v3/index.json`
//!
//! The Service Index advertises resource URLs:
//! - `PackageBaseAddress/3.0.0` — download packages
//! - `RegistrationsBaseUrl/3.6.0` — registration index
//! - `SearchQueryService/3.5.0` — search endpoint
//!
//! IronForge serves these at:
//! - Service Index:  `GET /api/v1/repos/{owner}/{repo}/packages/nuget/index.json`
//! - Registration:   `GET /api/v1/repos/{owner}/{repo}/packages/nuget/registration/{id}/index.json`
//! - Search:         `GET /api/v1/repos/{owner}/{repo}/packages/nuget/query?q=...`
//! - Package Content: `GET /api/v1/repos/{owner}/{repo}/packages/nuget/{name}/{version}/{file}`

use std::io::{Cursor, Read};

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct NuGetAdapter;

impl PackageAdapter for NuGetAdapter {
    fn package_type() -> &'static str {
        "nuget"
    }

    fn extract_metadata(
        &self,
        _filename: &str,
        data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        extract_from_nupkg(data)
    }

    fn validate(&self, data: &[u8]) -> Result<(), anyhow::Error> {
        // .nupkg is just a ZIP file; must contain a .nuspec
        if data.len() < 4 || &data[0..4] != b"PK\x03\x04" {
            anyhow::bail!("invalid NuGet package: not a valid ZIP");
        }

        // Find .nuspec inside
        let cursor = Cursor::new(data);
        let mut archive = zip::ZipArchive::new(cursor)
            .map_err(|e| anyhow::anyhow!("invalid NuGet package (corrupt ZIP): {e}"))?;

        let mut found_nuspec = false;
        for i in 0..archive.len() {
            let entry = archive
                .by_index(i)
                .map_err(|e| anyhow::anyhow!("failed to read ZIP entry: {e}"))?;
            let name = entry.name().to_lowercase();
            if name.ends_with(".nuspec") {
                found_nuspec = true;
                break;
            }
        }

        if !found_nuspec {
            anyhow::bail!("invalid NuGet package: no .nuspec found");
        }
        Ok(())
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        let lower = filename.to_lowercase();
        if lower.ends_with(".nupkg") {
            "application/zip".into()
        } else if lower.ends_with(".nuspec") {
            "application/xml".into()
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

/// Extract a simple XML tag value (no attributes).
fn xml_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);

    // Case-insensitive search
    if let Some(start) = xml.to_lowercase().find(&open.to_lowercase()) {
        let start = start + open.len();
        if let Some(end) = xml[start..].to_lowercase().find(&close.to_lowercase()) {
            return Some(xml[start..start + end].trim().to_string());
        }
    }
    None
}

/// Extract an XML tag value that may have attributes (e.g. `<license type="expr">`).
/// Ensures we don't match `<licenseUrl>` when looking for `<license>`.
fn xml_tag_value_with_attrs(xml: &str, tag: &str) -> Option<String> {
    let lower = xml.to_lowercase();
    let open_prefix = format!("<{}", tag);
    let close = format!("</{}>", tag);

    // Search for all occurrences of the prefix; only accept if the next char after <tag
    // is a non-alpha char (space, >, /, etc.) to avoid matching prefixes like <licenseUrl>
    let mut search_from = 0usize;
    while let Some(tag_start) = lower[search_from..].find(&open_prefix) {
        let abs_start = search_from + tag_start;
        let after_tag = abs_start + open_prefix.len();
        // Check that the character after <tag is a terminator, not a continuation letter
        let next_char = lower.as_bytes().get(after_tag).copied().unwrap_or(b'>');
        if next_char.is_ascii_alphabetic() {
            // This matched a longer tag name (e.g. <licenseUrl); skip past it
            search_from = after_tag;
            continue;
        }

        // Find the end of the opening tag (> character)
        if let Some(tag_content_start) = lower[abs_start..].find('>') {
            let content_start = abs_start + tag_content_start + 1;
            if let Some(closing_match) = lower[content_start..].find(&close) {
                return Some(
                    xml[content_start..content_start + closing_match]
                        .trim()
                        .to_string(),
                );
            }
        }
        return None;
    }
    None
}

/// Parse metadata from a .nupkg file (ZIP containing .nuspec).
fn extract_from_nupkg(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| anyhow::anyhow!("invalid .nupkg (not a valid ZIP): {e}"))?;

    let mut nuspec_content = None;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| anyhow::anyhow!("failed to read .nupkg entry {}: {e}", i))?;
        let name = entry.name().to_lowercase();
        if name.ends_with(".nuspec") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            nuspec_content = Some(content);
            break;
        }
    }

    let nuspec =
        nuspec_content.ok_or_else(|| anyhow::anyhow!("invalid .nupkg: no .nuspec file found"))?;

    extract_from_nuspec(&nuspec)
}

/// Parse a .nuspec XML string into ExtractedMetadata.
fn extract_from_nuspec(xml: &str) -> Result<ExtractedMetadata, anyhow::Error> {
    // NuGet .nuspec uses <metadata> element containing:
    // <id>, <version>, <title>, <description>, <projectUrl>, <licenseUrl>,
    // <tags>, <authors>, <repository type="git" url="..." />

    let id =
        xml_tag_value(xml, "id").ok_or_else(|| anyhow::anyhow!(".nuspec missing <id> element"))?;

    let version = xml_tag_value(xml, "version")
        .ok_or_else(|| anyhow::anyhow!(".nuspec missing <version> element"))?;

    let description = xml_tag_value(xml, "description")
        .or_else(|| xml_tag_value(xml, "summary"))
        .or_else(|| xml_tag_value(xml, "title"));

    let homepage = xml_tag_value(xml, "projectUrl");

    let repository_url =
        xml_tag_value(xml, "repository").or_else(|| extract_repository_url_from_xml(xml));

    let license =
        xml_tag_value_with_attrs(xml, "license").or_else(|| xml_tag_value(xml, "licenseUrl"));

    let keywords = xml_tag_value(xml, "tags");

    Ok(ExtractedMetadata {
        name: id,
        version: version.clone(),
        description,
        homepage,
        repository_url,
        keywords,
        license,
        semver: Some(version),
    })
}

/// Try to extract repository URL from <repository type="git" url="..." /> element.
fn extract_repository_url_from_xml(xml: &str) -> Option<String> {
    let lower = xml.to_lowercase();
    let repo_tag_start = lower.find("<repository")?;
    let repo_tag_end = lower[repo_tag_start..].find("/>")?;

    // Look for url="..." attribute
    let attr_search = &lower[repo_tag_start..repo_tag_start + repo_tag_end];
    let url_attr_start = attr_search.find("url=\"")?;

    let value_start = repo_tag_start + url_attr_start + 5; // skip 'url="'
    let value_rest = &xml[value_start..];
    let value_end = value_rest.find('"')?;

    Some(value_rest[..value_end].to_string())
}

// ── NuGet API v3 helpers ──────────────────────────────────

/// Build the NuGet Service Index JSON response.
///
/// This advertises all available NuGet API resources for the repository.
pub fn build_service_index(base_url: &str, owner: &str, repo: &str) -> serde_json::Value {
    let prefix = format!(
        "{}/api/v1/repos/{}/{}/packages/nuget",
        base_url.trim_end_matches('/'),
        owner,
        repo,
    );

    serde_json::json!({
        "version": "3.0.0",
        "resources": [
            {
                "@id": format!("{}/package/", prefix),
                "@type": "PackageBaseAddress/3.0.0",
                "comment": "Package content download"
            },
            {
                "@id": format!("{}/registration/", prefix),
                "@type": "RegistrationsBaseUrl/3.6.0",
                "comment": "Registration index for package metadata"
            },
            {
                "@id": format!("{}/query", prefix),
                "@type": "SearchQueryService/3.5.0",
                "comment": "Search NuGet packages"
            },
            {
                "@id": format!("{}/publish", prefix),
                "@type": "PackagePublish/2.0.0",
                "comment": "Push NuGet packages"
            },
            {
                "@id": format!("{}/", prefix),
                "@type": "SearchAutocompleteService/3.5.0",
                "comment": "Autocomplete package IDs"
            }
        ]
    })
}

/// Info for a NuGet registration page.
pub struct NuGetRegistrationEntry {
    pub version: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub tags: Option<String>,
    pub download_url: String,
    pub nuspec_url: Option<String>,
}

/// Build the NuGet Registration Index JSON (3.6.0 format).
///
/// Returns a single-page registration which lists all versions.
pub fn build_registration_index(
    package_name: &str,
    entries: &[NuGetRegistrationEntry],
) -> serde_json::Value {
    let mut leaves = Vec::new();
    for e in entries {
        let mut leaf = serde_json::json!({
            "packageContent": e.download_url,
            "registration": e.nuspec_url.as_ref().unwrap_or(&String::new()),
        });

        if let Some(ref catalog_entry) = build_catalog_entry(
            package_name,
            &e.version,
            e.description.as_deref(),
            e.homepage.as_deref(),
            e.license.as_deref(),
            e.tags.as_deref(),
        ) {
            leaf["catalogEntry"] = catalog_entry.clone();
        }

        leaves.push(leaf);
    }

    serde_json::json!({
        "count": 1,
        "items": [{
            "@id": "",
            "count": leaves.len(),
            "lower": entries.first().map(|e| e.version.clone()).unwrap_or_default(),
            "upper": entries.last().map(|e| e.version.clone()).unwrap_or_default(),
            "items": leaves,
        }]
    })
}

fn build_catalog_entry(
    name: &str,
    version: &str,
    description: Option<&str>,
    homepage: Option<&str>,
    license: Option<&str>,
    tags: Option<&str>,
) -> Option<serde_json::Value> {
    let mut entry = serde_json::Map::new();
    entry.insert("@id".into(), "".into());
    entry.insert("id".into(), name.into());
    entry.insert("version".into(), version.into());
    if let Some(d) = description {
        entry.insert("description".into(), d.into());
    }
    if let Some(h) = homepage {
        entry.insert("projectUrl".into(), h.into());
    }
    if let Some(l) = license {
        entry.insert("licenseUrl".into(), l.into());
    }
    if let Some(t) = tags {
        entry.insert(
            "tags".into(),
            t.split(&[',', ' '][..])
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .into(),
        );
    }
    Some(serde_json::Value::Object(entry))
}

/// Build the NuGet Search Query response (3.5.0 format).
pub fn build_search_results(results: &[NuGetSearchResult], total_hits: usize) -> serde_json::Value {
    let data: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let mut item = serde_json::Map::new();
            item.insert("@id".into(), r.registration_url.clone().into());
            item.insert("@type".into(), "Package".into());
            item.insert("id".into(), r.name.clone().into());
            item.insert("version".into(), r.version.clone().into());
            if let Some(ref d) = r.description {
                item.insert("description".into(), d.clone().into());
            }
            if let Some(ref t) = r.tags {
                let tags: Vec<&str> = t.split(&[',', ' '][..]).filter(|s| !s.is_empty()).collect();
                item.insert("tags".into(), tags.into());
            }
            item.insert(
                "versions".into(),
                serde_json::json!([{
                    "version": r.version,
                    "downloads": 0,
                }]),
            );
            item.insert("totalDownloads".into(), serde_json::Value::Number(0.into()));
            item.insert("verified".into(), serde_json::Value::Bool(false));
            serde_json::Value::Object(item)
        })
        .collect();

    serde_json::json!({
        "totalHits": total_hits,
        "data": data,
    })
}

/// Search result entry for NuGet search API.
pub struct NuGetSearchResult {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub tags: Option<String>,
    pub registration_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Create a minimal .nupkg (ZIP with .nuspec) for testing.
    fn make_nupkg(nuspec: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("TestPackage.nuspec", options).unwrap();
            zip.write_all(nuspec.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn test_extract_nuspec_basic() {
        let nuspec = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>MyLib</id>
    <version>1.2.3</version>
    <description>A test library for unit testing</description>
    <projectUrl>https://github.com/user/mylib</projectUrl>
    <tags>testing utility</tags>
  </metadata>
</package>"#;

        let data = make_nupkg(nuspec);
        let adapter = NuGetAdapter;

        let meta = adapter
            .extract_metadata("MyLib.1.2.3.nupkg", &data)
            .unwrap();
        assert_eq!(meta.name, "MyLib");
        assert_eq!(meta.version, "1.2.3");
        assert_eq!(meta.description.unwrap(), "A test library for unit testing");
        assert_eq!(meta.homepage.unwrap(), "https://github.com/user/mylib");
        assert_eq!(meta.keywords.unwrap(), "testing utility");
    }

    #[test]
    fn test_validate_valid_nupkg() {
        let nuspec = r#"<?xml version="1.0"?>
<package>
  <metadata>
    <id>Foo</id>
    <version>1.0.0</version>
  </metadata>
</package>"#;

        let data = make_nupkg(nuspec);
        let adapter = NuGetAdapter;
        assert!(adapter.validate(&data).is_ok());
    }

    #[test]
    fn test_validate_invalid_rejects_non_zip() {
        let adapter = NuGetAdapter;
        let err = adapter.validate(b"not a zip file at all").unwrap_err();
        assert!(err.to_string().contains("not a valid ZIP"));
    }

    #[test]
    fn test_service_index() {
        let json = build_service_index("https://git.example.com", "alice", "mylib");
        let resources = json["resources"].as_array().unwrap();

        assert_eq!(json["version"], "3.0.0");

        // Should have at least the major resource types
        let types: Vec<&str> = resources
            .iter()
            .map(|r| r["@type"].as_str().unwrap())
            .collect();
        assert!(types.contains(&"PackageBaseAddress/3.0.0"));
        assert!(types.contains(&"RegistrationsBaseUrl/3.6.0"));
        assert!(types.contains(&"SearchQueryService/3.5.0"));
    }

    #[test]
    fn test_registration_index() {
        let entries = vec![
            NuGetRegistrationEntry {
                version: "1.0.0".into(),
                description: Some("First release".into()),
                homepage: None,
                license: Some("MIT".into()),
                tags: Some("test".into()),
                download_url: "https://git.example.com/dl/1.0.0".into(),
                nuspec_url: Some("https://git.example.com/dl/1.0.0.nuspec".into()),
            },
            NuGetRegistrationEntry {
                version: "2.0.0".into(),
                description: Some("Second release".into()),
                homepage: Some("https://example.com".into()),
                license: None,
                tags: None,
                download_url: "https://git.example.com/dl/2.0.0".into(),
                nuspec_url: None,
            },
        ];

        let json = build_registration_index("MyLib", &entries);
        assert_eq!(json["count"], 1);

        let items = json["items"][0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);

        // First entry should have packageContent and catalogEntry
        assert_eq!(
            items[0]["packageContent"],
            "https://git.example.com/dl/1.0.0"
        );
        assert!(items[0]["catalogEntry"].is_object());
        assert_eq!(items[0]["catalogEntry"]["id"], "MyLib");
        assert_eq!(items[0]["catalogEntry"]["version"], "1.0.0");
    }

    #[test]
    fn test_search_results() {
        let results = vec![NuGetSearchResult {
            name: "Newtonsoft.Json".into(),
            version: "13.0.3".into(),
            description: Some("Json.NET".into()),
            tags: Some("json serializer".into()),
            registration_url: "https://example.com/reg".into(),
        }];

        let json = build_search_results(&results, 1);
        assert_eq!(json["totalHits"], 1);

        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], "Newtonsoft.Json");
        assert_eq!(data[0]["version"], "13.0.3");
    }

    #[test]
    fn test_extract_nuspec_with_license() {
        let nuspec = r#"<?xml version="1.0" encoding="utf-8"?>
<package>
  <metadata>
    <id>LicensedLib</id>
    <version>2.0.0</version>
    <license type="expression">MIT</license>
    <licenseUrl>https://opensource.org/licenses/MIT</licenseUrl>
  </metadata>
</package>"#;

        let data = make_nupkg(nuspec);
        let adapter = NuGetAdapter;
        let meta = adapter
            .extract_metadata("LicensedLib.2.0.0.nupkg", &data)
            .unwrap();
        // <license> tag should be preferred over <licenseUrl>
        assert_eq!(meta.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn test_extract_nuspec_missing_required_fields() {
        let nuspec = r#"<?xml version="1.0"?>
<package>
  <metadata>
    <title>No Id No Version</title>
  </metadata>
</package>"#;

        let data = make_nupkg(nuspec);
        let adapter = NuGetAdapter;
        let err = adapter.extract_metadata("test.nupkg", &data).unwrap_err();
        assert!(err.to_string().contains("missing <id>"));
    }
}
