//! Helm (Kubernetes) chart adapter.
//!
//! Handles `.tgz` / `.tar.gz` archives containing:
//! - `Chart.yaml` — chart metadata (required)
//! - `values.yaml` — default values
//! - `templates/` — Kubernetes resource templates
//!
//! ## Helm Repository API
//!
//! Helm clients expect:
//! - `GET /index.yaml` — repository index listing all charts
//! - Chart files served at paths relative to the index
//!
//! ForgeKeep serves these at:
//! - Index: `GET /api/v1/repos/{owner}/{repo}/packages/helm/index.yaml`
//! - Download: standard package download endpoint

use flate2::read::GzDecoder;
use rg_db::package_version_key::helm_version_key;
use std::io::Read;

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct HelmAdapter;

impl PackageAdapter for HelmAdapter {
    fn package_type() -> &'static str {
        "helm"
    }

    fn extract_metadata(
        &self,
        _filename: &str,
        data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        extract_from_chart(data)
    }

    /// A chart whose `Chart.yaml` does not parse is not a well-formed chart, so
    /// the manifest is read here and not only in `extract_metadata`.
    ///
    /// This used to check that `Chart.yaml` *existed* and stop there, which put
    /// the only parse of it behind `extract_metadata` — whose verdict the
    /// publish handler was free to ignore, and did. A chart with a corrupt
    /// `Chart.yaml` and explicit `?name=&version=` was accepted, stored, and
    /// then served to `helm` clients that cannot install it. `ComposerAdapter`
    /// has always parsed its `composer.json` here; this is the same rule.
    fn validate(&self, data: &[u8]) -> Result<(), anyhow::Error> {
        if data.len() < 2 || data[0] != 0x1f || data[1] != 0x8b {
            anyhow::bail!("invalid Helm chart: not a gzip file");
        }

        // Locates `Chart.yaml` and parses it — "no Chart.yaml found" is its own
        // error, so the presence check is not lost by folding the two together.
        extract_from_chart(data)?;
        Ok(())
    }

    /// A chart archive carries exactly one `Chart.yaml`, and `name`/`version`
    /// are required fields of it.
    fn manifest_is_authoritative(&self) -> bool {
        true
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        if filename.ends_with(".tgz") || filename.ends_with(".tar.gz") {
            "application/gzip".into()
        } else {
            self.default_content_type().into()
        }
    }

    fn default_content_type(&self) -> &'static str {
        "application/gzip"
    }

    fn has_protocol_endpoint(&self) -> bool {
        true
    }
}

/// Extract metadata from a Helm chart (.tgz containing Chart.yaml).
fn extract_from_chart(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let decoder = GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);

    let mut chart_yaml = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();

        // Chart.yaml is in the chart root directory: {chartname}/Chart.yaml
        if path.file_name().map(|n| n == "Chart.yaml").unwrap_or(false) {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            chart_yaml = Some(content);
            break;
        }
    }

    let yaml_str =
        chart_yaml.ok_or_else(|| anyhow::anyhow!("invalid Helm chart: no Chart.yaml found"))?;

    parse_chart_yaml(&yaml_str)
}

/// Parse Chart.yaml content.
fn parse_chart_yaml(yaml: &str) -> Result<ExtractedMetadata, anyhow::Error> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| anyhow::anyhow!("invalid Chart.yaml: {e}"))?;

    let name = doc
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Chart.yaml missing 'name'"))?
        .to_string();

    let version_value = doc
        .get("version")
        .ok_or_else(|| anyhow::anyhow!("Chart.yaml missing 'version'"))?;
    let version = version_value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Chart.yaml 'version' must be a string"))?
        .to_string();
    if helm_version_key(&version).is_none() {
        anyhow::bail!("Chart.yaml 'version' is not a valid Helm semantic version");
    }

    let description = doc
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let homepage = doc
        .get("home")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let keywords = doc
        .get("keywords")
        .and_then(|v| {
            v.as_sequence().map(|seq| {
                seq.iter()
                    .filter_map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        })
        .filter(|s| !s.is_empty());

    // Helm charts can have a "sources" list for repository URLs
    let repository_url = doc
        .get("sources")
        .and_then(|v| v.as_sequence())
        .and_then(|seq| seq.first())
        .and_then(|s| s.as_str())
        .map(String::from);

    Ok(ExtractedMetadata {
        name,
        version: version.clone(),
        description,
        homepage,
        repository_url,
        keywords,
        license: None, // Helm Chart.yaml doesn't standardize license
        semver: Some(version),
        protocol_metadata: chart_protocol_metadata(&doc)?,
    })
}

/// The Chart.yaml fields `index.yaml` republishes but no package column holds,
/// keyed the way `parse_helm_metadata` (rg-http) reads them back.
///
/// `apiVersion` is the load-bearing one: Helm decides how to read a chart from
/// it, so an index entry missing it describes a chart the client cannot place.
/// `kubeVersion` and `deprecated` are next: `helm install` refuses a chart whose
/// `kubeVersion` range the cluster does not satisfy, and `helm search repo`
/// hides deprecated charts — both decided from the *index*, not the archive, by
/// every client that resolves before downloading (Flux's `HelmRepository`,
/// ChartMuseum mirrors). A chart that omits them from the index looks
/// installable and current when it is neither.
/// `None` when the chart declared none of these.
///
/// card_33f83c325515: a key the chart *declared* but this cannot read is
/// refused, not skipped. Skipping said the opposite of what the chart wrote —
/// `dependencies:` spelled as a map rather than a list published the chart as
/// needing no subcharts at all, and one element that would not convert fell out
/// while the rest stayed, so the list still looked whole. The doctrine this
/// block is built on ("rendering it here rather than reshaping it keeps the
/// index entry a faithful `ChartMetadata`, which is what a mirror re-serves
/// verbatim") only holds up to the first unreadable element.
///
/// Severity below the resolver-graph cases in `sol_c6e0f247ca58`: Helm installs
/// subcharts out of `charts/` inside the `.tgz` and not out of the index, so no
/// resolver is misled — but `helm show chart`, `helm search repo` and every
/// mirror that re-serves this entry are.
///
/// The distinction that has to survive: a key that is simply absent (or an
/// explicit YAML `null`) is not damage, and neither is an empty list.
fn chart_protocol_metadata(doc: &serde_yaml::Value) -> Result<Option<String>, anyhow::Error> {
    let mut out = serde_json::Map::new();
    let unreadable = |key: &str, must_be: &str, found: &serde_yaml::Value| {
        anyhow::anyhow!(
            "Chart.yaml `{key}` must be {must_be}, found {}",
            yaml_type_name(found)
        )
    };

    for key in ["appVersion", "apiVersion", "kubeVersion", "type"] {
        let Some(declared) = declared(doc, key) else {
            continue;
        };
        // `appVersion: 1.19` is a number to YAML and a string to Helm.
        let value = declared
            .as_str()
            .map(str::to_string)
            .or_else(|| declared.as_f64().map(|n| n.to_string()))
            .or_else(|| declared.as_i64().map(|n| n.to_string()))
            .ok_or_else(|| unreadable(key, "a string or a number", declared))?;
        // An empty spelling is readable and simply says nothing, unlike a
        // value of the wrong shape.
        if !value.is_empty() {
            out.insert(key.into(), value.into());
        }
    }

    for key in ["keywords", "sources"] {
        let Some(declared) = declared(doc, key) else {
            continue;
        };
        let seq = declared
            .as_sequence()
            .ok_or_else(|| unreadable(key, "a list", declared))?;
        let mut values = Vec::with_capacity(seq.len());
        for (position, entry) in seq.iter().enumerate() {
            values.push(serde_json::Value::from(entry.as_str().ok_or_else(
                || unreadable(&format!("{key}[{position}]"), "a string", entry),
            )?));
        }
        if !values.is_empty() {
            out.insert(key.into(), values.into());
        }
    }

    // Only a `true` is worth recording: Helm's own `ChartMetadata` omits the
    // key when the chart is current, and writing `deprecated: false` into every
    // entry would say the registry checked something it merely defaulted. A
    // value that is not a boolean at all is a different matter — read as
    // `false` it inverts the claim rather than losing it.
    if let Some(declared) = declared(doc, "deprecated") {
        let deprecated = declared
            .as_bool()
            .ok_or_else(|| unreadable("deprecated", "a boolean", declared))?;
        if deprecated {
            out.insert("deprecated".into(), true.into());
        }
    }

    // `dependencies` travels as the chart spells it — a list of tables with
    // `name` / `version` / `repository` and optional `condition` / `tags` /
    // `alias`. Rendering it here rather than reshaping it keeps the index entry
    // a faithful `ChartMetadata`, which is what a mirror re-serves verbatim.
    if let Some(declared) = declared(doc, "dependencies") {
        let seq = declared
            .as_sequence()
            .ok_or_else(|| unreadable("dependencies", "a list", declared))?;
        let mut deps = Vec::with_capacity(seq.len());
        for (position, dep) in seq.iter().enumerate() {
            deps.push(serde_json::to_value(dep).map_err(|error| {
                anyhow::anyhow!(
                    "Chart.yaml `dependencies[{position}]` cannot be carried into the \
                     index entry: {error}"
                )
            })?);
        }
        if !deps.is_empty() {
            out.insert("dependencies".into(), deps.into());
        }
    }

    Ok((!out.is_empty()).then(|| serde_json::Value::Object(out).to_string()))
}

/// The value a `Chart.yaml` actually declared under `key`, if it declared one.
///
/// A key that is absent and one written with no value (`kubeVersion:`, which
/// YAML reads as `null`) are the same claim, and neither is damage.
fn declared<'a>(doc: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    doc.get(key).filter(|value| !value.is_null())
}

/// What a refusal calls the shape it found, so the message names a type rather
/// than echoing the chart back at its author.
fn yaml_type_name(value: &serde_yaml::Value) -> &'static str {
    match value {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "a boolean",
        serde_yaml::Value::Number(_) => "a number",
        serde_yaml::Value::String(_) => "a string",
        serde_yaml::Value::Sequence(_) => "a list",
        serde_yaml::Value::Mapping(_) => "a map",
        serde_yaml::Value::Tagged(_) => "a tagged value",
    }
}

// ── Helm repository index helpers ─────────────────────────

/// Entry for one chart version in index.yaml.
pub struct HelmIndexEntry {
    pub name: String,
    pub version: String,
    pub app_version: Option<String>,
    pub description: Option<String>,
    pub api_version: Option<String>,
    /// The semver range of Kubernetes the chart supports. `helm install`
    /// refuses a chart the cluster does not satisfy, and decides it from here
    /// when it resolved through the index.
    pub kube_version: Option<String>,
    /// `application` or `library` — a library chart installs nothing on its own.
    pub chart_type: Option<String>,
    /// Written only when true, the way Helm's own `ChartMetadata` writes it:
    /// `helm search repo` hides the chart on this key alone.
    pub deprecated: bool,
    /// The chart's subchart requirements, as `Chart.yaml` spells them.
    pub dependencies: Vec<serde_json::Value>,
    pub home: Option<String>,
    pub sources: Vec<String>,
    pub keywords: Vec<String>,
    pub created: String,
    pub digest: Option<String>,
    pub urls: Vec<String>,
}

/// Build a Helm repository index.yaml.
///
/// Format: https://helm.sh/docs/topics/chart_repository/#the-chart-repository-structure
pub fn build_helm_index(entries: &[HelmIndexEntry]) -> String {
    let mut chart_entries: serde_yaml::Mapping = serde_yaml::Mapping::new();

    // Group entries by chart name
    for entry in entries {
        let chart_list = chart_entries
            .entry(serde_yaml::Value::String(entry.name.clone()))
            .or_insert_with(|| serde_yaml::Value::Sequence(Vec::new()));

        if let serde_yaml::Value::Sequence(ref mut seq) = chart_list {
            let mut ver = serde_yaml::Mapping::new();
            ver.insert("name".into(), entry.name.clone().into());
            ver.insert("version".into(), entry.version.clone().into());
            if let Some(ref av) = entry.app_version {
                ver.insert("appVersion".into(), av.clone().into());
            }
            if let Some(ref desc) = entry.description {
                ver.insert("description".into(), desc.clone().into());
            }
            if let Some(ref api) = entry.api_version {
                ver.insert("apiVersion".into(), api.clone().into());
            }
            if let Some(ref kube) = entry.kube_version {
                ver.insert("kubeVersion".into(), kube.clone().into());
            }
            if let Some(ref chart_type) = entry.chart_type {
                ver.insert("type".into(), chart_type.clone().into());
            }
            if entry.deprecated {
                ver.insert("deprecated".into(), true.into());
            }
            if !entry.dependencies.is_empty() {
                if let Ok(deps) = serde_yaml::to_value(&entry.dependencies) {
                    ver.insert("dependencies".into(), deps);
                }
            }
            if let Some(ref home) = entry.home {
                ver.insert("home".into(), home.clone().into());
            }
            if !entry.sources.is_empty() {
                let sources: Vec<serde_yaml::Value> =
                    entry.sources.iter().map(|s| s.clone().into()).collect();
                ver.insert("sources".into(), serde_yaml::Value::Sequence(sources));
            }
            if !entry.keywords.is_empty() {
                let keywords: Vec<serde_yaml::Value> =
                    entry.keywords.iter().map(|k| k.clone().into()).collect();
                ver.insert("keywords".into(), serde_yaml::Value::Sequence(keywords));
            }
            ver.insert("created".into(), entry.created.clone().into());
            if let Some(ref d) = entry.digest {
                ver.insert("digest".into(), d.clone().into());
            }
            let urls: Vec<serde_yaml::Value> =
                entry.urls.iter().map(|u| u.clone().into()).collect();
            ver.insert("urls".into(), serde_yaml::Value::Sequence(urls));

            seq.push(serde_yaml::Value::Mapping(ver));
        }
    }

    let mut root = serde_yaml::Mapping::new();
    root.insert("apiVersion".into(), "v1".into());
    root.insert("generated".into(), chrono::Utc::now().to_rfc3339().into());
    root.insert("entries".into(), serde_yaml::Value::Mapping(chart_entries));

    let value = serde_yaml::Value::Mapping(root);
    serde_yaml::to_string(&value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    /// Create a minimal .tgz Helm chart for testing.
    fn make_chart(chart_yaml: &str) -> Vec<u8> {
        // Create a tar archive with Chart.yaml
        let mut tar_buf = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_path("mychart/Chart.yaml").unwrap();
            header.set_size(chart_yaml.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append(&header, chart_yaml.as_bytes()).unwrap();
            tar.finish().unwrap();
        }

        // Gzip the tar
        let mut gz_buf = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut gz_buf, Compression::default());
            encoder.write_all(&tar_buf).unwrap();
            encoder.finish().unwrap();
        }

        gz_buf
    }

    #[test]
    fn test_extract_chart_basic() {
        let yaml = r#"apiVersion: v2
name: nginx
version: 1.2.3
description: A Helm chart for nginx
home: https://github.com/kubernetes/ingress-nginx
keywords:
  - nginx
  - ingress
  - web
sources:
  - https://github.com/kubernetes/ingress-nginx
"#;

        let data = make_chart(yaml);
        let adapter = HelmAdapter;

        let meta = adapter.extract_metadata("nginx-1.2.3.tgz", &data).unwrap();
        assert_eq!(meta.name, "nginx");
        assert_eq!(meta.version, "1.2.3");
        assert_eq!(meta.description.unwrap(), "A Helm chart for nginx");
        assert_eq!(
            meta.homepage.unwrap(),
            "https://github.com/kubernetes/ingress-nginx"
        );
        assert_eq!(meta.keywords.unwrap(), "nginx, ingress, web");
        assert!(meta.repository_url.is_some());
    }

    /// `index.yaml` republishes `apiVersion` / `appVersion` / `keywords` /
    /// `sources`, and none of them has a package column — they only survive the
    /// publish if the adapter carries them across.
    #[test]
    fn chart_metadata_carries_the_index_only_fields() {
        let yaml = r#"apiVersion: v2
name: nginx
version: 1.2.3
appVersion: 1.19
keywords:
  - web
  - proxy
sources:
  - https://github.com/kubernetes/ingress-nginx
"#;

        let meta = HelmAdapter
            .extract_metadata("nginx-1.2.3.tgz", &make_chart(yaml))
            .unwrap();
        let stored: serde_json::Value =
            serde_json::from_str(&meta.protocol_metadata.expect("no protocol metadata")).unwrap();

        assert_eq!(stored["apiVersion"], "v2");
        // `appVersion: 1.19` is a number to YAML and a string to Helm.
        assert_eq!(stored["appVersion"], "1.19");
        assert_eq!(stored["keywords"], serde_json::json!(["web", "proxy"]));
        assert_eq!(
            stored["sources"],
            serde_json::json!(["https://github.com/kubernetes/ingress-nginx"])
        );
    }

    #[test]
    fn chart_metadata_is_absent_when_the_chart_declares_none_of_it() {
        let meta = HelmAdapter
            .extract_metadata(
                "bare-1.0.0.tgz",
                &make_chart("name: bare\nversion: 1.0.0\n"),
            )
            .unwrap();
        assert!(meta.protocol_metadata.is_none());
    }

    /// card_33f83c325515: a key the chart declared but the index entry cannot
    /// carry is refused rather than skipped. The headline is `dependencies:`
    /// written as a map — the whole key used to fall out and the chart
    /// published as needing no subcharts at all.
    #[test]
    fn a_chart_key_the_index_entry_cannot_carry_refuses_the_chart() {
        for (label, declared, expected) in [
            (
                "dependencies as a map",
                "dependencies:\n  common:\n    version: 1.0.0\n",
                "`dependencies` must be a list, found a map",
            ),
            (
                "dependencies as a string",
                "dependencies: common\n",
                "`dependencies` must be a list, found a string",
            ),
            (
                "deprecated as a string",
                "deprecated: \"yes\"\n",
                "`deprecated` must be a boolean, found a string",
            ),
            (
                "apiVersion as a list",
                "apiVersion:\n  - v2\n",
                "`apiVersion` must be a string or a number, found a list",
            ),
            (
                "a keyword that is not a string",
                "keywords:\n  - ok\n  - [nested]\n",
                "`keywords[1]` must be a string, found a list",
            ),
            (
                "a source that is not a string",
                "sources:\n  - https://example.test\n  - 7\n",
                "`sources[1]` must be a string, found a number",
            ),
        ] {
            let yaml = format!("name: chart\nversion: 1.0.0\n{declared}");
            let error = HelmAdapter
                .extract_metadata("chart-1.0.0.tgz", &make_chart(&yaml))
                .expect_err(&format!("{label}: an unreadable key must be refused"))
                .to_string();
            assert!(
                error.contains(expected),
                "{label}: the refusal must name the key it refused, got: {error}"
            );

            // `validate` is the gate every publish runs, so the refusal has to
            // reach it and not stop at `extract_metadata`.
            assert!(
                HelmAdapter.validate(&make_chart(&yaml)).is_err(),
                "{label}: the refusal must reach validate"
            );
        }

        // The discrimination: absence, an explicit YAML null and an empty list
        // are all a chart declaring nothing, not damage — and `deprecated:
        // false` is the ordinary spelling of a current chart.
        let meta = HelmAdapter
            .extract_metadata(
                "chart-1.0.0.tgz",
                &make_chart(
                    "name: chart\nversion: 1.0.0\nkubeVersion:\ndependencies: []\n\
                     keywords: []\ndeprecated: false\napiVersion: v2\n",
                ),
            )
            .expect("a chart that declares nothing unreadable must still publish");
        let stored: serde_json::Value =
            serde_json::from_str(&meta.protocol_metadata.expect("apiVersion was declared"))
                .unwrap();
        assert_eq!(stored["apiVersion"], "v2");
        assert!(
            stored.get("dependencies").is_none()
                && stored.get("keywords").is_none()
                && stored.get("kubeVersion").is_none()
                && stored.get("deprecated").is_none(),
            "an empty or absent declaration must not be written as a claim: {stored}"
        );
    }

    #[test]
    fn test_validate_valid_chart() {
        let yaml = "name: test\nversion: 1.0.0\n";
        let data = make_chart(yaml);
        let adapter = HelmAdapter;
        assert!(adapter.validate(&data).is_ok());
    }

    #[test]
    fn test_validate_rejects_non_gzip() {
        let adapter = HelmAdapter;
        let err = adapter.validate(b"not a gzip file").unwrap_err();
        assert!(err.to_string().contains("not a gzip"));
    }

    #[test]
    fn test_validate_rejects_no_chart_yaml() {
        // Create a .tgz without Chart.yaml
        let mut tar_buf = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_path("values.yaml").unwrap();
            header.set_size(3);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append(&header, &b"{}"[..]).unwrap();
            tar.finish().unwrap();
        }
        let mut gz_buf = Vec::new();
        {
            let mut encoder = GzEncoder::new(&mut gz_buf, Compression::default());
            encoder.write_all(&tar_buf).unwrap();
            encoder.finish().unwrap();
        }

        let adapter = HelmAdapter;
        let err = adapter.validate(&gz_buf).unwrap_err();
        assert!(err.to_string().contains("no Chart.yaml"));
    }

    #[test]
    fn chart_version_must_be_a_string() {
        let yaml = "name: test\nversion: 1.0\n";
        let data = make_chart(yaml);
        let adapter = HelmAdapter;
        let error = adapter.extract_metadata("test-1.0.tgz", &data).unwrap_err();
        assert!(error.to_string().contains("must be a string"), "{error:#}");
    }

    #[test]
    fn chart_version_must_match_helm_semver_rules() {
        let adapter = HelmAdapter;
        let invalid = make_chart("name: test\nversion: not-a-version\n");
        let error = adapter.extract_metadata("test.tgz", &invalid).unwrap_err();
        assert!(
            error.to_string().contains("valid Helm semantic version"),
            "{error:#}"
        );

        for version in ["1", "1.2", "v01.002.0003+build.7"] {
            let chart = make_chart(&format!("name: test\nversion: {version:?}\n"));
            let metadata = adapter.extract_metadata("test.tgz", &chart).unwrap();
            assert_eq!(metadata.version, version);
        }
    }

    #[test]
    fn test_build_helm_index() {
        let entries = vec![HelmIndexEntry {
            name: "nginx".into(),
            version: "1.2.3".into(),
            app_version: Some("1.19.0".into()),
            description: Some("A Helm chart".into()),
            api_version: Some("v2".into()),
            kube_version: None,
            chart_type: None,
            deprecated: false,
            dependencies: Vec::new(),
            home: Some("https://example.com".into()),
            sources: vec!["https://github.com/x/y".into()],
            keywords: vec!["web".into(), "proxy".into()],
            created: "2024-01-01T00:00:00Z".into(),
            digest: Some("sha256:abc123".into()),
            urls: vec!["https://example.com/charts/nginx-1.2.3.tgz".into()],
        }];

        let yaml = build_helm_index(&entries);
        assert!(yaml.contains("apiVersion: v1"));
        assert!(yaml.contains("nginx"));
        assert!(yaml.contains("1.2.3"));
        assert!(yaml.contains("sha256:abc123"));
        assert!(yaml.contains("generated"));
        // A chart that declared neither must not have the keys invented for it:
        // `deprecated: false` in an index entry is an answer, not a silence.
        assert!(!yaml.contains("deprecated"), "{yaml}");
        assert!(!yaml.contains("kubeVersion"), "{yaml}");
    }

    /// `helm install` refuses a chart whose `kubeVersion` the cluster does not
    /// satisfy, and `helm search repo` hides a deprecated one — both decided
    /// from the index by any client that resolves before downloading. A chart
    /// that declared them must carry them through to `index.yaml`.
    #[test]
    fn the_index_republishes_kube_version_deprecation_and_dependencies() {
        let chart = make_chart(
            r#"apiVersion: v2
name: legacy
version: 1.0.0
kubeVersion: ">=1.21.0-0 <1.28.0-0"
type: application
deprecated: true
dependencies:
  - name: postgresql
    version: "12.x.x"
    repository: https://charts.bitnami.com/bitnami
    condition: postgresql.enabled
"#,
        );

        let stored = HelmAdapter
            .extract_metadata("legacy-1.0.0.tgz", &chart)
            .unwrap()
            .protocol_metadata
            .expect("chart declares protocol metadata");
        let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();

        assert_eq!(stored["kubeVersion"], ">=1.21.0-0 <1.28.0-0", "{stored}");
        assert_eq!(stored["type"], "application", "{stored}");
        assert_eq!(stored["deprecated"], true, "{stored}");
        assert_eq!(stored["dependencies"][0]["name"], "postgresql", "{stored}");
        assert_eq!(
            stored["dependencies"][0]["condition"], "postgresql.enabled",
            "the dependency table travels as the chart spells it: {stored}"
        );

        let yaml = build_helm_index(&[HelmIndexEntry {
            name: "legacy".into(),
            version: "1.0.0".into(),
            app_version: None,
            description: None,
            api_version: Some("v2".into()),
            kube_version: stored["kubeVersion"].as_str().map(String::from),
            chart_type: stored["type"].as_str().map(String::from),
            deprecated: stored["deprecated"].as_bool().unwrap_or(false),
            dependencies: stored["dependencies"].as_array().cloned().unwrap(),
            home: None,
            sources: Vec::new(),
            keywords: Vec::new(),
            created: "2024-01-01T00:00:00Z".into(),
            digest: None,
            urls: vec!["https://example.com/charts/legacy-1.0.0.tgz".into()],
        }]);

        assert!(yaml.contains("kubeVersion:"), "{yaml}");
        assert!(yaml.contains("deprecated: true"), "{yaml}");
        assert!(yaml.contains("postgresql"), "{yaml}");
        assert!(yaml.contains("condition: postgresql.enabled"), "{yaml}");
    }
}
