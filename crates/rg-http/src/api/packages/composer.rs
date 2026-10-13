//! Composer packages.json.

use super::*;

// ── Composer Protocol Endpoint ────────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/composer/packages.json
///
/// Returns a Composer repository `packages.json` compatible with
/// Composer 2.x SAT solver.
pub async fn composer_packages_json(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };

    let packages = match rg_core::package_registry::service::list_packages(
        &state.db, &owner, &name, "composer",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => return package_error_response(e),
    };

    let mut json_output = String::new();
    let mut first = true;

    // Start building the combined JSON manually
    json_output.push_str("{\"packages\":{");

    for pkg in &packages {
        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "composer", &pkg.name,
        )
        .await
        {
            Ok(v) => v,
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        let composer_versions: Vec<
            rg_core::package_registry::adapters::composer::ComposerVersionInfo,
        > = versions
            .iter()
            // Composer's `abandoned` flag is package-wide, not a version yank.
            // A withdrawn version therefore has to disappear from this map.
            .filter(|v| v.is_install_candidate())
            .map(|v| {
                let archive = v.files.first();
                let filename = archive
                    .map(|f| f.filename.clone())
                    .unwrap_or_else(|| format!("{}.zip", v.version));
                rg_core::package_registry::adapters::composer::ComposerVersionInfo {
                    version: v.version.clone(),
                    filename,
                    // Digests of the archive the `dist` block points at, not of
                    // whatever the version recorded first.
                    sha256: archive.and_then(|f| v.sha256_of(f)),
                    sha1: archive.and_then(|f| f.sha1.clone()),
                    description: pkg.description.clone(),
                    // `license` and `type` come out of the manifest sections the
                    // adapter lifted at publish, below — there is no license or
                    // type column on a version row to read them from, and the
                    // literal `None` that used to sit here is what made every
                    // package announce itself as a `library`.
                    license: None,
                    package_type: None,
                    metadata: v.metadata.clone(),
                }
            })
            .collect();
        let name_json = serde_json::json!(pkg.name).to_string();
        // Same as the npm packument and the cargo index: an unreadable row must
        // not be served as an entry saying the package requires nothing.
        let versions_json = match rg_core::package_registry::adapters::composer::build_packages_json(
            &pkg.name,
            &composer_versions,
            &base_url,
            &owner,
            &name,
        ) {
            Ok(versions_json) => versions_json,
            Err(error) => return package_error_response(error),
        };
        // Extract just the inner version map from the full response
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&versions_json) {
            if let Some(pkgs) = val.get("packages") {
                if let Some(inner) = pkgs.get(&pkg.name) {
                    if !first {
                        json_output.push(',');
                    }
                    first = false;
                    json_output.push_str(&format!("{}:{}", name_json, inner));
                }
            }
        }
    }

    json_output.push_str("}}");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json_output,
    )
        .into_response()
}
