//! NuGet v3 service index, registration, flat container and push.

use super::*;

pub(crate) async fn decode_nuget_push(
    mut multipart: Multipart,
    repo_root: &FsPath,
    max_bytes: usize,
) -> Result<PackageArtifact, AppError> {
    let mut package = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| package_multipart_error(error, "invalid NuGet multipart body"))?
    {
        if field.name() != Some("package") {
            continue;
        }
        if package.is_some() {
            return Err(AppError::bad_request(
                "NuGet upload repeats the `package` field",
            ));
        }
        package = Some(
            stage_multipart_artifact(
                field,
                repo_root,
                max_bytes,
                "cannot read NuGet `package` field",
            )
            .await?,
        );
    }

    package.ok_or_else(|| AppError::bad_request("NuGet upload is missing `package`"))
}

/// POST/PUT /api/v1/repos/{owner}/{name}/packages/nuget/publish
///
/// `dotnet nuget push` sends PUT to the advertised `PackagePublish` resource,
/// with the nupkg in a multipart field named `package`. Its part filename is
/// always the generic `package.nupkg`, not the artifact's own name. Keep raw
/// bodies too: Plombir Git exposed that contract before the native-client route
/// existed, and the explicit POST route still serves those publishers.
pub async fn nuget_publish(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<PublishPackageQuery>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let headers = request.headers().clone();
    let is_multipart = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("multipart/form-data"));

    let (filename, body) = if is_multipart {
        let multipart = match Multipart::from_request(request, &state).await {
            Ok(multipart) => multipart,
            Err(rejection) => return rejection.into_response(),
        };
        let body =
            match decode_nuget_push(multipart, &state.repo_root, state.package_upload_max_bytes)
                .await
            {
                Ok(body) => body,
                Err(error) => return error.into_response(),
            };
        // NuGet.Client deliberately sends a random filename; using the same
        // stable spelling also makes a repeated file hit the uniqueness guard.
        ("package.nupkg".to_string(), body)
    } else {
        let filename = filename_from_disposition(&headers);
        let body = match stage_request_artifact(
            &state,
            request.into_body(),
            state.package_upload_max_bytes,
        )
        .await
        {
            Ok(body) => body,
            Err(response) => return response,
        };
        (filename, body)
    };

    publish_package(
        state,
        user_id,
        &headers,
        owner,
        name,
        "nuget".to_string(),
        query,
        filename,
        body,
    )
    .await
}

// ── NuGet Protocol Endpoints ──────────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/index.json
///
/// NuGet Service Index (v3) — returns the list of available API resources.
pub async fn nuget_service_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };
    let json = rg_core::package_registry::build_service_index(&base_url, &owner, &name);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/index.json
///
/// NuGet Registration Index — returns the metadata for all versions of a package.
pub async fn nuget_registration_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let pkg_name = match resolve_nuget_id(&state, &owner, &name, &pkg_name).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
    };

    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };
    let registration_url = nuget_registration_url(&base_url, &owner, &name, &pkg_name);
    let entries = versions
        .iter()
        .map(|version| nuget_registration_entry(&base_url, &owner, &name, &pkg_name, version))
        .collect::<anyhow::Result<Vec<_>>>();
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) => return package_error_response(error),
    };

    let json =
        rg_core::package_registry::build_registration_index(&pkg_name, &registration_url, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET/HEAD /api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}
///
/// A registration index advertises this document through every inline leaf's
/// `@id`. The URL uses NuGet's normalized version spelling, while historical
/// rows are resolved with the same equality rules as the flat container.
pub async fn nuget_registration_leaf(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name, version)): Path<(String, String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let pkg_name = match resolve_nuget_id(&state, &owner, &name, &pkg_name).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(versions) => versions,
        Err(error) => return package_error_response(error),
    };
    let Some(found) = versions
        .iter()
        .find(|stored| rg_core::package_registry::nuget_versions_match(&stored.version, &version))
    else {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!("package '{pkg_name}' has no version '{version}'"),
        );
    };

    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };
    let registration_url = nuget_registration_url(&base_url, &owner, &name, &pkg_name);
    let entry = match nuget_registration_entry(&base_url, &owner, &name, &pkg_name, found) {
        Ok(entry) => entry,
        Err(error) => return package_error_response(error),
    };
    let json =
        rg_core::package_registry::build_registration_leaf(&pkg_name, &registration_url, &entry);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

pub(crate) fn nuget_registration_url(
    base_url: &str,
    owner: &str,
    repo: &str,
    package_name: &str,
) -> String {
    format!(
        "{}/api/v1/repos/{}/{}/packages/nuget/registration/{}/index.json",
        base_url.trim_end_matches('/'),
        encode_path_segment(owner),
        encode_path_segment(repo),
        encode_path_segment(&package_name.to_lowercase()),
    )
}

pub(crate) fn nuget_registration_entry(
    base_url: &str,
    owner: &str,
    repo: &str,
    package_name: &str,
    version: &rg_core::package_registry::VersionDetail,
) -> anyhow::Result<rg_core::package_registry::NuGetRegistrationEntry> {
    let metadata =
        parse_nuget_metadata(version.metadata.as_deref(), package_name, &version.version)?;
    let filename = version
        .files
        .iter()
        .find(|file| file.filename.to_lowercase().ends_with(".nupkg"))
        .map(|file| file.filename.clone())
        .unwrap_or_else(|| format!("{}.{}.nupkg", package_name, version.version));
    let download_url = format!(
        "{}/api/v1/repos/{}/{}/packages/nuget/{}/{}/{}",
        base_url.trim_end_matches('/'),
        encode_path_segment(owner),
        encode_path_segment(repo),
        encode_path_segment(package_name),
        encode_path_segment(&version.version),
        encode_path_segment(&filename),
    );

    Ok(rg_core::package_registry::NuGetRegistrationEntry {
        version: version.version.clone(),
        description: metadata.description,
        homepage: metadata.homepage,
        license: metadata.license,
        tags: metadata.tags,
        download_url,
        dependency_groups: metadata.dependency_groups,
        // A yanked version stays in the registration so a consumer that
        // already resolved it keeps restoring; `listed` is what keeps it out
        // of a fresh resolution.
        listed: version.is_install_candidate(),
    })
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/query?q=...
///
/// NuGet Search Query API (3.5.0) — search packages by name.
pub async fn nuget_search(
    State(state): State<AppState>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<NuGetSearchParams>,
) -> axum::response::Response {
    let query = params.q.as_deref().unwrap_or("");
    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };

    // List all nuget packages in the repo. A repository that never enabled the
    // registry answers an empty result set rather than a 404: the service index
    // advertises this endpoint for any repository, so it owes a readable answer.
    let packages = match nuget_search_packages(
        &state,
        &owner,
        &name,
        params.prerelease,
        params.semver_level.as_deref(),
    )
    .await
    {
        Ok(packages) => packages,
        Err(response) => return response,
    };

    let mut results: Vec<rg_core::package_registry::NuGetSearchResult> = Vec::new();
    let query_lower = query.to_lowercase();

    for pkg in &packages {
        let name_lower = pkg.summary.name.to_lowercase();
        // Simple substring match
        if query.is_empty() || name_lower.contains(&query_lower) {
            let Some(version) = pkg.summary.latest_version.clone() else {
                continue;
            };
            let registration_url =
                nuget_registration_url(&base_url, &owner, &name, &pkg.summary.name);

            results.push(rg_core::package_registry::NuGetSearchResult {
                name: pkg.summary.name.clone(),
                version,
                versions: pkg.versions.clone(),
                description: pkg.summary.description.clone(),
                tags: pkg.summary.keywords.clone(),
                registration_url,
            });
        }
    }

    // `totalHits` counts the whole match set; `data` carries the one window the
    // client paged to. Both are cut from this same filtered sequence, which is
    // ordered by package name — a total order, so walking the pages visits every
    // hit exactly once.
    let total_hits = results.len();
    let page = rg_core::package_registry::nuget_page(results, params.skip, params.take);
    let json = rg_core::package_registry::build_search_results(&page, total_hits);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// Resolve the package id a NuGet client asked for to the id it was published
/// under.
///
/// Ids are case-insensitive and every v3 URL carries the lowercase form, so
/// `dotnet restore` of `Matrix.NuGet` asks for `matrix.nuget` while the registry
/// stores what the nuspec said. Matching the path segment literally therefore
/// missed every id with a capital in it — the same defect PyPI already fixed
/// with [`resolve_pypi_project`] (card_dba77cceec56).
///
/// One query, and an exact spelling still wins over a case-folded one, so a
/// registry that somehow holds two ids differing only in case keeps answering
/// for the one that was actually asked for. An unresolvable id is handed back
/// unchanged: producing the 404 is the caller's job, not this helper's.
pub(crate) async fn resolve_nuget_id(
    state: &AppState,
    owner: &str,
    repo: &str,
    requested: &str,
) -> Result<String, axum::response::Response> {
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, owner, repo, "nuget")
            .await
        {
            Ok(packages) => packages,
            Err(error) if package_is_absent(&error) => return Ok(requested.to_string()),
            Err(error) => return Err(package_error_response(error)),
        };

    if packages.iter().any(|pkg| pkg.name == requested) {
        return Ok(requested.to_string());
    }

    let wanted = rg_core::package_registry::normalize_package_id(requested);
    Ok(packages
        .into_iter()
        .find(|pkg| rg_core::package_registry::normalize_package_id(&pkg.name) == wanted)
        .map_or_else(|| requested.to_string(), |pkg| pkg.name))
}

pub(crate) async fn nuget_search_packages(
    state: &AppState,
    owner: &str,
    repo: &str,
    include_prerelease: bool,
    semver_level: Option<&str>,
) -> Result<Vec<rg_core::package_registry::NuGetSearchPackage>, axum::response::Response> {
    match rg_core::package_registry::service::list_nuget_search_packages(
        &state.db,
        owner,
        repo,
        include_prerelease,
        semver_level,
    )
    .await
    {
        Ok(packages) => Ok(packages),
        Err(error) if package_is_absent(&error) => Ok(Vec::new()),
        Err(error) => Err(package_error_response(error)),
    }
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/package/{id}/index.json
///
/// NuGet flat container (`PackageBaseAddress/3.0.0`) — the versions of one
/// package. This is the first hop of `dotnet restore`, and it had no route at
/// all while the service index advertised the resource, so restore could not
/// download anything (card_dba77cceec56).
pub async fn nuget_flat_container_index(
    State(state): State<AppState>,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let pkg_name = match resolve_nuget_id(&state, &owner, &name, &pkg_name).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(versions) => versions,
        Err(error) => return package_error_response(error),
    };

    let listed: Vec<String> = versions.into_iter().map(|v| v.version).collect();
    let json = rg_core::package_registry::build_flat_container_index(&listed);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/package/{id}/{version}/{file}
///
/// NuGet flat container package content — the `.nupkg` itself.
///
/// The client builds this URL out of *normalized* parts (`{id-lower}` and
/// `{id-lower}.{version}.nupkg`), which is almost never the filename the package
/// was published under, so the stored file is matched case-insensitively and
/// then by extension rather than by the client's spelling.
pub async fn nuget_flat_container_download(
    State(state): State<AppState>,
    Path((owner, name, pkg_name, version, filename)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let pkg_name = match resolve_nuget_id(&state, &owner, &name, &pkg_name).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(versions) => versions,
        Err(error) => return package_error_response(error),
    };

    let Some(found) = versions
        .iter()
        .find(|v| rg_core::package_registry::nuget_versions_match(&v.version, &version))
    else {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!("package '{pkg_name}' has no version '{version}'"),
        );
    };

    let wanted_file = filename.to_lowercase();
    let Some(file) = found
        .files
        .iter()
        .find(|f| f.filename.to_lowercase() == wanted_file)
        .or_else(|| {
            found
                .files
                .iter()
                .find(|f| f.filename.to_lowercase().ends_with(".nupkg"))
        })
    else {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!("no package content stored for '{pkg_name}' {version}"),
        );
    };

    serve_package_file(
        &state,
        &owner,
        &name,
        "nuget",
        &pkg_name,
        &found.version,
        &file.filename,
    )
    .await
}

/// The `?id=` form of autocomplete: every version of one package, under the
/// same capability filter that builds `versions` in SearchQueryService.
///
/// An id nobody published answers an empty list rather than a 404 — this
/// endpoint feeds a picker while the user is still typing, so absence is a
/// normal state of the request, not a diagnosis.
pub(crate) async fn nuget_autocomplete_versions(
    state: &AppState,
    owner: &str,
    repo: &str,
    requested: &str,
    params: &NuGetSearchParams,
) -> Result<Vec<String>, axum::response::Response> {
    let resolved = resolve_nuget_id(state, owner, repo, requested).await?;
    let packages = nuget_search_packages(
        state,
        owner,
        repo,
        params.prerelease,
        params.semver_level.as_deref(),
    )
    .await?;

    Ok(packages
        .into_iter()
        .find(|pkg| pkg.summary.name == resolved)
        .map(|pkg| {
            pkg.versions
                .into_iter()
                .map(|version| version.version)
                .collect()
        })
        .unwrap_or_default())
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/autocomplete?q=... | ?id=...
///
/// NuGet `SearchAutocompleteService/3.5.0`. The service index used to advertise
/// this resource at the bare `nuget/` root, which is not an endpoint of anything
/// (card_dba77cceec56).
///
/// One URL, two questions: `?q=` completes package ids, `?id=` completes the
/// versions of one package. Serving only the first answered a version picker
/// with the repository's package ids — a 200 of the right *shape*, which is
/// worse than a 404, because the ids land in the client's version dropdown
/// (card_11227138d44e).
pub async fn nuget_autocomplete(
    State(state): State<AppState>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<NuGetSearchParams>,
) -> axum::response::Response {
    // `id` wins over `q` when a client sends both: the version enumeration is
    // the more specific request, and the two answers are not interchangeable.
    let data = match params.id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => match nuget_autocomplete_versions(&state, &owner, &name, id, &params).await {
            Ok(versions) => versions,
            Err(response) => return response,
        },
        None => {
            let query = params.q.as_deref().unwrap_or("").to_lowercase();
            let packages = match nuget_search_packages(
                &state,
                &owner,
                &name,
                params.prerelease,
                params.semver_level.as_deref(),
            )
            .await
            {
                Ok(packages) => packages,
                Err(response) => return response,
            };

            packages
                .into_iter()
                .filter(|pkg| query.is_empty() || pkg.summary.name.to_lowercase().contains(&query))
                .map(|pkg| pkg.summary.name)
                .collect()
        }
    };

    // One paging step for both request forms: whichever question was asked, the
    // count describes the whole answer and `data` carries the window of it the
    // client paged to. Ids arrive ordered by name and versions in version order,
    // so either sequence can be walked page by page without repeats.
    let total_hits = data.len();
    let page = rg_core::package_registry::nuget_page(data, params.skip, params.take);
    let json = rg_core::package_registry::build_autocomplete_results(&page, total_hits);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// Parse NuGet-specific metadata from a JSON metadata string.
pub(crate) fn parse_nuget_metadata(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<NuGetVersionMetadata> {
    let Some(doc) = parse_stored_metadata_document("nuget", package_name, version, metadata_json)?
    else {
        return Ok(NuGetVersionMetadata::default());
    };

    let description = doc
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let homepage = doc
        .get("projectUrl")
        .and_then(|v| v.as_str())
        .or_else(|| doc.get("homepage").and_then(|v| v.as_str()))
        .map(String::from);
    let license = doc
        .get("licenseUrl")
        .and_then(|v| v.as_str())
        .or_else(|| doc.get("license").and_then(|v| v.as_str()))
        .map(String::from);
    let tags = doc.get("tags").and_then(|v| v.as_str()).map(String::from);

    Ok(NuGetVersionMetadata {
        description,
        homepage,
        license,
        tags,
        // Read through rg-core, which also writes these keys and classifies the
        // same graph for the search SemVer-level filter. A second reader here
        // would be free to drift from both — including about what counts as a
        // damaged element, which is why the refusal comes from there too.
        dependency_groups: rg_core::package_registry::stored_dependency_groups(
            package_name,
            version,
            &doc,
        )?,
    })
}
