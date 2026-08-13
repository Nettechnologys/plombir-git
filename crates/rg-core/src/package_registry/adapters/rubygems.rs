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
//! - `GET /api/v1/dependencies?gems={name}` — legacy Marshal dependency resolution
//! - `GET /api/v1/dependencies.json?gems={name}` — JSON dependency metadata
//! - `GET /api/v1/gems/{name}.json` — gem metadata
//! - `GET /gems/{name}-{version}.gem` — gem download
//! - `POST /api/v1/gems` — gem push
//!
//! ForgeKeep serves these at:
//! - Dependencies: `GET /api/v1/repos/{owner}/{repo}/packages/rubygems/api/v1/dependencies.json?gems={name}`
//! - Gem info:     `GET /api/v1/repos/{owner}/{repo}/packages/rubygems/api/v1/gems/{name}.json`
//! - Download:     (standard package download endpoint)
//!
//! ## Compact index
//!
//! Neither of those two is how a modern client resolves a gem.
//! `Gem::Source#dependency_resolver_set` asks the source for `versions`, and
//! what it does next depends only on whether that file is there: on a hit it
//! resolves through the *compact index* (`info/<gem>`), on a miss it falls back
//! to the legacy Marshal index (`specs.4.8.gz` + `quick/Marshal.4.8/…`), which
//! ForgeKeep does not serve. So `versions` is the switch, and the three files
//! built below — `versions`, `info/<gem>`, `names` — are the whole read side:
//! <https://guides.rubygems.org/rubygems-org-compact-index-api/>
//!
//! The download URL is not part of that negotiation and is not configurable the
//! way Cargo's `dl` is: `Gem::RemoteFetcher#download` glues `gems/<file>` onto
//! the source URL, so `.../packages/rubygems/gems/{file}` is the one path a
//! `gem_uri` may point at.

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

        // Read the gemspec, don't merely find `metadata.gz` — see
        // `CargoAdapter::validate`. A `metadata.gz` that is not gzip, or whose
        // YAML has no name, is a gem `gem install` cannot resolve, and this is
        // the only gate publish always runs. The absence check survives inside
        // `extract_from_gem` (`invalid .gem: no metadata.gz found`).
        extract_from_gem(data)?;
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
        protocol_metadata: Some(gemspec_protocol_metadata(&doc)?),
    })
}

/// The gemspec fields no package column holds, in the shape the RubyGems
/// endpoints read them back.
///
/// Dependencies are the reason this exists: they live only here, and a gem
/// served without them looks to Bundler like a gem that genuinely depends on
/// nothing — a wrong answer that resolves cleanly instead of failing. The keys
/// are the ones `parse_rubygems_deps` / `parse_rubygems_info` (rg-http) look
/// for, and the object is always written, so an empty dependency list is a
/// recorded fact rather than a missing one.
///
/// Fallible because a gemspec can declare a runtime dependency this shape
/// cannot carry; see [`gemspec_dependencies`] for why that is a refusal rather
/// than a best-effort translation.
fn gemspec_protocol_metadata(doc: &serde_yaml::Value) -> Result<String, anyhow::Error> {
    let mut out = serde_json::Map::new();

    for (key, yaml_key) in [("summary", "summary"), ("description", "description")] {
        if let Some(value) = doc
            .get(yaml_key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            out.insert(key.into(), value.into());
        }
    }

    if let Some(homepage) = doc
        .get("homepage")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        out.insert("homepage".into(), homepage.into());
    }

    let licenses: Vec<serde_json::Value> = doc
        .get("licenses")
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|l| l.as_str())
                .map(Into::into)
                .collect()
        })
        .unwrap_or_default();
    if !licenses.is_empty() {
        out.insert("licenses".into(), licenses.into());
    }

    // The platform is a *declared* fact, not one to be inferred back out of a
    // filename. The compact index has to spell the `VERSION-PLATFORM` chunk the
    // client will turn into a download URL, and both resolver endpoints used to
    // answer a flat `ruby` for every gem — which locks a native build as
    // platform-independent (card_0c9e858230b6).
    if let Some(platform) = gemspec_platform(doc) {
        out.insert("platform".into(), platform.into());
    }

    // `required_ruby_version` is the constraint a resolver filters candidates
    // on before it looks at a dependency. Unpublished, a gem needing 3.1
    // resolves cleanly onto 2.7 and fails at parse time.
    for key in ["required_ruby_version", "required_rubygems_version"] {
        let Some(declared) = doc.get(key).map(untagged).filter(|v| !v.is_null()) else {
            continue;
        };
        out.insert(key.into(), gem_requirement_string(declared, key)?.into());
    }

    let dependencies: Vec<serde_json::Value> = gemspec_dependencies(doc)?
        .into_iter()
        .map(|(name, requirements)| {
            serde_json::json!({ "name": name, "requirements": requirements })
        })
        .collect();
    out.insert("dependencies".into(), dependencies.into());

    Ok(serde_json::Value::Object(out).to_string())
}

/// The platform a gemspec declares, when it is not the default `ruby`.
///
/// Recorded only when it differs, because that is exactly the rule the compact
/// index follows: `1.0.0` for a pure-ruby gem, `1.0.0-x86_64-linux` for a native
/// one, and a spurious `-ruby` suffix would send the client after a file that
/// does not exist.
fn gemspec_platform(doc: &serde_yaml::Value) -> Option<String> {
    let platform = doc.get("platform")?;

    let spelled = match platform.as_str() {
        Some(text) => text.trim().to_string(),
        // Gems packed before RubyGems flattened the field carry a serialized
        // `Gem::Platform` instead of the `cpu-os-version` string it prints as.
        None => {
            let part = |key: &str| {
                platform
                    .get(key)
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
            };
            let parts: Vec<&str> = ["cpu", "os", "version"]
                .iter()
                .filter_map(|k| part(k))
                .collect();
            if parts.is_empty() {
                return None;
            }
            parts.join("-")
        }
    };

    (!spelled.is_empty() && spelled != "ruby").then_some(spelled)
}

/// The runtime dependencies a gemspec declares, as `(name, requirement)`.
///
/// `dependencies:` lists both kinds under one key and only the runtime ones
/// belong in an index: a development dependency published there would pull a
/// gem's own test suite into every consumer's resolution.
///
/// card_69cfa8de4fd1: a runtime entry this shape cannot carry is refused rather
/// than dropped. The dependency list is the whole resolver input, and both
/// endpoints read it back from here rather than from the `.gem` — so a gemspec
/// declaring one good and one malformed dependency used to publish as a gem
/// that depends on the good one alone. That is the failure a resolver cannot
/// detect: a shorter list still resolves, cleanly, and Bundler only finds out
/// at the missing constant far from the publish that caused it. Naming the
/// element at publish is the last point where the answer is still cheap.
fn gemspec_dependencies(doc: &serde_yaml::Value) -> Result<Vec<(String, String)>, anyhow::Error> {
    let Some(declared) = doc.get("dependencies").map(untagged) else {
        return Ok(Vec::new());
    };
    if declared.is_null() {
        return Ok(Vec::new());
    }
    let deps = declared.as_sequence().ok_or_else(|| {
        anyhow::anyhow!(
            "RubyGems metadata `dependencies` must be a list of Gem::Dependency, found {}",
            yaml_type_name(declared)
        )
    })?;

    // Indices count the declared list, development entries included: that is
    // the position the gemspec author is looking at.
    let mut runtime = Vec::with_capacity(deps.len());
    for (index, dep) in deps.iter().enumerate() {
        if !is_runtime_dependency(dep, index)? {
            continue;
        }

        let declared_name = dep.get("name").map(untagged);
        let name = declared_name
            .and_then(serde_yaml::Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| match declared_name {
                Some(value) => anyhow::anyhow!(
                    "RubyGems metadata `dependencies[{index}].name` must be a non-empty string, found {}",
                    yaml_type_name(value)
                ),
                None => anyhow::anyhow!(
                    "RubyGems metadata `dependencies[{index}].name` is missing"
                ),
            })?
            .to_string();

        // `requirement` is the modern spelling; `version_requirements` is
        // what gems packed before RubyGems 1.4 carry, and old gems stay
        // installable forever.
        let declared_requirement = dep
            .get("requirement")
            .map(|value| ("requirement", value))
            .or_else(|| {
                dep.get("version_requirements")
                    .map(|value| ("version_requirements", value))
            })
            .map(|(key, value)| (key, untagged(value)))
            .filter(|(_, value)| !value.is_null());
        let requirements = match declared_requirement {
            Some((key, value)) => {
                gem_requirement_string(value, &format!("dependencies[{index}].{key}"))?
            }
            // A dependency that declares no requirement at all is the gemspec
            // saying "any version", which is what `Gem::Requirement.default`
            // spells. That is the one reading of `>= 0` this may reach.
            None => ">= 0".to_string(),
        };
        runtime.push((name, requirements));
    }

    Ok(runtime)
}

/// Whether a `Gem::Dependency` is a runtime one. A gemspec spells the kind as a
/// Ruby symbol (`:runtime` / `:development`), and an absent `type` predates the
/// distinction — those are runtime.
///
/// Fallible for the same reason as [`gemspec_dependencies`]: `type` decides
/// whether the entry enters the resolver graph at all, so a value that is not a
/// symbol at all cannot be read as "runtime, presumably". A malformed entry the
/// kind check does exclude is *not* refused — a development dependency never
/// reaches the index, so nothing about it can be published wrong.
fn is_runtime_dependency(dep: &serde_yaml::Value, index: usize) -> Result<bool, anyhow::Error> {
    let Some(declared) = dep.get("type").map(untagged) else {
        return Ok(true);
    };
    if declared.is_null() {
        return Ok(true);
    }
    let kind = declared.as_str().ok_or_else(|| {
        anyhow::anyhow!(
            "RubyGems metadata `dependencies[{index}].type` must be :runtime or :development, found {}",
            yaml_type_name(declared)
        )
    })?;
    Ok(kind.trim().trim_start_matches(':') == "runtime")
}

/// The value under a Ruby tag.
///
/// Indexing a `Value` sees through tags on its own, but `as_str` does not — so
/// a field Psych wrote as `!ruby/symbol runtime` would otherwise read as "not a
/// string" and be refused as damaged. Everything a gemspec declares is tagged
/// somewhere in the wild, which is exactly why the refusals below have to
/// discriminate the shape and not the spelling.
fn untagged(value: &serde_yaml::Value) -> &serde_yaml::Value {
    let mut value = value;
    while let serde_yaml::Value::Tagged(tagged) = value {
        value = &tagged.value;
    }
    value
}

/// What a YAML value is, for a refusal that has to say why without echoing the
/// gemspec back at the client.
fn yaml_type_name(value: &serde_yaml::Value) -> &'static str {
    match untagged(value) {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "a boolean",
        serde_yaml::Value::Number(_) => "a number",
        serde_yaml::Value::String(_) => "a string",
        serde_yaml::Value::Sequence(_) => "a list",
        serde_yaml::Value::Mapping(_) => "a mapping",
        // `untagged` loops until the value is not one.
        serde_yaml::Value::Tagged(_) => unreachable!("untagged value is never tagged"),
    }
}

/// Flatten a `Gem::Requirement` into the comma-separated string a gemspec would
/// have been written with (`">= 2.0, < 4.0"`).
///
/// `path` is where the caller found it, so a refusal can name the element
/// instead of the file.
///
/// card_28a5258e5a31: a constraint this cannot read is refused rather than
/// dropped, and the reason is sharper than the one behind the sibling `name`
/// check. Dropping a constraint *widens* the dependency: `rack (>= 2.0, < 4.0)`
/// with an unreadable second pair used to publish as `rack (>= 2.0)`, so the
/// index permitted exactly the `rack 4.0` the gemspec ruled out, and a
/// requirement no pair survived published as `>= 0` — a dependency on every
/// version there will ever be. A resolver cannot notice either: a wider
/// constraint only ever resolves more easily than the one that was meant, so
/// the first sign is a consumer breaking on a version this gem said it could
/// not use.
fn gem_requirement_string(
    requirement: &serde_yaml::Value,
    path: &str,
) -> Result<String, anyhow::Error> {
    let Some(constraints) = requirement
        .get("requirements")
        .and_then(|v| v.as_sequence())
    else {
        // Some tooling writes the requirement as a bare string.
        return requirement
            .as_str()
            .map(str::trim)
            .filter(|spelled| !spelled.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "RubyGems metadata `{path}` must be a Gem::Requirement or a non-empty \
                     constraint string, found {}",
                    yaml_type_name(requirement)
                )
            });
    };

    // `requirements: []` is not damage: it is a requirement declaring no
    // constraint, which is what `Gem::Requirement.default` spells `>= 0`.
    if constraints.is_empty() {
        return Ok(">= 0".to_string());
    }

    let mut spelled = Vec::with_capacity(constraints.len());
    for (position, constraint) in constraints.iter().enumerate() {
        let pair = untagged(constraint).as_sequence().ok_or_else(|| {
            anyhow::anyhow!(
                "RubyGems metadata `{path}.requirements[{position}]` must be an \
                 [operator, version] pair, found {}",
                yaml_type_name(constraint)
            )
        })?;
        let operator = pair
            .first()
            .map(untagged)
            .and_then(serde_yaml::Value::as_str)
            .map(str::trim)
            .filter(|operator| !operator.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "RubyGems metadata `{path}.requirements[{position}]` must open with a \
                     comparison operator, found {}",
                    pair.first().map_or("nothing", yaml_type_name)
                )
            })?;
        let version = pair.get(1).and_then(gem_version_string).ok_or_else(|| {
            anyhow::anyhow!(
                "RubyGems metadata `{path}.requirements[{position}]` must name a version, \
                 found {}",
                pair.get(1).map_or("nothing", yaml_type_name)
            )
        })?;
        spelled.push(format!("{operator} {version}"));
    }

    Ok(spelled.join(", "))
}

/// A `Gem::Version` is a tagged object wrapping a `version` scalar, but a plain
/// string turns up in hand-written and older metadata.
fn gem_version_string(value: &serde_yaml::Value) -> Option<String> {
    if let Some(version) = value.as_str() {
        return Some(version.to_string());
    }
    let inner = value.get("version")?;
    inner
        .as_str()
        .map(str::to_string)
        .or_else(|| inner.as_f64().map(|n| n.to_string()))
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

/// Build the `/api/v1/dependencies.json` response.
///
/// The extensionless legacy endpoint is a different wire protocol: Bundler
/// reads that body with `Marshal.load`, so this value must never be served there.
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

// ── Compact index ─────────────────────────────────────────

/// One version's line in a compact index `info` file.
pub struct CompactIndexVersion {
    pub number: String,
    /// The platform, and only when it is not the default `ruby` — the format
    /// spells `1.0.0-java` out but leaves a plain `ruby` gem as `1.0.0`.
    pub platform: Option<String>,
    pub dependencies: Vec<RubyGemsDep>,
    /// SHA-256 of the `.gem` the client is about to download. A client that
    /// gets one checks the file against it and refuses a mismatch, so it is
    /// omitted rather than faked when the stored file has no digest.
    pub checksum: Option<String>,
    /// `required_ruby_version` — the interpreter constraint the resolver
    /// applies before it looks at a single dependency. Published as the `ruby:`
    /// segment of the requirements chunk.
    pub ruby_version: Option<String>,
    /// `required_rubygems_version`, published as the `rubygems:` segment.
    pub rubygems_version: Option<String>,
}

impl CompactIndexVersion {
    /// The `VERSION[-PLATFORM]` chunk, spelled the same way in both files —
    /// `versions` lists it per gem and `info` opens each line with it.
    pub fn version_and_platform(&self) -> String {
        match self.platform {
            Some(ref platform) => format!("{}-{}", self.number, platform),
            None => self.number.clone(),
        }
    }
}

/// One gem's line in the compact index `versions` file.
pub struct CompactIndexGem {
    pub name: String,
    /// `VERSION[-PLATFORM]` chunks, in the order the `info` file lists them.
    pub versions: Vec<String>,
    /// MD5 of this gem's `info` file — see [`compact_index_info_checksum`].
    pub info_checksum: String,
}

/// Build a gem's `info` file: one line per version.
///
/// ```text
/// ---
/// 1.0.0 rack:>= 2.0&< 4.0,rake:>= 0|checksum:6d2f…
/// ```
///
/// The pipe is always there even with no dependencies, because it is what the
/// client's parser splits on; the requirements after it carry the checksum.
pub fn build_compact_index_info(versions: &[CompactIndexVersion]) -> String {
    let mut out = String::from("---\n");

    for version in versions {
        out.push_str(&version.version_and_platform());
        out.push(' ');

        let deps: Vec<String> = version
            .dependencies
            .iter()
            .map(|dep| format!("{}:{}", dep.name, join_constraints(&dep.requirements)))
            .collect();
        out.push_str(&deps.join(","));

        // Everything after the pipe is a comma-separated `key:value` list, and
        // the two version constraints live there beside the checksum. Without
        // `ruby:`, a gem that needs 3.1 resolves cleanly onto 2.7 and fails at
        // parse time — the resolver had nothing to filter on (card_0c9e858230b6).
        out.push('|');
        let mut requirements: Vec<String> = Vec::new();
        if let Some(ref checksum) = version.checksum {
            requirements.push(format!("checksum:{checksum}"));
        }
        for (key, requirement) in [
            ("ruby", &version.ruby_version),
            ("rubygems", &version.rubygems_version),
        ] {
            if let Some(requirement) = requirement {
                requirements.push(format!("{key}:{}", join_constraints(requirement)));
            }
        }
        out.push_str(&requirements.join(","));
        out.push('\n');
    }

    out
}

/// Build the `versions` file — the index a client reads before anything else.
///
/// `created_at` sits above the `---` separator, which the format calls opaque
/// metadata, so it is passed through as stored rather than reformatted.
pub fn build_compact_index_versions(created_at: &str, gems: &[CompactIndexGem]) -> String {
    let mut out = format!("created_at: {created_at}\n---\n");

    for gem in gems {
        out.push_str(&gem.name);
        out.push(' ');
        out.push_str(&gem.versions.join(","));
        out.push(' ');
        out.push_str(&gem.info_checksum);
        out.push('\n');
    }

    out
}

/// Build the `names` file: every gem name, one per line.
pub fn build_compact_index_names(names: &[String]) -> String {
    let mut out = String::from("---\n");
    for name in names {
        out.push_str(name);
        out.push('\n');
    }
    out
}

/// The checksum the `versions` file publishes for a gem's `info` file.
///
/// MD5, and not a choice: the client caches `info/<gem>` on disk and re-fetches
/// it only when this column differs from its own `Digest::MD5` of the cached
/// copy. It must therefore be the MD5 of the info body byte for byte as it is
/// served — anything else is a permanent cache miss.
pub fn compact_index_info_checksum(info: &str) -> String {
    use md5::{Digest, Md5};

    let mut hasher = Md5::new();
    hasher.update(info.as_bytes());
    hex::encode(hasher.finalize())
}

/// Rewrite a RubyGems requirement string into one compact-index constraint
/// chunk.
///
/// A gemspec spells several constraints on one dependency comma-separated
/// (`">= 2.0, < 4.0"`), but the comma is what separates *dependencies* in this
/// format — the ampersand separates constraints on the same gem. An empty
/// requirement becomes `>= 0`, which is what it means.
fn join_constraints(requirements: &str) -> String {
    let constraints: Vec<&str> = requirements
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();

    if constraints.is_empty() {
        ">= 0".to_string()
    } else {
        constraints.join("&")
    }
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

    /// The gemspec a real `gem build` produces: every dependency is a tagged
    /// `Gem::Dependency`, its constraints are `[operator, Gem::Version]` pairs,
    /// and runtime and development ones sit in the same list.
    #[test]
    fn gemspec_metadata_carries_runtime_dependencies_only() {
        let yaml = r#"--- !ruby/object:Gem::Specification
name: matrix-deps-gem
version: !ruby/object:Gem::Version
  version: '1.0.0'
summary: Depends on things
description: The long form of the summary
homepage: https://example.com/matrix-deps-gem
licenses:
- MIT
- Apache-2.0
dependencies:
- !ruby/object:Gem::Dependency
  name: rack
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - ">="
      - !ruby/object:Gem::Version
        version: '2.0'
    - - "<"
      - !ruby/object:Gem::Version
        version: '4.0'
  type: :runtime
- !ruby/object:Gem::Dependency
  name: rake
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - ">="
      - !ruby/object:Gem::Version
        version: '0'
  type: :runtime
- !ruby/object:Gem::Dependency
  name: rspec
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - "~>"
      - !ruby/object:Gem::Version
        version: '3.0'
  type: :development
"#;

        let data = make_gem(yaml);
        let meta = RubyGemsAdapter
            .extract_metadata("matrix-deps-gem-1.0.0.gem", &data)
            .unwrap();

        let stored: serde_json::Value =
            serde_json::from_str(&meta.protocol_metadata.expect("no protocol metadata")).unwrap();

        assert_eq!(stored["summary"], "Depends on things");
        assert_eq!(stored["description"], "The long form of the summary");
        assert_eq!(stored["homepage"], "https://example.com/matrix-deps-gem");
        assert_eq!(stored["licenses"], serde_json::json!(["MIT", "Apache-2.0"]));

        // Two constraints on one gem stay comma-separated here; the compact
        // index is where they become `&`-joined.
        assert_eq!(
            stored["dependencies"],
            serde_json::json!([
                { "name": "rack", "requirements": ">= 2.0, < 4.0" },
                { "name": "rake", "requirements": ">= 0" },
            ]),
            "development dependency leaked into the index, or a constraint was lost"
        );
    }

    /// A gem with no dependencies still records the empty list: the endpoints
    /// cannot tell "resolved to nothing" from "never written" otherwise.
    #[test]
    fn gemspec_metadata_records_an_empty_dependency_list() {
        let data = make_gem("name: matrix-bare\nversion: '1.0.0'\n");
        let meta = RubyGemsAdapter
            .extract_metadata("matrix-bare-1.0.0.gem", &data)
            .unwrap();

        let stored: serde_json::Value =
            serde_json::from_str(&meta.protocol_metadata.expect("no protocol metadata")).unwrap();
        assert_eq!(stored["dependencies"], serde_json::json!([]));
        assert!(stored.get("licenses").is_none());
    }

    /// Gems packed before RubyGems 1.4 spell the requirement
    /// `version_requirements`, and they stay installable forever.
    #[test]
    fn gemspec_metadata_reads_the_legacy_requirement_key() {
        let yaml = r#"--- !ruby/object:Gem::Specification
name: matrix-legacy
version: '1.0.0'
dependencies:
- !ruby/object:Gem::Dependency
  name: rack
  version_requirements: !ruby/object:Gem::Requirement
    requirements:
    - - ">="
      - !ruby/object:Gem::Version
        version: '1.0'
"#;

        let data = make_gem(yaml);
        let meta = RubyGemsAdapter
            .extract_metadata("matrix-legacy-1.0.0.gem", &data)
            .unwrap();
        let stored: serde_json::Value =
            serde_json::from_str(&meta.protocol_metadata.unwrap()).unwrap();

        assert_eq!(
            stored["dependencies"],
            serde_json::json!([{ "name": "rack", "requirements": ">= 1.0" }]),
            "a dependency with no `type` is a runtime one"
        );
    }

    /// A gemspec whose `dependencies:` block is `deps`, wrapped in the rest of
    /// the document `gem build` writes.
    fn gem_declaring(deps: &str) -> Vec<u8> {
        make_gem(&format!(
            "--- !ruby/object:Gem::Specification\nname: matrix-deps-gem\nversion: '1.0.0'\ndependencies:\n{deps}"
        ))
    }

    /// card_69cfa8de4fd1: the dependency list is the resolver input, so a
    /// runtime entry the index cannot carry has to stop the publish. Dropping
    /// it publishes a gem that depends on the *remaining* entries — a graph
    /// Bundler resolves cleanly before failing far away, at a constant that
    /// never got required. The refusal names the element by its position in the
    /// declared list.
    #[test]
    fn a_runtime_dependency_the_index_cannot_carry_refuses_the_gem() {
        for (declared, expected) in [
            // The headline shape: one good dependency and one malformed, which
            // used to publish as a gem that needs only the good one.
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 - !ruby/object:Gem::Dependency\n  name: 42\n  type: :runtime\n",
                "`dependencies[1].name` must be a non-empty string, found a number",
            ),
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 - !ruby/object:Gem::Dependency\n  type: :runtime\n",
                "`dependencies[1].name` is missing",
            ),
            (
                "- !ruby/object:Gem::Dependency\n  name: ''\n  type: :runtime\n",
                "`dependencies[0].name` must be a non-empty string, found a string",
            ),
            (
                "- !ruby/object:Gem::Dependency\n  name:\n  type: :runtime\n",
                "`dependencies[0].name` must be a non-empty string, found null",
            ),
            (
                "- !ruby/object:Gem::Dependency\n  name:\n  - rack\n  type: :runtime\n",
                "`dependencies[0].name` must be a non-empty string, found a list",
            ),
            // Not a `Gem::Dependency` at all — the whole entry is unreadable,
            // and the name is the first thing missing from it.
            ("- rack\n", "`dependencies[0].name` is missing"),
            // `type` decides whether the entry reaches the index at all, so a
            // value that is not a symbol cannot be read as "runtime, presumably".
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: 42\n",
                "`dependencies[0].type` must be :runtime or :development",
            ),
        ] {
            let data = gem_declaring(declared);
            let error = RubyGemsAdapter
                .extract_metadata("matrix-deps-gem-1.0.0.gem", &data)
                .err()
                .unwrap_or_else(|| panic!("`{declared}` must not publish"));
            let error = format!("{error:#}");
            assert!(
                error.contains(expected),
                "`{declared}` must name what it refused, got: {error}"
            );
            // The same gem through the gate every publish runs.
            assert!(
                RubyGemsAdapter.validate(&data).is_err(),
                "`{declared}` reached the registry through `validate`"
            );
        }
    }

    /// A `dependencies:` key that is not a list is the same lie one level up:
    /// every dependency the gem declares would vanish at once.
    #[test]
    fn a_dependencies_key_that_is_not_a_list_refuses_the_gem() {
        let data = gem_declaring("  rack: '>= 2.0'\n");
        let error = RubyGemsAdapter
            .extract_metadata("matrix-deps-gem-1.0.0.gem", &data)
            .expect_err("a mapping `dependencies` must not publish");
        let error = format!("{error:#}");
        assert!(
            error.contains("`dependencies` must be a list of Gem::Dependency, found a mapping"),
            "got: {error}"
        );
    }

    /// The boundary of the refusal: a development dependency never reaches the
    /// index, so nothing about it can be published wrong and it must not gate a
    /// gem the resolver would otherwise read correctly.
    #[test]
    fn a_malformed_development_dependency_does_not_gate_the_publish() {
        let data = gem_declaring(
            "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
             - !ruby/object:Gem::Dependency\n  name: 42\n  type: :development\n",
        );
        let meta = RubyGemsAdapter
            .extract_metadata("matrix-deps-gem-1.0.0.gem", &data)
            .expect("a development dependency is not resolver input");
        let stored: serde_json::Value =
            serde_json::from_str(&meta.protocol_metadata.unwrap()).unwrap();

        assert_eq!(
            stored["dependencies"],
            serde_json::json!([{ "name": "rack", "requirements": ">= 0" }]),
        );
    }

    /// The discrimination: an absent list, an empty list and a list of proper
    /// `Gem::Dependency` objects are all legitimate and still publish unchanged.
    /// A gemspec that declares its symbols under an explicit Ruby tag is one of
    /// them — the refusals above read the shape, not the spelling.
    #[test]
    fn a_well_formed_dependency_list_still_publishes() {
        let empty = RubyGemsAdapter
            .extract_metadata(
                "matrix-deps-gem-1.0.0.gem",
                &make_gem("name: matrix-deps-gem\nversion: '1.0.0'\ndependencies: []\n"),
            )
            .expect("an empty list is a gem that needs nothing");
        let stored: serde_json::Value =
            serde_json::from_str(&empty.protocol_metadata.unwrap()).unwrap();
        assert_eq!(stored["dependencies"], serde_json::json!([]));

        let tagged = RubyGemsAdapter
            .extract_metadata(
                "matrix-deps-gem-1.0.0.gem",
                &gem_declaring(
                    "- !ruby/object:Gem::Dependency\n  name: rack\n  type: !ruby/symbol runtime\n",
                ),
            )
            .expect("a tagged Ruby symbol is still a symbol");
        let stored: serde_json::Value =
            serde_json::from_str(&tagged.protocol_metadata.unwrap()).unwrap();
        assert_eq!(
            stored["dependencies"],
            serde_json::json!([{ "name": "rack", "requirements": ">= 0" }]),
        );
    }

    /// card_28a5258e5a31: a constraint the requirement cannot carry *widens*
    /// the dependency when it is dropped, which is worse than losing it. The
    /// first case is the headline: `rack (>= 2.0, < 4.0)` used to publish as
    /// `rack (>= 2.0)`, so the index permitted the `rack 4.0` the gemspec ruled
    /// out — and the resolver cannot object, because the wider constraint is
    /// the easier one to satisfy.
    #[test]
    fn a_requirement_the_index_cannot_carry_refuses_the_gem() {
        for (declared, expected) in [
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 requirement: !ruby/object:Gem::Requirement\n\
                 \x20   requirements:\n\
                 \x20   - - \">=\"\n\
                 \x20     - !ruby/object:Gem::Version\n\
                 \x20       version: '2.0'\n\
                 \x20   - - \"<\"\n\
                 \x20     - 4\n",
                "`dependencies[0].requirement.requirements[1]` must name a version, found a number",
            ),
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 requirement: !ruby/object:Gem::Requirement\n\
                 \x20   requirements:\n\
                 \x20   - - 1\n\
                 \x20     - '2.0'\n",
                "`dependencies[0].requirement.requirements[0]` must open with a comparison \
                 operator, found a number",
            ),
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 requirement: !ruby/object:Gem::Requirement\n\
                 \x20   requirements:\n\
                 \x20   - \">= 2.0\"\n",
                "`dependencies[0].requirement.requirements[0]` must be an [operator, version] \
                 pair, found a string",
            ),
            // The whole requirement is unreadable — the shape that used to
            // publish as `>= 0`, a dependency on every version there will be.
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 requirement: 42\n",
                "`dependencies[0].requirement` must be a Gem::Requirement or a non-empty \
                 constraint string, found a number",
            ),
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 requirement: ''\n",
                "`dependencies[0].requirement` must be a Gem::Requirement or a non-empty \
                 constraint string, found a string",
            ),
            // The legacy spelling is the same path and names itself.
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 version_requirements: !ruby/object:Gem::Requirement\n\
                 \x20   requirements:\n\
                 \x20   - - \">=\"\n\
                 \x20     - !ruby/object:Gem::Version\n\
                 \x20       version:\n",
                "`dependencies[0].version_requirements.requirements[0]` must name a version, \
                 found a mapping",
            ),
        ] {
            let data = gem_declaring(declared);
            let error = RubyGemsAdapter
                .extract_metadata("matrix-deps-gem-1.0.0.gem", &data)
                .err()
                .unwrap_or_else(|| panic!("`{declared}` must not publish"));
            let error = format!("{error:#}");
            assert!(
                error.contains(expected),
                "`{declared}` must name what it refused, got: {error}"
            );
            // The same gem through the gate every publish runs.
            assert!(
                RubyGemsAdapter.validate(&data).is_err(),
                "`{declared}` reached the registry through `validate`"
            );
        }
    }

    /// The interpreter constraints go through the same reader, and a broken one
    /// used to vanish from the metadata entirely — which is the failure the
    /// field exists to prevent: a gem needing 3.1 resolves cleanly onto 2.7 and
    /// fails at parse time.
    #[test]
    fn an_unreadable_interpreter_constraint_refuses_the_gem() {
        let error = parse_gemspec_yaml(
            "name: nokogiri\nversion: 1.16.0\n\
             required_ruby_version: !ruby/object:Gem::Requirement\n\
             \x20 requirements:\n\
             \x20 - - \">=\"\n\
             \x20   - []\n",
        )
        .expect_err("an unreadable interpreter constraint must not publish silently");
        let error = format!("{error:#}");
        assert!(
            error.contains(
                "`required_ruby_version.requirements[0]` must name a version, found a list"
            ),
            "got: {error}"
        );
    }

    /// The discrimination the refusal has to keep: no requirement at all, a
    /// requirement declaring no constraint, a bare constraint string and a
    /// two-pair `Gem::Requirement` are all legitimate and publish unchanged.
    #[test]
    fn a_well_formed_requirement_still_publishes() {
        for (declared, expected) in [
            // Absent — `Gem::Requirement.default`, the one honest `>= 0`.
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n",
                ">= 0",
            ),
            // Declared, and declaring no constraint. Same meaning, said out loud.
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 requirement: !ruby/object:Gem::Requirement\n\
                 \x20   requirements: []\n",
                ">= 0",
            ),
            // Some tooling writes the requirement as a bare string.
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 requirement: \">= 2.0\"\n",
                ">= 2.0",
            ),
            // The shape that must survive intact: both halves of a range.
            (
                "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
                 \x20 requirement: !ruby/object:Gem::Requirement\n\
                 \x20   requirements:\n\
                 \x20   - - \">=\"\n\
                 \x20     - !ruby/object:Gem::Version\n\
                 \x20       version: '2.0'\n\
                 \x20   - - \"<\"\n\
                 \x20     - !ruby/object:Gem::Version\n\
                 \x20       version: '4.0'\n",
                ">= 2.0, < 4.0",
            ),
        ] {
            let meta = RubyGemsAdapter
                .extract_metadata("matrix-deps-gem-1.0.0.gem", &gem_declaring(declared))
                .unwrap_or_else(|error| panic!("`{declared}` must publish, got: {error:#}"));
            let stored: serde_json::Value =
                serde_json::from_str(&meta.protocol_metadata.unwrap()).unwrap();
            assert_eq!(
                stored["dependencies"],
                serde_json::json!([{ "name": "rack", "requirements": expected }]),
                "`{declared}` changed shape"
            );
        }
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

    #[test]
    fn info_line_separates_dependencies_from_constraints() {
        let info = build_compact_index_info(&[CompactIndexVersion {
            number: "1.0.0".into(),
            platform: None,
            dependencies: vec![
                RubyGemsDep {
                    name: "rack".into(),
                    // One dependency, two constraints: the comma a gemspec uses
                    // here separates *dependencies* in this format.
                    requirements: ">= 2.0, < 4.0".into(),
                },
                RubyGemsDep {
                    name: "rake".into(),
                    requirements: String::new(),
                },
            ],
            checksum: Some("6d2f".into()),
            ruby_version: None,
            rubygems_version: None,
        }]);

        assert_eq!(
            info,
            "---\n1.0.0 rack:>= 2.0&< 4.0,rake:>= 0|checksum:6d2f\n"
        );
    }

    #[test]
    fn info_line_keeps_the_pipe_without_dependencies_or_checksum() {
        // The client's parser splits on the pipe before it looks at either
        // side, so a line missing it is not an empty entry — it is unparseable.
        let info = build_compact_index_info(&[CompactIndexVersion {
            number: "1.0.0".into(),
            platform: Some("java".into()),
            dependencies: Vec::new(),
            checksum: None,
            ruby_version: None,
            rubygems_version: None,
        }]);

        assert_eq!(info, "---\n1.0.0-java |\n");
    }

    #[test]
    fn versions_file_lists_each_gem_with_its_info_checksum() {
        let info = build_compact_index_info(&[CompactIndexVersion {
            number: "1.0.0".into(),
            platform: None,
            dependencies: Vec::new(),
            checksum: Some("abc".into()),
            ruby_version: None,
            rubygems_version: None,
        }]);
        let checksum = compact_index_info_checksum(&info);

        let versions = build_compact_index_versions(
            "2026-07-28T00:00:00Z",
            &[CompactIndexGem {
                name: "matrix-gem".into(),
                versions: vec!["1.0.0".into(), "1.1.0-java".into()],
                info_checksum: checksum.clone(),
            }],
        );

        assert_eq!(
            versions,
            format!(
                "created_at: 2026-07-28T00:00:00Z\n---\nmatrix-gem 1.0.0,1.1.0-java {checksum}\n"
            )
        );
        // MD5 of the info body, not of anything else: the client compares this
        // column against its own digest of the cached file.
        use md5::{Digest, Md5};
        assert_eq!(checksum, format!("{:x}", Md5::digest(info.as_bytes())));
    }

    /// Everything after the pipe is a `key:value` list, and `ruby:` is the
    /// constraint a resolver filters candidates on before it looks at a single
    /// dependency. Its several constraints join with `&`, like a dependency's —
    /// the comma belongs to the list.
    #[test]
    fn info_line_publishes_the_interpreter_constraints_beside_the_checksum() {
        let info = build_compact_index_info(&[CompactIndexVersion {
            number: "1.0.0".into(),
            platform: Some("x86_64-linux".into()),
            dependencies: vec![RubyGemsDep {
                name: "rack".into(),
                requirements: ">= 2.0".into(),
            }],
            checksum: Some("6d2f".into()),
            ruby_version: Some(">= 3.1, < 4.0".into()),
            rubygems_version: Some(">= 3.0".into()),
        }]);

        assert_eq!(
            info,
            "---\n1.0.0-x86_64-linux rack:>= 2.0|checksum:6d2f,ruby:>= 3.1&< 4.0,rubygems:>= 3.0\n"
        );

        // A gem that declares neither says nothing after the pipe but the
        // checksum — an empty `ruby:` would be a constraint of its own.
        let info = build_compact_index_info(&[CompactIndexVersion {
            number: "1.0.0".into(),
            platform: None,
            dependencies: Vec::new(),
            checksum: Some("abc".into()),
            ruby_version: None,
            rubygems_version: None,
        }]);
        assert_eq!(info, "---\n1.0.0 |checksum:abc\n");
    }

    /// The platform is what Bundler keys a candidate on together with the
    /// version, and what the compact index turns into a download URL. It is
    /// recorded only when it differs from `ruby`, because a spurious `-ruby`
    /// suffix names a file that does not exist.
    #[test]
    fn gemspec_records_the_platform_and_the_interpreter_constraints() {
        let native = parse_gemspec_yaml(
            "name: nokogiri\nversion: 1.16.0\nplatform: x86_64-linux\n\
             required_ruby_version: !ruby/object:Gem::Requirement\n\
             \x20 requirements:\n\
             \x20 - - \">=\"\n\
             \x20   - !ruby/object:Gem::Version\n\
             \x20     version: '3.1'\n",
        )
        .unwrap();
        let native: serde_json::Value =
            serde_json::from_str(&native.protocol_metadata.unwrap()).unwrap();
        assert_eq!(native["platform"], "x86_64-linux");
        assert_eq!(native["required_ruby_version"], ">= 3.1");

        // The default is an absence, not the string `ruby`.
        let pure = parse_gemspec_yaml("name: rack\nversion: 3.0.0\nplatform: ruby\n").unwrap();
        let pure: serde_json::Value =
            serde_json::from_str(&pure.protocol_metadata.unwrap()).unwrap();
        assert!(pure["platform"].is_null(), "{pure}");
        assert!(pure["required_ruby_version"].is_null(), "{pure}");

        // Gems packed before the field was flattened carry a `Gem::Platform`.
        let legacy = parse_gemspec_yaml(
            "name: old\nversion: 0.1.0\nplatform:\n  cpu: x86_64\n  os: darwin\n  version: '19'\n",
        )
        .unwrap();
        let legacy: serde_json::Value =
            serde_json::from_str(&legacy.protocol_metadata.unwrap()).unwrap();
        assert_eq!(legacy["platform"], "x86_64-darwin-19");
    }

    #[test]
    fn names_file_is_one_gem_per_line_under_the_separator() {
        let names = build_compact_index_names(&["a-gem".to_string(), "b-gem".to_string()]);
        assert_eq!(names, "---\na-gem\nb-gem\n");
    }
}
