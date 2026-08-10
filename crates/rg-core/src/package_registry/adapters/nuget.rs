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
//! ForgeKeep serves these at:
//! - Service Index:  `GET /api/v1/repos/{owner}/{repo}/packages/nuget/index.json`
//! - Registration:   `GET /api/v1/repos/{owner}/{repo}/packages/nuget/registration/{id}/index.json`
//! - Search:         `GET /api/v1/repos/{owner}/{repo}/packages/nuget/query?q=...`
//! - Package Content: `GET /api/v1/repos/{owner}/{repo}/packages/nuget/{name}/{version}/{file}`

use std::{
    cmp::Ordering,
    io::{Cursor, Read},
};

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

        // Read the .nuspec, don't merely find it — see `CargoAdapter::validate`.
        // A `.nuspec` with no `<id>` is a package NuGet cannot resolve, and
        // `extract_from_nupkg` is where that is decided; it reports the absent
        // file too (`no .nuspec found in package`).
        extract_from_nupkg(data)?;
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

/// Case-insensitive search for an ASCII `needle`, as a byte offset into
/// `haystack`.
///
/// A normalized copy cannot safely supply offsets into the original: Unicode
/// case folding may change the byte length. This compares in place, so every
/// returned offset belongs to the string that produced it. Every needle here
/// begins with `<`, `</` or a space, so the offsets always land on an ASCII
/// byte and slicing is safe.
fn find_ascii_ci(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let (hay, pin) = (haystack.as_bytes(), needle.as_bytes());
    if pin.is_empty() || hay.len() < pin.len() || from > hay.len() - pin.len() {
        return None;
    }
    (from..=hay.len() - pin.len()).find(|&at| {
        hay[at..at + pin.len()]
            .iter()
            .zip(pin)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

/// One element named `name`, found at or after `from`.
struct XmlElement<'a> {
    /// The text between the element name and the `>` that ends its start tag.
    attrs: &'a str,
    /// The element's content — empty for a self-closing element.
    inner: &'a str,
    /// Byte offset just past the element, to resume scanning from.
    end: usize,
}

/// Scan out the next `<name …>` element.
///
/// The workspace carries no XML parser and a `.nuspec` is small and
/// machine-written, so this is a scanner rather than a parse: it honours the
/// self-closing form, refuses to mistake `<dependencies>` for `<dependency>`,
/// and reads content up to the first matching end tag. Same-name nesting is not
/// supported, and a nuspec's dependency block has none.
fn xml_element_at<'a>(xml: &'a str, name: &str, from: usize) -> Option<XmlElement<'a>> {
    let open = format!("<{name}");
    let mut cursor = from;
    loop {
        let start = find_ascii_ci(xml, &open, cursor)?;
        let after = start + open.len();
        // `<dependency` must not match the `<dependencies>` that contains it.
        if xml
            .as_bytes()
            .get(after)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            cursor = after;
            continue;
        }

        let tag_end = start + xml[start..].find('>')?;
        let raw = xml[after..tag_end].trim_end();
        let self_closing = raw.ends_with('/');
        let attrs = raw.trim_end_matches('/');
        if self_closing {
            return Some(XmlElement {
                attrs,
                inner: "",
                end: tag_end + 1,
            });
        }

        let content_start = tag_end + 1;
        let close = format!("</{name}>");
        let inner_len = find_ascii_ci(xml, &close, content_start)? - content_start;
        return Some(XmlElement {
            attrs,
            inner: &xml[content_start..content_start + inner_len],
            end: content_start + inner_len + close.len(),
        });
    }
}

/// One attribute of a start tag, by name.
fn xml_attr(attrs: &str, name: &str) -> Option<String> {
    let mut cursor = 0;
    loop {
        let at = find_ascii_ci(attrs, name, cursor)?;
        // The name has to stand on its own: `version` must not be read out of
        // `minVersion`, nor out of the value of an earlier attribute.
        let standalone = at == 0
            || attrs.as_bytes()[at - 1].is_ascii_whitespace()
            || attrs.as_bytes()[at - 1] == b'<';
        let rest = attrs[at + name.len()..].trim_start();
        if !standalone || !rest.starts_with('=') {
            cursor = at + name.len();
            continue;
        }

        let value = rest[1..].trim_start();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let value = &value[1..];
        let end = value.find(quote)?;
        return Some(value[..end].trim().to_string());
    }
}

/// One `<dependency>` of a nuspec, as the registration index publishes it.
#[derive(Debug, Clone, PartialEq)]
pub struct NuGetDependency {
    pub id: String,
    /// The version range, passed through as the nuspec spelled it. NuGet reads
    /// a bare `1.2.3` as `[1.2.3, )` and a bracketed form literally, so
    /// re-spelling it here would only risk saying something the author did not.
    pub range: String,
}

/// One `targetFramework` group of a nuspec's `<dependencies>` block.
///
/// A group with no dependencies is a fact, not an empty result: it declares
/// that the framework is supported and needs nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct NuGetDependencyGroup {
    pub target_framework: Option<String>,
    pub dependencies: Vec<NuGetDependency>,
}

/// The dependency groups a `.nuspec` declares.
///
/// `dotnet restore` builds its graph out of the registration index, not out of
/// the `.nupkg` — so a leaf without these is read as a package that genuinely
/// depends on nothing, restore goes green, and the build fails on a missing
/// assembly instead (card_b21fb6511a25).
///
/// Two shapes are accepted because both are in the wild: the modern one groups
/// dependencies under `<group targetFramework=…>`, and the pre-2.0 one lists
/// them flat. A flat list is returned as one group with no framework, which is
/// exactly what it means.
fn nuspec_dependency_groups(xml: &str) -> Vec<NuGetDependencyGroup> {
    let Some(block) = xml_element_at(xml, "dependencies", 0) else {
        return Vec::new();
    };

    let mut groups = Vec::new();
    let mut cursor = 0;
    while let Some(group) = xml_element_at(block.inner, "group", cursor) {
        cursor = group.end;
        groups.push(NuGetDependencyGroup {
            target_framework: xml_attr(group.attrs, "targetFramework").filter(|f| !f.is_empty()),
            dependencies: nuspec_dependencies(group.inner),
        });
    }

    if groups.is_empty() {
        let flat = nuspec_dependencies(block.inner);
        if !flat.is_empty() {
            groups.push(NuGetDependencyGroup {
                target_framework: None,
                dependencies: flat,
            });
        }
    }

    groups
}

/// The `<dependency>` elements directly inside one block.
fn nuspec_dependencies(xml: &str) -> Vec<NuGetDependency> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(dep) = xml_element_at(xml, "dependency", cursor) {
        cursor = dep.end;
        let Some(id) = xml_attr(dep.attrs, "id").filter(|id| !id.is_empty()) else {
            continue;
        };
        out.push(NuGetDependency {
            id,
            // An absent `version` means "any", which NuGet spells as an
            // unbounded range rather than as an empty string.
            range: xml_attr(dep.attrs, "version")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "(, )".to_string()),
        });
    }
    out
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

    let simple_value = |name: &str| {
        xml_element_at(xml, name, 0)
            .filter(|element| element.attrs.trim().is_empty())
            .map(|element| element.inner.trim().to_string())
    };

    let id = simple_value("id").ok_or_else(|| anyhow::anyhow!(".nuspec missing <id> element"))?;

    let version = simple_value("version")
        .ok_or_else(|| anyhow::anyhow!(".nuspec missing <version> element"))?;

    let description = simple_value("description")
        .or_else(|| simple_value("summary"))
        .or_else(|| simple_value("title"));

    let homepage = simple_value("projectUrl");

    let repository_url = xml_element_at(xml, "repository", 0).and_then(|element| {
        if element.attrs.trim().is_empty() {
            Some(element.inner.trim().to_string())
        } else {
            xml_attr(element.attrs, "url")
        }
    });

    let license = xml_element_at(xml, "license", 0)
        .map(|element| element.inner.trim().to_string())
        .or_else(|| simple_value("licenseUrl"));

    let keywords = simple_value("tags");

    // The registration index reads these back out of the stored metadata rather
    // than off the package row, because they describe one *version* — a later
    // release may drop a tag or change its licence, and the row only ever holds
    // whatever the first publish said.
    let protocol_metadata = nuspec_protocol_metadata(
        description.as_deref(),
        homepage.as_deref(),
        license.as_deref(),
        keywords.as_deref(),
        &nuspec_dependency_groups(xml),
    );

    Ok(ExtractedMetadata {
        name: id,
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

/// The per-version nuspec fields, keyed the way `parse_nuget_metadata`
/// (rg-http) reads them. `None` when the nuspec carried none of them, so an
/// empty object is never stored in place of a real absence.
fn nuspec_protocol_metadata(
    description: Option<&str>,
    project_url: Option<&str>,
    license: Option<&str>,
    tags: Option<&str>,
    dependency_groups: &[NuGetDependencyGroup],
) -> Option<String> {
    let fields = [
        ("description", description),
        ("projectUrl", project_url),
        ("license", license),
        ("tags", tags),
    ];

    let mut out = serde_json::Map::new();
    for (key, value) in fields {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            out.insert(key.into(), value.into());
        }
    }

    // Written only when the nuspec declared a `<dependencies>` block: an absent
    // key and an empty list both mean "no dependencies" to a client, but only
    // the empty list would claim we looked.
    if !dependency_groups.is_empty() {
        out.insert(
            "dependencyGroups".into(),
            dependency_groups
                .iter()
                .map(|group| {
                    serde_json::json!({
                        "targetFramework": group.target_framework,
                        "dependencies": group
                            .dependencies
                            .iter()
                            .map(|dep| serde_json::json!({ "id": dep.id, "range": dep.range }))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect::<Vec<_>>()
                .into(),
        );
    }

    (!out.is_empty()).then(|| serde_json::Value::Object(out).to_string())
}

// ── NuGet API v3 helpers ──────────────────────────────────

/// The spelling a NuGet client puts in a URL.
///
/// Package ids are case-insensitive and every v3 URL carries the *lowercase*
/// form: `dotnet restore` of `Matrix.NuGet` asks for `matrix.nuget`. The
/// registry stores the id as the nuspec spelled it, so a lookup on the literal
/// path segment misses every id that has a capital in it. Same shape as PyPI's
/// [`normalize_project_name`](super::pypi::normalize_project_name), simpler
/// rule: NuGet folds case and nothing else.
pub fn normalize_package_id(id: &str) -> String {
    id.to_lowercase()
}

/// The flat container's version list — `{id-lower}/index.json`.
///
/// This is the first hop of a restore: the client reads the available versions
/// here and only then asks for a `.nupkg`. Versions are lowercased because the
/// flat container addresses them in their normalized form.
pub fn build_flat_container_index(versions: &[String]) -> serde_json::Value {
    serde_json::json!({
        "versions": versions
            .iter()
            .map(|version| version.to_lowercase())
            .collect::<Vec<_>>(),
    })
}

/// The autocomplete answer — a bare list of package ids.
pub fn build_autocomplete_results(names: &[String], total_hits: usize) -> serde_json::Value {
    serde_json::json!({
        "@context": { "@vocab": "http://schema.nuget.org/schema#" },
        "totalHits": total_hits,
        "data": names,
    })
}

/// Build the NuGet Service Index JSON response.
///
/// This advertises all available NuGet API resources for the repository.
///
/// Every `@id` here has to be a path the router serves. It did not use to be:
/// `PackageBaseAddress` pointed at a flat container with no routes at all — so
/// `dotnet restore` could not download a package — and
/// `SearchAutocompleteService` pointed at the bare `nuget/` root, which is not
/// an endpoint of anything (card_dba77cceec56). The invariant is now held by a
/// test that walks every advertised `@id` and refuses a 404.
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
                "@id": format!("{}/autocomplete", prefix),
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
    /// What the nuspec's `<dependencies>` block declared. This is the graph
    /// `dotnet restore` resolves against — it never opens the `.nupkg` to find
    /// one (card_b21fb6511a25).
    pub dependency_groups: Vec<NuGetDependencyGroup>,
    /// Whether the version is offered as a candidate. A yanked version is still
    /// listed in the registration — that is how an already-locked consumer keeps
    /// restoring — but `listed: false` keeps it out of a fresh resolution.
    pub listed: bool,
}

/// Build the NuGet Registration Index JSON (3.6.0 format).
///
/// Returns a single-page registration which lists all versions.
pub fn build_registration_index(
    package_name: &str,
    registration_url: &str,
    entries: &[NuGetRegistrationEntry],
) -> serde_json::Value {
    let lower = entries
        .first()
        .map(|entry| entry.version.clone())
        .unwrap_or_default();
    let upper = entries
        .last()
        .map(|entry| entry.version.clone())
        .unwrap_or_default();
    // The page is inline, so its fragment identifies the page within the index
    // document without advertising a second HTTP resource that does not exist.
    let page_url = format!("{registration_url}#page/{lower}/{upper}");

    let mut leaves = Vec::new();
    for e in entries {
        let mut leaf = serde_json::json!({
            "packageContent": e.download_url,
            "registration": registration_url,
        });

        if let Some(ref catalog_entry) = build_catalog_entry(package_name, e, &page_url) {
            leaf["catalogEntry"] = catalog_entry.clone();
        }

        leaves.push(leaf);
    }

    serde_json::json!({
        "count": 1,
        "items": [{
            "@id": page_url,
            "count": leaves.len(),
            "lower": lower,
            "upper": upper,
            "items": leaves,
        }]
    })
}

/// The `catalogEntry` of one registration leaf — the document `dotnet restore`
/// reads a version's identity, dependency graph and availability out of.
fn build_catalog_entry(
    name: &str,
    entry: &NuGetRegistrationEntry,
    page_url: &str,
) -> Option<serde_json::Value> {
    let mut out = serde_json::Map::new();
    out.insert("@id".into(), page_url.into());
    out.insert("id".into(), name.into());
    out.insert("version".into(), entry.version.clone().into());
    // Stated rather than left to the client's default, because the default is
    // `true` and a yanked version would inherit it.
    out.insert("listed".into(), entry.listed.into());
    if let Some(d) = entry.description.as_deref() {
        out.insert("description".into(), d.into());
    }
    if let Some(h) = entry.homepage.as_deref() {
        out.insert("projectUrl".into(), h.into());
    }
    if let Some(l) = entry.license.as_deref() {
        out.insert("licenseUrl".into(), l.into());
    }
    if let Some(t) = entry.tags.as_deref() {
        out.insert(
            "tags".into(),
            t.split(&[',', ' '][..])
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .into(),
        );
    }
    if !entry.dependency_groups.is_empty() {
        out.insert(
            "dependencyGroups".into(),
            entry
                .dependency_groups
                .iter()
                .map(build_dependency_group)
                .collect::<Vec<_>>()
                .into(),
        );
    }
    Some(serde_json::Value::Object(out))
}

/// One `PackageDependencyGroup` of a catalog entry.
///
/// `targetFramework` is omitted rather than sent as `null` when the nuspec
/// listed its dependencies flat: NuGet reads an absent framework as "applies to
/// all of them", and a `null` as a malformed group.
fn build_dependency_group(group: &NuGetDependencyGroup) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("@type".into(), "PackageDependencyGroup".into());
    if let Some(framework) = group.target_framework.as_deref() {
        out.insert("targetFramework".into(), framework.into());
    }
    out.insert(
        "dependencies".into(),
        group
            .dependencies
            .iter()
            .map(|dep| {
                serde_json::json!({
                    "@type": "PackageDependency",
                    "id": dep.id,
                    "range": dep.range,
                })
            })
            .collect::<Vec<_>>()
            .into(),
    );
    serde_json::Value::Object(out)
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

/// The part of NuGet's version contract that differs from strict SemVer.
///
/// NuGet accepts one through four numeric components (missing components are
/// zero), compares the fourth `Revision`, and compares prerelease labels
/// case-insensitively. Build metadata does not participate in the default
/// ordering. Keeping this parser here avoids accidentally reusing npm's strict
/// SemVer selector for versions such as `2.0.0.1`.
#[derive(Debug, Eq, PartialEq)]
struct NuGetVersion {
    numbers: [u32; 4],
    release: Option<Vec<NuGetReleaseLabel>>,
}

#[derive(Debug, Eq, PartialEq)]
enum NuGetReleaseLabel {
    Numeric(u32),
    Text(String),
}

impl NuGetVersion {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }

        let (without_metadata, metadata) = value
            .split_once('+')
            .map_or((value, None), |(version, metadata)| {
                (version, Some(metadata))
            });
        if metadata.is_some_and(|metadata| !valid_nuget_labels(metadata, true)) {
            return None;
        }

        let (numeric, release) = without_metadata
            .split_once('-')
            .map_or((without_metadata, None), |(numeric, release)| {
                (numeric, Some(release))
            });
        let components: Vec<&str> = numeric.split('.').collect();
        if components.is_empty() || components.len() > 4 {
            return None;
        }

        let mut numbers = [0; 4];
        for (slot, component) in numbers.iter_mut().zip(components) {
            let component = component.trim();
            if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let parsed = component.parse::<u32>().ok()?;
            if parsed > i32::MAX as u32 {
                return None;
            }
            *slot = parsed;
        }

        let release = match release {
            Some(release) => {
                if !valid_nuget_labels(release, false) {
                    return None;
                }
                Some(
                    release
                        .split('.')
                        .map(|label| {
                            label
                                .parse::<u32>()
                                .ok()
                                .filter(|value| *value <= i32::MAX as u32)
                                .map_or_else(
                                    || NuGetReleaseLabel::Text(label.to_ascii_lowercase()),
                                    NuGetReleaseLabel::Numeric,
                                )
                        })
                        .collect(),
                )
            }
            None => None,
        };

        Some(Self { numbers, release })
    }

    fn is_prerelease(&self) -> bool {
        self.release.is_some()
    }
}

fn valid_nuget_labels(labels: &str, allow_numeric_leading_zeroes: bool) -> bool {
    labels.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && (allow_numeric_leading_zeroes
                || label.len() == 1
                || !label.starts_with('0')
                || !label.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

impl Ord for NuGetVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers
            .cmp(&other.numbers)
            .then_with(|| match (&self.release, &other.release) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            })
    }
}

impl PartialOrd for NuGetVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NuGetReleaseLabel {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            (Self::Numeric(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for NuGetReleaseLabel {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Pick the highest live stable version using NuGetVersion precedence.
///
/// The iterator is in deterministic publication order. Its first unparsable
/// live row is only a compatibility fallback when no valid stable NuGetVersion
/// exists; a valid prerelease is deliberately not promoted to stable latest.
pub(crate) fn latest_live_nuget<'a>(
    versions: impl IntoIterator<Item = (&'a str, bool)>,
) -> Option<&'a str> {
    let mut fallback = None;
    let mut latest: Option<(NuGetVersion, &'a str)> = None;

    for (version, is_yanked) in versions {
        if is_yanked {
            continue;
        }
        let Some(parsed) = NuGetVersion::parse(version) else {
            fallback.get_or_insert(version);
            continue;
        };
        if parsed.is_prerelease() {
            continue;
        }
        if latest.as_ref().is_none_or(|(current, _)| parsed > *current) {
            latest = Some((parsed, version));
        }
    }

    latest.map(|(_, version)| version).or(fallback)
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
    fn nuget_version_order_includes_revision_and_ignores_metadata() {
        let revision = NuGetVersion::parse("2.0.0.1+build.7").unwrap();
        let three_part = NuGetVersion::parse("2.0.0").unwrap();
        let normalized_short = NuGetVersion::parse("2").unwrap();

        assert!(revision > three_part);
        assert_eq!(three_part, normalized_short);
    }

    #[test]
    fn nuget_prerelease_order_is_numeric_and_case_insensitive() {
        let rc_two = NuGetVersion::parse("1.0.0-RC.2").unwrap();
        let rc_ten = NuGetVersion::parse("1.0.0-rc.10").unwrap();
        let stable = NuGetVersion::parse("1.0.0").unwrap();

        assert!(rc_two < rc_ten);
        assert!(rc_ten < stable);
        assert_eq!(
            NuGetVersion::parse("1.0.0-ALPHA").unwrap(),
            NuGetVersion::parse("1.0.0-alpha").unwrap()
        );
    }

    #[test]
    fn latest_nuget_version_is_stable_live_and_has_deterministic_fallback() {
        assert_eq!(
            latest_live_nuget([
                ("4.0.0-beta", false),
                ("1.2.4", false),
                ("9.0.0", true),
                ("2.0.0.1", false),
            ]),
            Some("2.0.0.1")
        );
        assert_eq!(
            latest_live_nuget([("legacy-newest", false), ("legacy-older", false)]),
            Some("legacy-newest")
        );
        assert_eq!(latest_live_nuget([("3.0.0-beta", false)]), None);
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

    /// The registration index builds its `catalogEntry` from the stored version
    /// metadata, so anything the nuspec says about *this* version has to be
    /// carried across the publish.
    #[test]
    fn nuspec_metadata_carries_the_registration_fields() {
        let nuspec = r#"<?xml version="1.0" encoding="utf-8"?>
<package>
  <metadata>
    <id>MyLib</id>
    <version>1.2.3</version>
    <description>A test library</description>
    <projectUrl>https://github.com/user/mylib</projectUrl>
    <license type="expression">MIT</license>
    <tags>testing utility</tags>
  </metadata>
</package>"#;

        let meta = NuGetAdapter
            .extract_metadata("MyLib.1.2.3.nupkg", &make_nupkg(nuspec))
            .unwrap();
        let stored: serde_json::Value =
            serde_json::from_str(&meta.protocol_metadata.expect("no protocol metadata")).unwrap();

        assert_eq!(stored["description"], "A test library");
        assert_eq!(stored["projectUrl"], "https://github.com/user/mylib");
        assert_eq!(stored["license"], "MIT");
        assert_eq!(stored["tags"], "testing utility");
    }

    #[test]
    fn nuspec_metadata_is_absent_when_the_nuspec_declares_none_of_it() {
        let nuspec = r#"<?xml version="1.0"?>
<package><metadata><id>Bare</id><version>1.0.0</version></metadata></package>"#;

        let meta = NuGetAdapter
            .extract_metadata("Bare.1.0.0.nupkg", &make_nupkg(nuspec))
            .unwrap();
        assert!(meta.protocol_metadata.is_none());
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
                dependency_groups: vec![NuGetDependencyGroup {
                    target_framework: Some("net8.0".into()),
                    dependencies: vec![NuGetDependency {
                        id: "Newtonsoft.Json".into(),
                        range: "13.0.1".into(),
                    }],
                }],
                listed: true,
            },
            NuGetRegistrationEntry {
                version: "2.0.0".into(),
                description: Some("Second release".into()),
                homepage: Some("https://example.com".into()),
                license: None,
                tags: None,
                download_url: "https://git.example.com/dl/2.0.0".into(),
                dependency_groups: Vec::new(),
                listed: false,
            },
        ];

        let registration_url = "https://git.example.com/registration/mylib/index.json";
        let json = build_registration_index("MyLib", registration_url, &entries);
        assert_eq!(json["count"], 1);
        let page_url = format!("{registration_url}#page/1.0.0/2.0.0");
        assert_eq!(json["items"][0]["@id"], page_url);

        let items = json["items"][0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);

        // First entry should have packageContent and catalogEntry
        assert_eq!(
            items[0]["packageContent"],
            "https://git.example.com/dl/1.0.0"
        );
        assert_eq!(items[0]["registration"], registration_url);
        assert!(items[0]["catalogEntry"].is_object());
        assert_eq!(items[0]["catalogEntry"]["@id"], page_url);
        assert_eq!(items[0]["catalogEntry"]["id"], "MyLib");
        assert_eq!(items[0]["catalogEntry"]["version"], "1.0.0");

        // The graph `dotnet restore` resolves against — it never opens the
        // `.nupkg` to find one, so an absent key reads as "no dependencies".
        let group = &items[0]["catalogEntry"]["dependencyGroups"][0];
        assert_eq!(group["@type"], "PackageDependencyGroup");
        assert_eq!(group["targetFramework"], "net8.0");
        assert_eq!(group["dependencies"][0]["@type"], "PackageDependency");
        assert_eq!(group["dependencies"][0]["id"], "Newtonsoft.Json");
        assert_eq!(group["dependencies"][0]["range"], "13.0.1");

        // A version with no declared dependencies says nothing rather than
        // publishing an empty list it never read.
        assert_eq!(items[0]["catalogEntry"]["listed"], true);
        assert!(items[1]["catalogEntry"]["dependencyGroups"].is_null());
        assert_eq!(
            items[1]["catalogEntry"]["listed"], false,
            "a yanked version stays in the registration but must not be a candidate"
        );
    }

    #[test]
    fn nuspec_dependency_groups_reads_both_layouts_and_neither() {
        let grouped = r#"<package><metadata>
            <id>MyLib</id><version>1.0.0</version>
            <dependencies>
              <group targetFramework="net8.0">
                <dependency id="Newtonsoft.Json" version="13.0.1" />
                <dependency id="Serilog" version="[3.0.0, 4.0.0)" exclude="Build" />
              </group>
              <group targetFramework="netstandard2.0" />
            </dependencies>
        </metadata></package>"#;

        let groups = nuspec_dependency_groups(grouped);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].target_framework.as_deref(), Some("net8.0"));
        assert_eq!(
            groups[0].dependencies,
            vec![
                NuGetDependency {
                    id: "Newtonsoft.Json".into(),
                    range: "13.0.1".into(),
                },
                NuGetDependency {
                    id: "Serilog".into(),
                    range: "[3.0.0, 4.0.0)".into(),
                },
            ]
        );
        // A framework that needs nothing is a declaration, not an empty result.
        assert_eq!(
            groups[1].target_framework.as_deref(),
            Some("netstandard2.0")
        );
        assert!(groups[1].dependencies.is_empty());

        // The pre-2.0 layout lists them flat, which means "every framework".
        let flat = r#"<package><metadata><dependencies>
            <dependency id="Legacy.Pack" version="1.0" />
            <dependency id="Anything" />
        </dependencies></metadata></package>"#;
        let groups = nuspec_dependency_groups(flat);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].target_framework, None);
        assert_eq!(
            groups[0].dependencies,
            vec![
                NuGetDependency {
                    id: "Legacy.Pack".into(),
                    range: "1.0".into(),
                },
                // No `version` is NuGet's unbounded range, not an empty string.
                NuGetDependency {
                    id: "Anything".into(),
                    range: "(, )".into(),
                },
            ]
        );

        // No block at all is not an empty block: nothing is published.
        assert!(
            nuspec_dependency_groups("<package><metadata><id>x</id></metadata></package>")
                .is_empty()
        );
        assert!(nuspec_dependency_groups(
            "<package><metadata><dependencies /></metadata></package>"
        )
        .is_empty());
    }

    /// `<dependency` must not be read out of the `<dependencies>` that holds
    /// it, and `version` must not be read out of another attribute's value.
    #[test]
    fn nuspec_dependency_scanner_does_not_confuse_neighbouring_names() {
        let xml = r#"<package><metadata><dependencies>
            <dependency id="My.Version.Helper" version="2.0" />
        </dependencies></metadata></package>"#;

        let groups = nuspec_dependency_groups(xml);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].dependencies,
            vec![NuGetDependency {
                id: "My.Version.Helper".into(),
                range: "2.0".into(),
            }]
        );
    }

    /// The scanner's offsets are offsets into the document it was given — the
    /// lowercase-a-copy-and-index-the-original shape breaks on a character
    /// whose lowercase form is a different number of bytes.
    #[test]
    fn nuspec_dependency_scanner_survives_non_ascii_before_the_block() {
        let xml = "<package><metadata>\
            <description>İstanbul — Ünïcode</description>\
            <dependencies><dependency id=\"Ok\" version=\"1.0\" /></dependencies>\
        </metadata></package>";

        let groups = nuspec_dependency_groups(xml);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].dependencies[0].id, "Ok");
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
    fn nuspec_metadata_offsets_survive_length_changing_unicode_case_folds() {
        for description in ["ASCII", "İstanbul", "ẞtraße"] {
            let nuspec = format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<package>
  <metadata>
    <description>{description}</description>
    <ID>Unicode.Package</ID>
    <Version>1.2.3</Version>
    <License TYPE="expression">MIT</License>
    <Repository TYPE="git" URL="https://example.com/unicode.git" />
  </metadata>
</package>"#
            );

            let metadata = extract_from_nuspec(&nuspec)
                .unwrap_or_else(|error| panic!("{description:?}: {error}"));
            assert_eq!(metadata.name, "Unicode.Package", "{description:?}");
            assert_eq!(metadata.version, "1.2.3", "{description:?}");
            assert_eq!(metadata.license.as_deref(), Some("MIT"), "{description:?}");
            assert_eq!(
                metadata.repository_url.as_deref(),
                Some("https://example.com/unicode.git"),
                "{description:?}"
            );
        }
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
