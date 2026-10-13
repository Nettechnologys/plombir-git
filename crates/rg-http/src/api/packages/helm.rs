//! Helm chart repository index and chart upload.

use super::*;

// ── Helm Protocol Endpoints ───────────────────────────────

/// The chart archive of a version — the file `urls` points at and `digest`
/// makes its promise about, as opposed to anything stored beside it.
pub(crate) fn chart_file(
    version: &rg_core::package_registry::VersionDetail,
) -> Option<&rg_core::package_registry::FileDetail> {
    version
        .files
        .iter()
        .find(|f| f.filename.ends_with(".tgz"))
        .or_else(|| version.files.first())
}

/// GET /api/v1/repos/{owner}/{name}/packages/helm/index.yaml
///
/// Helm repository index — returns the index.yaml that `helm repo add` expects.
pub async fn helm_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };

    // List all helm packages in the repo
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, "helm")
            .await
        {
            Ok(p) => p,
            Err(e) => return AppError::from(e).into_response(),
        };

    let mut entries: Vec<rg_core::package_registry::HelmIndexEntry> = Vec::new();

    for pkg in &packages {
        // Get all versions for this chart
        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "helm", &pkg.name,
        )
        .await
        {
            Ok(v) => v,
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        for v in &versions {
            if !v.is_install_candidate() {
                continue;
            }

            // Build download URL
            let chart = chart_file(v);
            let filename = chart
                .map(|f| f.filename.clone())
                .unwrap_or_else(|| format!("{}-{}.tgz", pkg.name, v.version));

            let download_url = format!(
                "{}/api/v1/repos/{}/{}/packages/helm/{}/{}/{}",
                base_url.trim_end_matches('/'),
                encode_path_segment(&owner),
                encode_path_segment(&name),
                encode_path_segment(&pkg.name),
                encode_path_segment(&v.version),
                encode_path_segment(&filename),
            );

            // Parse Helm-specific metadata from version JSON
            let meta = match parse_helm_metadata(v.metadata.as_deref(), &pkg.name, &v.version) {
                Ok(meta) => meta,
                Err(error) => return package_error_response(error),
            };

            entries.push(rg_core::package_registry::HelmIndexEntry {
                name: pkg.name.clone(),
                version: v.version.clone(),
                app_version: meta.app_version,
                description: pkg.description.clone(),
                api_version: meta.api_version,
                kube_version: meta.kube_version,
                chart_type: meta.chart_type,
                deprecated: meta.deprecated,
                dependencies: meta.dependencies,
                home: pkg.homepage.clone(),
                sources: meta.sources,
                keywords: meta.keywords,
                created: v.created_at.clone(),
                // `digest` is defined as the SHA-256 of the archive `urls`
                // points at, and `helm` verifies exactly that.
                digest: chart.and_then(|f| v.sha256_of(f)),
                urls: vec![download_url],
            });
        }
    }

    let yaml = rg_core::package_registry::build_helm_index(&entries);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/x-yaml; charset=utf-8"),
            // Some Helm clients also check for text/yaml
        ],
        yaml,
    )
        .into_response()
}

/// The Chart.yaml fields `index.yaml` republishes, read back out of the stored
/// version metadata.
#[derive(Default)]
pub(crate) struct HelmChartMetadata {
    pub(crate) app_version: Option<String>,
    pub(crate) api_version: Option<String>,
    pub(crate) kube_version: Option<String>,
    pub(crate) chart_type: Option<String>,
    pub(crate) deprecated: bool,
    pub(crate) dependencies: Vec<serde_json::Value>,
    pub(crate) keywords: Vec<String>,
    pub(crate) sources: Vec<String>,
}

/// Parse Helm-specific metadata from version metadata JSON.
pub(crate) fn parse_helm_metadata(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<HelmChartMetadata> {
    let Some(doc) = parse_stored_metadata_document("helm", package_name, version, metadata_json)?
    else {
        return Ok(HelmChartMetadata::default());
    };

    // card_33f83c325515: `parse_stored_metadata_document` already refuses a
    // stored blob that is not a JSON object, but inside it every field used to
    // fall back to a default — so a `dependencies` that is not an array served
    // `index.yaml` with `200` and no subcharts, and a `deprecated` that is not
    // a boolean served the chart as current. `chart_protocol_metadata` writes
    // each of these with one shape and refuses anything it could not carry, so
    // a value of another shape here is this registry's own row having rotted,
    // not something a `Chart.yaml` could have said. Untyped on purpose: that
    // reaches the client as a 5xx, because the request was fine.
    let damaged = |key: &str, must_be: &str| {
        anyhow::anyhow!(
            "stored helm metadata for package '{package_name}' version '{version}' is \
             damaged: `{key}` is not {must_be}"
        )
    };
    // An absent key and an explicit null are the same claim — a chart that
    // declared none of this — and neither is damage.
    let declared = |key: &str| doc.get(key).filter(|value| !value.is_null());

    let string_list = |key: &str| -> anyhow::Result<Vec<String>> {
        let Some(value) = declared(key) else {
            return Ok(Vec::new());
        };
        let array = value.as_array().ok_or_else(|| damaged(key, "an array"))?;
        array
            .iter()
            .enumerate()
            .map(|(position, entry)| {
                entry
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| damaged(&format!("{key}[{position}]"), "a string"))
            })
            .collect()
    };

    let string_field = |key: &str| -> anyhow::Result<Option<String>> {
        declared(key)
            .map(|value| {
                value
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| damaged(key, "a string"))
            })
            .transpose()
    };

    Ok(HelmChartMetadata {
        app_version: string_field("appVersion")?,
        api_version: string_field("apiVersion")?,
        kube_version: string_field("kubeVersion")?,
        chart_type: string_field("type")?,
        deprecated: match declared("deprecated") {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| damaged("deprecated", "a boolean"))?,
            None => false,
        },
        dependencies: match declared("dependencies") {
            Some(value) => value
                .as_array()
                .ok_or_else(|| damaged("dependencies", "an array"))?
                .clone(),
            None => Vec::new(),
        },
        keywords: string_list("keywords")?,
        sources: string_list("sources")?,
    })
}
