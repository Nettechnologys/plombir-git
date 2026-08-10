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
    collections::HashSet,
    io::{Cursor, Read},
};

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};
use crate::package_registry::url_path::encode_path_segment;
use rg_db::package_version_key::NuGetVersion;

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
    let mut versions = versions
        .iter()
        .filter_map(|version| NuGetVersion::parse(version))
        .collect::<Vec<_>>();
    versions.sort();
    versions.dedup();

    serde_json::json!({
        "versions": versions
            .into_iter()
            .map(|version| version.normalized())
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
    let (lower, upper) = registration_bounds(entries);
    // The page is inline, so its fragment identifies the page within the index
    // document without advertising a second HTTP resource that does not exist.
    let page_url = format!("{registration_url}#page/{lower}/{upper}");

    let mut leaf_ids = HashSet::new();
    let mut leaves = Vec::new();
    for e in entries {
        let leaf_id = registration_leaf_id(registration_url, &e.version);
        // Historical rows can contain two spellings which NuGet considers the
        // same version (`1`, `1.0.0`). They are one protocol leaf and therefore
        // must not advertise the same address twice.
        if leaf_ids.insert(leaf_id.clone()) {
            leaves.push(build_registration_page_leaf(
                package_name,
                registration_url,
                e,
                leaf_id,
            ));
        }
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

/// Build the independently fetchable document advertised by an inline leaf's
/// `@id`.
///
/// A page embeds the complete catalog entry, while the standalone leaf points
/// at that entry by URL. Both shapes are derived from the same page leaf so the
/// package-content and registration links cannot drift apart.
pub fn build_registration_leaf(
    package_name: &str,
    registration_url: &str,
    entry: &NuGetRegistrationEntry,
) -> serde_json::Value {
    let leaf_id = registration_leaf_id(registration_url, &entry.version);
    let page_leaf = build_registration_page_leaf(package_name, registration_url, entry, leaf_id);
    serde_json::json!({
        "@id": page_leaf["@id"],
        "catalogEntry": page_leaf["catalogEntry"]["@id"],
        "listed": page_leaf["catalogEntry"]["listed"],
        "packageContent": page_leaf["packageContent"],
        "registration": page_leaf["registration"],
    })
}

fn build_registration_page_leaf(
    package_name: &str,
    registration_url: &str,
    entry: &NuGetRegistrationEntry,
    leaf_id: String,
) -> serde_json::Value {
    let catalog_id = format!("{leaf_id}#catalogEntry");
    serde_json::json!({
        "@id": leaf_id,
        "catalogEntry": build_catalog_entry(package_name, entry, &catalog_id),
        "packageContent": entry.download_url,
        "registration": registration_url,
    })
}

fn registration_leaf_id(registration_url: &str, version: &str) -> String {
    let registration_base = registration_url
        .strip_suffix("/index.json")
        .unwrap_or_else(|| registration_url.trim_end_matches('/'));
    let version = NuGetVersion::parse(version)
        .map(|version| version.normalized())
        .unwrap_or_else(|| version.trim().to_lowercase());
    format!("{registration_base}/{}", encode_path_segment(&version))
}

fn registration_bounds(entries: &[NuGetRegistrationEntry]) -> (String, String) {
    let mut parsed = entries
        .iter()
        .filter_map(|entry| NuGetVersion::parse(&entry.version));

    if let Some(first) = parsed.next() {
        let (lower, upper) = parsed.fold((first.clone(), first), |(lower, upper), version| {
            (lower.min(version.clone()), upper.max(version))
        });
        return (lower.normalized(), upper.normalized());
    }

    // Historical rows predate protocol validation. If every spelling is
    // unreadable as a NuGetVersion, keep the document stable without letting
    // publication order pretend to be version precedence.
    let mut legacy = entries
        .iter()
        .map(|entry| entry.version.trim().to_lowercase())
        .collect::<Vec<_>>();
    legacy.sort();
    (
        legacy.first().cloned().unwrap_or_default(),
        legacy.last().cloned().unwrap_or_default(),
    )
}

/// The `catalogEntry` of one registration leaf — the document `dotnet restore`
/// reads a version's identity, dependency graph and availability out of.
fn build_catalog_entry(
    name: &str,
    entry: &NuGetRegistrationEntry,
    catalog_id: &str,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("@id".into(), catalog_id.into());
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
    serde_json::Value::Object(out)
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
                r.versions
                    .iter()
                    .map(|version| {
                        serde_json::json!({
                            "@id": registration_leaf_id(&r.registration_url, &version.version),
                            "version": version.version,
                            "downloads": version.downloads,
                        })
                    })
                    .collect::<Vec<_>>()
                    .into(),
            );
            item.insert(
                "totalDownloads".into(),
                r.versions
                    .iter()
                    .map(|version| version.downloads)
                    .sum::<i64>()
                    .into(),
            );
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
    pub versions: Vec<NuGetSearchVersion>,
    pub description: Option<String>,
    pub tags: Option<String>,
    pub registration_url: String,
}

/// One version advertised by NuGet SearchQueryService.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NuGetSearchVersion {
    pub version: String,
    pub downloads: i64,
}

/// Whether two spellings identify the same NuGet version.
///
/// Unparsable historical spellings retain the old case-insensitive exact-match
/// behavior; they cannot alias a valid normalized URL token.
pub fn nuget_versions_match(stored: &str, requested: &str) -> bool {
    match (NuGetVersion::parse(stored), NuGetVersion::parse(requested)) {
        (Some(stored), Some(requested)) => stored == requested,
        _ => stored.eq_ignore_ascii_case(requested),
    }
}

/// One stored version, in the terms a NuGet client capability filter decides on.
///
/// The SemVer level of a package version is not a property of its own spelling
/// alone: NuGet also calls it SemVer 2-specific when the minimum or maximum of
/// any of its dependency ranges is. A row therefore carries what the stored
/// dependency graph said, alongside the two facts the database row itself holds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NuGetVersionRow<'a> {
    pub version: &'a str,
    pub is_yanked: bool,
    /// Whether this version's dependency ranges need a SemVer 2-aware client.
    ///
    /// `false` from a caller that never read the graph — which is sound only
    /// because such a caller is one whose answer the graph cannot change; see
    /// [`NuGetVersionFilter::negotiates_semver_level`].
    pub dependencies_require_semver2: bool,
}

impl<'a> NuGetVersionRow<'a> {
    /// A row classified by its version spelling alone.
    pub(crate) const fn spelling_only(version: &'a str, is_yanked: bool) -> Self {
        Self {
            version,
            is_yanked,
            dependencies_require_semver2: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NuGetVersionFilter {
    include_prerelease: bool,
    include_semver2: bool,
    allow_legacy_fallback: bool,
}

impl NuGetVersionFilter {
    /// Preserve the generic package summary contract: it is not a NuGet client
    /// capability negotiation surface, but it still must not promote a
    /// prerelease over a stable release.
    pub(crate) const PACKAGE_SUMMARY: Self = Self {
        include_prerelease: false,
        include_semver2: true,
        allow_legacy_fallback: true,
    };

    /// SearchQueryService defaults to SemVer 1 stable versions. A parseable
    /// `semVerLevel >= 2.0.0` opts into SemVer 2; invalid input is equivalent to
    /// omitting the parameter, as required by the NuGet server contract.
    pub(crate) fn search(include_prerelease: bool, semver_level: Option<&str>) -> Self {
        let semver2_floor =
            NuGetVersion::parse("2.0.0").expect("the static SemVer 2 floor must be valid NuGet");
        let include_semver2 = semver_level
            .and_then(NuGetVersion::parse)
            .is_some_and(|level| level >= semver2_floor);

        Self {
            include_prerelease,
            include_semver2,
            allow_legacy_fallback: false,
        }
    }

    /// Parse one live version when it is visible to this client capability set.
    pub(crate) fn candidate(self, row: NuGetVersionRow<'_>) -> Option<NuGetVersion> {
        if row.is_yanked {
            return None;
        }
        let parsed = NuGetVersion::parse(row.version)?;
        if !self.include_prerelease && parsed.is_prerelease() {
            return None;
        }
        // Both halves of NuGet's SemVer 2 rule: the version's own spelling, and
        // the bounds of the dependency ranges the client would have to resolve
        // after picking it.
        if !self.include_semver2
            && (NuGetVersion::is_semver2_specific(row.version) || row.dependencies_require_semver2)
        {
            return None;
        }
        Some(parsed)
    }

    /// Whether this filter's answer can depend on the stored dependency graph.
    ///
    /// Only a client that did *not* opt into SemVer 2 can be hidden from a
    /// package by its dependency ranges, so every other caller is spared both
    /// the metadata read and the fail-loud decode that comes with it — a damaged
    /// row must not fail a request whose answer it could not have changed.
    pub(crate) const fn negotiates_semver_level(self) -> bool {
        !self.include_semver2
    }
}

/// The dependency groups recorded beside a NuGet version, read back out of the
/// stored protocol metadata.
///
/// Kept here, next to [`nuspec_protocol_metadata`] which writes these keys, so
/// that the registration leaf and the search capability filter classify exactly
/// the same graph — a reader that drifts from the writer is how a leaf ends up
/// publishing a dependency the filter never saw.
///
/// A group whose `dependencies` array is missing is still a group — it declares
/// a supported framework that needs nothing — so an entry is dropped only when
/// it carries no framework *and* no dependencies at all.
pub fn stored_dependency_groups(doc: &serde_json::Value) -> Vec<NuGetDependencyGroup> {
    let Some(groups) = doc.get("dependencyGroups").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    groups
        .iter()
        .map(|group| NuGetDependencyGroup {
            target_framework: group
                .get("targetFramework")
                .and_then(|v| v.as_str())
                .filter(|f| !f.is_empty())
                .map(String::from),
            dependencies: group
                .get("dependencies")
                .and_then(|v| v.as_array())
                .map(|deps| {
                    deps.iter()
                        .filter_map(|dep| {
                            Some(NuGetDependency {
                                id: dep
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .filter(|id| !id.is_empty())?
                                    .to_string(),
                                range: dep
                                    .get("range")
                                    .and_then(|v| v.as_str())
                                    .filter(|r| !r.is_empty())
                                    .unwrap_or("(, )")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .filter(|group| group.target_framework.is_some() || !group.dependencies.is_empty())
        .collect()
}

/// Whether a dependency graph makes the version that declares it SemVer
/// 2-specific.
///
/// NuGet's own rule: a package version is SemVer 2-specific when its own version
/// is, **or** when the minimum or maximum of any dependency range is. So a plain
/// `1.0.0` that depends on `[2.0.0-alpha.1, )` must stay invisible to a client
/// that did not ask for SemVer 2 — advertising it hands that client a package
/// whose graph it cannot parse, and the failure lands at restore time as a
/// version nobody can resolve rather than as a package that was never offered.
pub(crate) fn dependency_groups_require_semver2(groups: &[NuGetDependencyGroup]) -> bool {
    groups.iter().any(|group| {
        group
            .dependencies
            .iter()
            .any(|dependency| range_requires_semver2(&dependency.range))
    })
}

/// Whether either bound of one NuGet version range needs a SemVer 2 client.
///
/// The interval notation is `[1.0.0]`, `[1.0.0,)`, `(,2.0.0]`, `[1.0,2.0)`; a
/// bare `1.0.0` is shorthand for `[1.0.0, )`. Every non-empty comma-separated
/// piece inside the brackets is a version spelling, and an omitted bound is
/// exactly that — no bound, so nothing to classify.
fn range_requires_semver2(range: &str) -> bool {
    range
        .trim()
        .trim_start_matches(['[', '('])
        .trim_end_matches([']', ')'])
        .split(',')
        .map(str::trim)
        .filter(|bound| !bound.is_empty())
        .any(NuGetVersion::is_semver2_specific)
}

/// Whether the version behind this stored metadata blob needs a SemVer 2 client
/// for its dependency graph.
///
/// A damaged blob is an error, not a `false`. `false` means "the graph was read
/// and it is SemVer 1", and saying that about a row nobody could read advertises
/// the package to precisely the client least able to cope with it — the same lie
/// `build_sparse_index_entry` refuses to tell for a cargo index entry. The
/// message names the coordinate for operators and never carries the blob.
pub(crate) fn stored_dependencies_require_semver2(
    package_name: &str,
    version: &str,
    metadata_json: Option<&str>,
) -> anyhow::Result<bool> {
    // A legacy row that predates the adapter recording anything is an absence,
    // not damage: there is no graph to be SemVer 2-specific.
    let Some(blob) = metadata_json else {
        return Ok(false);
    };

    let doc = serde_json::from_str::<serde_json::Value>(blob).map_err(|error| {
        tracing::error!(
            package = %package_name,
            version = %version,
            error = %error,
            "stored nuget metadata is not valid JSON — refusing to classify the version as \
             SemVer 1 and advertise it to a client that cannot read its dependency graph"
        );
        unreadable_nuget_metadata(package_name, version)
    })?;
    if !doc.is_object() {
        tracing::error!(
            package = %package_name,
            version = %version,
            "stored nuget metadata is valid JSON but not an object — refusing to classify the \
             version as SemVer 1 and advertise it to a client that cannot read its dependency graph"
        );
        return Err(unreadable_nuget_metadata(package_name, version));
    }

    Ok(dependency_groups_require_semver2(
        &stored_dependency_groups(&doc),
    ))
}

/// Untyped on purpose: this is the registry's own row being unreadable, so it
/// must reach the client as a 5xx and never as "fix your request".
fn unreadable_nuget_metadata(package_name: &str, version: &str) -> anyhow::Error {
    anyhow::anyhow!("stored nuget metadata for '{package_name}' {version} could not be read")
}

/// Pick the highest live version allowed by one NuGet client capability mode.
///
/// The iterator is in deterministic publication order. Its first unparsable
/// live row remains a compatibility fallback only for the generic package
/// summary; protocol search never advertises a version its client cannot parse.
pub(crate) fn latest_live_nuget<'a>(
    versions: impl IntoIterator<Item = NuGetVersionRow<'a>>,
    filter: NuGetVersionFilter,
) -> Option<&'a str> {
    let mut fallback = None;
    let mut latest: Option<(NuGetVersion, &'a str)> = None;

    for row in versions {
        let Some(parsed) = filter.candidate(row) else {
            if !row.is_yanked
                && filter.allow_legacy_fallback
                && NuGetVersion::parse(row.version).is_none()
            {
                fallback.get_or_insert(row.version);
            }
            continue;
        };
        if latest.as_ref().is_none_or(|(current, _)| parsed > *current) {
            latest = Some((parsed, row.version));
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
    fn flat_container_versions_are_normalized_sorted_and_deduplicated() {
        let versions = [
            "2.0.0.1+Build.7",
            "1.0.0.0",
            "1",
            "1.5.0-RC.2+metadata",
            "legacy-row",
        ]
        .map(String::from);

        let index = build_flat_container_index(&versions);
        assert_eq!(
            index["versions"],
            serde_json::json!(["1.0.0", "1.5.0-rc.2", "2.0.0.1"])
        );
        assert!(nuget_versions_match("1", "1.0.0"));
        assert!(nuget_versions_match("1.0.0.0", "1.0.0"));
        assert!(nuget_versions_match("1.5.0-RC.2+metadata", "1.5.0-rc.2"));
        assert!(!nuget_versions_match("legacy-row", "1.0.0"));
    }

    #[test]
    fn registration_bounds_follow_nuget_precedence_with_legacy_fallback() {
        let entry = |version: &str| NuGetRegistrationEntry {
            version: version.into(),
            description: None,
            homepage: None,
            license: None,
            tags: None,
            download_url: format!("https://git.example.com/dl/{version}"),
            dependency_groups: Vec::new(),
            listed: true,
        };
        let entries = [
            entry("2.0.0.1+Build.7"),
            entry("legacy-row"),
            entry("1.2"),
            entry("1.5.0-RC.2+metadata"),
        ];

        let index = build_registration_index(
            "Matrix.Bounds",
            "https://git.example.com/registration/matrix.bounds/index.json",
            &entries,
        );
        assert_eq!(index["items"][0]["lower"], "1.2.0");
        assert_eq!(index["items"][0]["upper"], "2.0.0.1");

        let legacy_only = [entry("zeta-version"), entry("Alpha-Version")];
        let index = build_registration_index(
            "Matrix.Legacy",
            "https://git.example.com/registration/matrix.legacy/index.json",
            &legacy_only,
        );
        assert_eq!(index["items"][0]["lower"], "alpha-version");
        assert_eq!(index["items"][0]["upper"], "zeta-version");
    }

    /// Rows classified by their version spelling alone, as every caller that
    /// does not negotiate a SemVer level supplies them.
    fn rows<'a>(versions: impl IntoIterator<Item = (&'a str, bool)>) -> Vec<NuGetVersionRow<'a>> {
        versions
            .into_iter()
            .map(|(version, is_yanked)| NuGetVersionRow::spelling_only(version, is_yanked))
            .collect()
    }

    #[test]
    fn latest_nuget_version_is_stable_live_and_has_deterministic_fallback() {
        assert_eq!(
            latest_live_nuget(
                rows([
                    ("4.0.0-beta", false),
                    ("1.2.4", false),
                    ("9.0.0", true),
                    ("2.0.0.1", false),
                ]),
                NuGetVersionFilter::PACKAGE_SUMMARY
            ),
            Some("2.0.0.1")
        );
        assert_eq!(
            latest_live_nuget(
                rows([("legacy-newest", false), ("legacy-older", false)]),
                NuGetVersionFilter::PACKAGE_SUMMARY,
            ),
            Some("legacy-newest")
        );
        assert_eq!(
            latest_live_nuget(
                rows([("3.0.0-beta", false)]),
                NuGetVersionFilter::PACKAGE_SUMMARY,
            ),
            None
        );
    }

    #[test]
    fn search_version_filter_negotiates_prerelease_and_semver2_independently() {
        let versions = rows([
            ("9.0.0", true),
            ("6.0.0+build.7", false),
            ("5.0.0-beta.1", false),
            ("4.0.0-beta", false),
            ("1.0.0", false),
            ("legacy-row", false),
        ]);

        assert_eq!(
            latest_live_nuget(
                versions.iter().copied(),
                NuGetVersionFilter::search(false, None)
            ),
            Some("1.0.0")
        );
        assert_eq!(
            latest_live_nuget(
                versions.iter().copied(),
                NuGetVersionFilter::search(true, None)
            ),
            Some("4.0.0-beta")
        );
        assert_eq!(
            latest_live_nuget(
                versions.iter().copied(),
                NuGetVersionFilter::search(false, Some("2.0.0")),
            ),
            Some("6.0.0+build.7")
        );
        assert_eq!(
            latest_live_nuget(
                versions.iter().copied(),
                NuGetVersionFilter::search(true, Some("2.1.0")),
            ),
            Some("6.0.0+build.7")
        );
        assert_eq!(
            latest_live_nuget(
                rows([("3.0.0-alpha.1", false)]),
                NuGetVersionFilter::search(true, Some("not-a-version")),
            ),
            None
        );
    }

    /// NuGet's SemVer 2 rule has a second half: a version whose own spelling is
    /// plain SemVer 1 is still SemVer 2-specific when a dependency range bound
    /// is. Advertising it to a SemVer 1 client hands over a graph that client
    /// cannot resolve.
    #[test]
    fn dependency_range_bounds_decide_the_semver_level_too() {
        let semver2_dependency = NuGetVersionRow {
            version: "1.0.0",
            is_yanked: false,
            dependencies_require_semver2: true,
        };

        assert_eq!(
            latest_live_nuget(
                [semver2_dependency],
                NuGetVersionFilter::search(false, None)
            ),
            None
        );
        assert_eq!(
            latest_live_nuget(
                [semver2_dependency],
                NuGetVersionFilter::search(false, Some("2.0.0"))
            ),
            Some("1.0.0")
        );
        // The generic package summary negotiates nothing, so it neither hides
        // the version nor asks anyone to read the graph for it.
        assert!(!NuGetVersionFilter::PACKAGE_SUMMARY.negotiates_semver_level());
        assert!(NuGetVersionFilter::search(true, None).negotiates_semver_level());
        assert!(!NuGetVersionFilter::search(false, Some("2.0.0")).negotiates_semver_level());
    }

    #[test]
    fn semver2_range_bounds_are_recognised_at_both_ends_of_the_interval() {
        for semver1 in [
            "1.0.0",
            "[1.0.0]",
            "[1.0.0, 2.0.0)",
            "(, 2.0.0]",
            "(, )",
            "[1.0.0-beta, 2.0.0-rc)",
            "1.0.0.4",
        ] {
            assert!(!range_requires_semver2(semver1), "{semver1}");
        }

        for semver2 in [
            "[2.0.0-alpha.1, )",
            "2.0.0-alpha.1",
            "(1.0.0, 2.0.0-rc.1]",
            "[1.0.0+build.7, )",
            "[1.0.0, 2.0.0+build.7)",
        ] {
            assert!(range_requires_semver2(semver2), "{semver2}");
        }
    }

    #[test]
    fn stored_dependency_graph_classifies_the_version_and_fails_loud_when_damaged() {
        let semver2_graph = r#"{"dependencyGroups":[
            {"targetFramework":"net8.0","dependencies":[{"id":"A","range":"[1.0.0, )"}]},
            {"targetFramework":"net9.0","dependencies":[{"id":"B","range":"[2.0.0-alpha.1, )"}]}
        ]}"#;
        assert!(
            stored_dependencies_require_semver2("Matrix.Deps", "1.0.0", Some(semver2_graph))
                .unwrap()
        );

        let semver1_graph = r#"{"dependencyGroups":[{"targetFramework":"net8.0","dependencies":[
                {"id":"A","range":"13.0.1"},{"id":"B","range":"[3.0.0, 4.0.0)"}]}]}"#;
        assert!(
            !stored_dependencies_require_semver2("Matrix.Deps", "1.0.0", Some(semver1_graph))
                .unwrap()
        );

        // A row published before the adapter recorded anything declares no
        // graph; that is an absence, not damage.
        assert!(!stored_dependencies_require_semver2("Matrix.Deps", "1.0.0", None).unwrap());
        assert!(!stored_dependencies_require_semver2(
            "Matrix.Deps",
            "1.0.0",
            Some(r#"{"tags":"a b"}"#)
        )
        .unwrap());

        for damaged in ["{broken-json", "[]", "\"a string\""] {
            let error = stored_dependencies_require_semver2("Matrix.Deps", "1.0.0", Some(damaged))
                .expect_err(damaged);
            assert!(
                !format!("{error}").contains(damaged),
                "the unreadable blob must not travel with the error: {error}"
            );
        }
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
            items[0]["@id"],
            "https://git.example.com/registration/mylib/1.0.0"
        );
        assert_eq!(
            items[1]["@id"],
            "https://git.example.com/registration/mylib/2.0.0"
        );
        assert_eq!(
            items[0]["packageContent"],
            "https://git.example.com/dl/1.0.0"
        );
        assert_eq!(items[0]["registration"], registration_url);
        assert!(items[0]["catalogEntry"].is_object());
        assert_eq!(
            items[0]["catalogEntry"]["@id"],
            "https://git.example.com/registration/mylib/1.0.0#catalogEntry"
        );
        assert_eq!(items[0]["catalogEntry"]["id"], "MyLib");
        assert_eq!(items[0]["catalogEntry"]["version"], "1.0.0");

        let leaf = build_registration_leaf("MyLib", registration_url, &entries[0]);
        assert_eq!(leaf["@id"], items[0]["@id"]);
        assert_eq!(leaf["packageContent"], items[0]["packageContent"]);
        assert_eq!(leaf["registration"], items[0]["registration"]);
        assert_eq!(leaf["catalogEntry"], items[0]["catalogEntry"]["@id"]);
        assert_eq!(leaf["listed"], true);

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
    fn registration_leaf_ids_deduplicate_equivalent_version_spellings() {
        let entry = |version: &str| NuGetRegistrationEntry {
            version: version.into(),
            description: None,
            homepage: None,
            license: None,
            tags: None,
            download_url: format!("https://git.example.com/dl/{version}"),
            dependency_groups: Vec::new(),
            listed: true,
        };
        let entries = [entry("1"), entry("1.0.0"), entry("legacy-row")];
        let index = build_registration_index(
            "MyLib",
            "https://git.example.com/registration/mylib/index.json",
            &entries,
        );
        let leaves = index["items"][0]["items"].as_array().unwrap();

        assert_eq!(leaves.len(), 2, "equivalent NuGet versions are one leaf");
        assert_eq!(
            leaves[0]["@id"],
            "https://git.example.com/registration/mylib/1.0.0"
        );
        assert_eq!(
            leaves[1]["@id"],
            "https://git.example.com/registration/mylib/legacy-row"
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
            versions: vec![
                NuGetSearchVersion {
                    version: "1.0.0".into(),
                    downloads: 3,
                },
                NuGetSearchVersion {
                    version: "13.0.3".into(),
                    downloads: 7,
                },
            ],
            description: Some("Json.NET".into()),
            tags: Some("json serializer".into()),
            registration_url: "https://example.com/registration/newtonsoft.json/index.json".into(),
        }];

        let json = build_search_results(&results, 1);
        assert_eq!(json["totalHits"], 1);

        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], "Newtonsoft.Json");
        assert_eq!(data[0]["version"], "13.0.3");
        assert_eq!(data[0]["totalDownloads"], 10);
        assert_eq!(
            data[0]["versions"],
            serde_json::json!([
                {
                    "@id": "https://example.com/registration/newtonsoft.json/1.0.0",
                    "version": "1.0.0",
                    "downloads": 3,
                },
                {
                    "@id": "https://example.com/registration/newtonsoft.json/13.0.3",
                    "version": "13.0.3",
                    "downloads": 7,
                },
            ])
        );
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
