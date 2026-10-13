//! RubyGems compact index, push and `.gem` download.

use super::*;

// ── RubyGems Protocol Endpoints ───────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/dependencies.json?gems={name}
///
/// RubyGems dependencies JSON API. The extensionless endpoint uses Ruby
/// Marshal; Plombir Git leaves it unregistered rather than returning JSON bytes
/// that legacy Bundler will try to pass to `Marshal.load`.
pub async fn rubygems_dependencies_json(
    State(state): State<AppState>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<RubyGemsDepsParams>,
) -> axum::response::Response {
    let gem_list: Vec<&str> = params
        .gems
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .collect();

    let mut entries: Vec<rg_core::package_registry::RubyGemsDependencyEntry> = Vec::new();

    for gem_name in gem_list {
        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "rubygems", gem_name,
        )
        .await
        {
            Ok(v) => v,
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        for v in &versions {
            // A withdrawn version is not a candidate. The compact index has
            // always dropped them; this endpoint is the other half of the same
            // resolution and used to offer them (card_0c9e858230b6).
            if !v.is_install_candidate() {
                continue;
            }

            // Parse dependencies from metadata JSON
            let deps = match parse_rubygems_deps(v.metadata.as_deref(), gem_name, &v.version) {
                Ok(deps) => deps,
                Err(error) => return package_error_response(error),
            };

            entries.push(rg_core::package_registry::RubyGemsDependencyEntry {
                name: gem_name.to_string(),
                number: v.version.clone(),
                // Bundler keys a candidate on `(number, platform)` and picks
                // between a pure-ruby gem and a native build with it. A flat
                // `ruby` for every gem locks a native build as
                // platform-independent, and the URL Bundler derives from the
                // pair does not exist.
                platform: gem_declared_platform(v, gem_name),
                dependencies: deps,
            });
        }
    }

    // Return empty array instead of null for unknown gems
    let json = rg_core::package_registry::build_dependencies_json(&entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems/{gem_name}.json
///
/// RubyGems gem info API — returns detailed metadata for all versions.
pub async fn rubygems_gem_info(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, gem_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let gem_name = gem_name
        .strip_suffix(".json")
        .unwrap_or(&gem_name)
        .to_string();

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "rubygems", &gem_name,
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
    let root = rubygems_root(&base_url, &owner, &name);

    let version_info = versions
        .iter()
        .filter(|v| v.is_install_candidate())
        .map(|v| parse_rubygems_info(v.metadata.as_deref(), &gem_name, &v.version))
        .collect::<anyhow::Result<Vec<_>>>();
    let version_info = match version_info {
        Ok(info) => info,
        Err(error) => return package_error_response(error),
    };
    let entries: Vec<rg_core::package_registry::RubyGemsVersionEntry> = versions
        .iter()
        // A withdrawn version is not on offer here either — see
        // `rubygems_dependencies_json`, which resolves against the same rows.
        .filter(|v| v.is_install_candidate())
        .zip(version_info)
        .map(|(v, info)| {
            // The name the file was published under, not one rebuilt from the
            // coordinates: a platform gem is stored as `{name}-{ver}-{platform}.gem`.
            let file = gem_file(v);
            let filename = file
                .map(|f| f.filename.clone())
                .unwrap_or_else(|| format!("{}-{}.gem", gem_name, v.version));
            let download_url = format!("{root}/{gem_name}/{}/{filename}", v.version);
            // `gems/{file}` under the registry root, because that is the only
            // path a client will ever ask for: `Gem::RemoteFetcher#download`
            // glues it onto the source URL itself. Advertising anything else
            // here is advertising a URL nothing serves.
            let gem_uri = format!("{root}/gems/{filename}");

            rg_core::package_registry::RubyGemsVersionEntry {
                number: v.version.clone(),
                platform: gem_declared_platform(v, &gem_name),
                summary: info.summary,
                description: info.description,
                homepage: info.homepage,
                license: info.license,
                // The digest of the `.gem` the two URLs above point at — not
                // the version's, which is a different file as soon as the
                // version carries more than one.
                sha256: file.and_then(|f| v.sha256_of(f)),
                download_url,
                gem_uri,
                created_at: v.created_at.clone(),
            }
        })
        .collect();

    let json = rg_core::package_registry::build_gem_info_json(&gem_name, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct RubyGemsDepsParams {
    #[serde(default)]
    pub gems: Option<String>,
}

// ── RubyGems compact index ────────────────────────────────

/// The registry root a client is pointed at — `gem install --source <this>`,
/// or a `Gemfile`'s `source`. Every compact-index path hangs off it, and so
/// does the `gems/{file}` download the client builds on its own.
pub(crate) fn rubygems_root(base_url: &str, owner: &str, repo: &str) -> String {
    format!(
        "{}/api/v1/repos/{}/{}/packages/rubygems",
        base_url.trim_end_matches('/'),
        owner,
        repo,
    )
}

/// The `.gem` of a version — the file a client downloads, as opposed to any
/// checksum or signature published beside it.
pub(crate) fn gem_file(
    version: &rg_core::package_registry::VersionDetail,
) -> Option<&rg_core::package_registry::FileDetail> {
    version
        .files
        .iter()
        .find(|f| f.filename.ends_with(".gem"))
        .or_else(|| version.files.first())
}

/// The gemspec facts every RubyGems endpoint needs beside the dependency list.
pub(crate) struct RubyGemsFacts {
    /// The declared platform, and only when it is not the default `ruby`.
    pub(crate) platform: Option<String>,
    pub(crate) ruby_version: Option<String>,
    pub(crate) rubygems_version: Option<String>,
}

/// Read them back out of a version's stored gemspec metadata.
///
/// `platform` is absent for a pure-ruby gem *and* for anything published before
/// the adapter recorded it; [`gem_platform`] is the fallback for the second
/// case, and it cannot tell the two apart — which is the whole reason the
/// declared value is now stored.
pub(crate) fn parse_rubygems_facts(metadata_json: Option<&str>) -> RubyGemsFacts {
    let field = |doc: &serde_json::Value, key: &str| {
        doc.get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(String::from)
    };

    match metadata_json.and_then(|md| serde_json::from_str::<serde_json::Value>(md).ok()) {
        Some(doc) => RubyGemsFacts {
            platform: field(&doc, "platform"),
            ruby_version: field(&doc, "required_ruby_version"),
            rubygems_version: field(&doc, "required_rubygems_version"),
        },
        None => RubyGemsFacts {
            platform: None,
            ruby_version: None,
            rubygems_version: None,
        },
    }
}

/// The platform a version declares, spelled the way the two JSON RubyGems APIs
/// publish it: `ruby` for a pure-ruby gem, and the real one otherwise.
pub(crate) fn gem_declared_platform(
    version: &rg_core::package_registry::VersionDetail,
    gem_name: &str,
) -> String {
    parse_rubygems_facts(version.metadata.as_deref())
        .platform
        .or_else(|| {
            gem_file(version).and_then(|f| gem_platform(&f.filename, gem_name, &version.version))
        })
        .unwrap_or_else(|| "ruby".to_string())
}

/// The platform a stored `.gem` carries, when it is not the default `ruby`.
///
/// The fallback for a version published before the gemspec's own `platform` was
/// recorded: the client encodes it in the file name it published
/// (`nokogiri-1.16.0-x86_64-linux.gem`), and the compact index has to spell the
/// same `VERSION-PLATFORM` chunk back or the download the client derives from it
/// will not exist.
pub(crate) fn gem_platform(filename: &str, gem_name: &str, version: &str) -> Option<String> {
    let stem = filename.strip_suffix(".gem")?;
    let rest = stem.strip_prefix(&format!("{gem_name}-{version}"))?;
    let platform = rest.strip_prefix('-')?;

    (!platform.is_empty() && platform != "ruby").then(|| platform.to_string())
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/versions
///
/// The compact index's entry point, and the request that decides which protocol
/// the client speaks for the rest of the session: `Gem::Source` asks for this
/// file first, resolves through `info/{gem}` when it is served, and falls back
/// to the legacy Marshal index (which Plombir Git does not serve) when it is not.
///
/// A repository with no gems answers an empty index rather than a 404, for that
/// reason: the 404 would not read as "nothing published yet", it would push the
/// client onto a protocol that then fails on its own missing files.
pub async fn rubygems_compact_versions(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let mut gems = Vec::new();
    let mut created_at = String::new();

    let packages = match rubygems_packages(&state, &owner, &name).await {
        Ok(packages) => packages,
        Err(error) => return package_error_response(error),
    };

    for pkg in packages {
        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "rubygems", &pkg.name,
        )
        .await
        {
            Ok(v) => v,
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        let entries = match compact_index_entries(&pkg.name, &versions) {
            Ok(entries) => entries,
            Err(error) => return package_error_response(error),
        };
        if entries.is_empty() {
            continue;
        }

        if let Some(newest) = versions.iter().map(|v| &v.created_at).max() {
            if *newest > created_at {
                created_at.clone_from(newest);
            }
        }

        // The checksum has to be of the body this server will actually serve at
        // `info/{gem}`, so the info file is built here and hashed, not guessed.
        let info = rg_core::package_registry::build_compact_index_info(&entries);
        gems.push(rg_core::package_registry::CompactIndexGem {
            name: pkg.name.clone(),
            versions: entries.iter().map(|e| e.version_and_platform()).collect(),
            info_checksum: rg_core::package_registry::compact_index_info_checksum(&info),
        });
    }

    gems.sort_by(|a, b| a.name.cmp(&b.name));

    text_index(rg_core::package_registry::build_compact_index_versions(
        &created_at,
        &gems,
    ))
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/info/{gem_name}
///
/// One gem's versions, dependencies and checksums — what the client resolves
/// against once `versions` has told it this registry speaks the compact index.
pub async fn rubygems_compact_info(
    State(state): State<AppState>,
    Path((owner, name, gem_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "rubygems", &gem_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
    };

    let entries = match compact_index_entries(&gem_name, &versions) {
        Ok(entries) => entries,
        Err(error) => return package_error_response(error),
    };
    if entries.is_empty() {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!("gem '{gem_name}' has no installable version"),
        );
    }

    text_index(rg_core::package_registry::build_compact_index_info(
        &entries,
    ))
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/names
///
/// Every gem name in the registry. No official RubyGems tool reads it, but it
/// is part of the index a mirroring client expects to find beside the other
/// two, and it costs one query.
pub async fn rubygems_compact_names(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let packages = match rubygems_packages(&state, &owner, &name).await {
        Ok(packages) => packages,
        Err(error) => return package_error_response(error),
    };
    let mut names: Vec<String> = packages.into_iter().map(|pkg| pkg.name).collect();
    names.sort();

    text_index(rg_core::package_registry::build_compact_index_names(&names))
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/gems/{filename}
///
/// The `.gem` download. The client does not read this path out of any index —
/// `Gem::RemoteFetcher#download` appends `gems/{file}` to the source URL — so
/// it is fixed, and the file is found by the name it was published under rather
/// than by splitting `{name}-{version}` back out of it (both halves may contain
/// dashes, and a platform gem carries a third).
pub async fn rubygems_gem_download(
    State(state): State<AppState>,
    Path((owner, name, filename)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let packages = match rubygems_packages(&state, &owner, &name).await {
        Ok(packages) => packages,
        Err(error) => return package_error_response(error),
    };

    for pkg in packages {
        // Every gem file starts with its gem's name, so most candidates are
        // ruled out without a query.
        if !filename.starts_with(&format!("{}-", pkg.name)) {
            continue;
        }

        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "rubygems", &pkg.name,
        )
        .await
        {
            Ok(v) => v,
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        if let Some(version) = versions
            .iter()
            .find(|v| v.files.iter().any(|f| f.filename == filename))
        {
            return serve_package_file(
                &state,
                &owner,
                &name,
                "rubygems",
                &pkg.name,
                &version.version,
                &filename,
            )
            .await;
        }
    }

    err_text(
        StatusCode::NOT_FOUND,
        &format!("gem '{filename}' not found"),
    )
}

// ── RubyGems write API ────────────────────────────────────
//
// The read side was completed long before this existed (card_01ee27252197), so
// a gem could be resolved and installed from Plombir Git but never put there by
// the tool that builds it (card_11a578ae1820).

/// POST /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems
///
/// What `gem push <file> --host <source>` sends: the `.gem` as the entire body,
/// no `Content-Disposition` naming it, and the API key out of
/// `~/.gem/credentials` in a scheme-less `Authorization` header — the spelling
/// the PAT middleware learned for cargo (card_5a790cc6ac35), which is why this
/// route can sit behind the ordinary [`RepoWrite`] gate.
///
/// The stored filename is therefore ours to derive, and it is not cosmetic:
/// `Gem::RemoteFetcher#download` asks for `gems/{name}-{version}.gem`, and
/// [`rubygems_gem_download`] resolves a file by the name it was published
/// under. Routing this through the generic publish handler instead would have
/// stored the gem as `package` — the `Content-Disposition` fallback — leaving
/// it undownloadable by the very client that pushed it.
///
/// The answer is `text/plain` because `gem` prints the body of a 2xx verbatim
/// as the server's word on the push; the generic publish envelope would reach
/// the user as a line of JSON. A failure is passed through as it came, since
/// the client prints that body just as literally and the shared classifier's
/// message is the informative part.
pub async fn rubygems_push(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    body: Body,
) -> axum::response::Response {
    let body = match stage_request_artifact(&state, body, state.package_upload_max_bytes).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Some(adapter) = rg_core::package_registry::get_adapter("rubygems") else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the rubygems adapter is not registered",
        );
    };

    // Read the gemspec here rather than letting `publish_package` do it,
    // because the name and version are what the filename is built from and the
    // filename has to be settled before the upload is described at all.
    let (body, inspection) = match body
        .inspect_with_adapter(adapter, "package.gem".to_string())
        .await
    {
        Ok(result) => result,
        Err(error) => {
            return AppError::internal(format!(
                "RubyGems package inspection task did not complete: {error}"
            ))
            .into_response()
        }
    };
    let meta = match inspection {
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("invalid package payload: {error:#}"),
            )
        }
        Ok(Ok(meta)) => meta,
        Ok(Err(error)) => return err(StatusCode::BAD_REQUEST, &format!("{error:#}")),
    };
    let (gem_name, version) = (meta.name.clone(), meta.version.clone());
    // `{name}-{version}[-{platform}].gem`, the name RubyGems itself builds. The
    // platform belongs in it because the compact index spells the same
    // `VERSION-PLATFORM` chunk back and the client turns that chunk into the
    // download URL — a native gem stored under the pure-ruby name would be
    // advertised at a path nothing serves (card_0c9e858230b6).
    let filename = match parse_rubygems_facts(meta.protocol_metadata.as_deref()).platform {
        Some(platform) => format!("{gem_name}-{version}-{platform}.gem"),
        None => format!("{gem_name}-{version}.gem"),
    };

    let published = persist_package(
        state,
        user_id,
        owner,
        name,
        "rubygems".to_string(),
        PublishPackageQuery {
            name: Some(gem_name.clone()),
            version: Some(version.clone()),
            description: None,
            homepage: None,
            repository_url: None,
            semver: None,
        },
        vec![(filename, body)],
        Some(meta),
        None,
    )
    .await;

    if !published.status().is_success() {
        return published;
    }
    (
        published.status(),
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("Successfully registered gem: {gem_name} ({version})\n"),
    )
        .into_response()
}

/// The gems published to a repository, or nothing at all.
///
/// A repository that never enabled the registry is not an error on these
/// routes — an empty index is the honest answer, and the alternative pushes the
/// client onto the legacy protocol.
pub(crate) async fn rubygems_packages(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> anyhow::Result<Vec<rg_core::package_registry::PackageSummary>> {
    match rg_core::package_registry::service::list_packages(&state.db, owner, repo, "rubygems")
        .await
    {
        Ok(packages) => Ok(packages),
        Err(error) if package_is_absent(&error) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Parse RubyGems dependencies from version metadata JSON.
///
/// card_69cfa8de4fd1: the adapter refuses to publish a runtime dependency the
/// stored shape cannot carry, so an element here without a usable `name` is
/// damage to the row rather than something a gemspec could have said. Skipping
/// it answers `200` with a dependency graph the gem never declared — and a
/// resolver cannot tell that apart from an honest answer, because a shorter
/// list still resolves. This is the same rule the whole document is already
/// read under (card_a4be92713930), one level down.
pub(crate) fn parse_rubygems_deps(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<Vec<rg_core::package_registry::RubyGemsDep>> {
    let Some(doc) =
        parse_stored_metadata_document("rubygems", package_name, version, metadata_json)?
    else {
        return Ok(Vec::new());
    };
    let Some(declared) = doc.get("dependencies").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let deps = declared.as_array().ok_or_else(|| {
        damaged_rubygems_metadata(package_name, version, "`dependencies` is not a list")
    })?;

    deps.iter()
        .enumerate()
        .map(|(index, dep)| {
            let name = dep
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    damaged_rubygems_metadata(
                        package_name,
                        version,
                        &format!("`dependencies[{index}].name` is not a non-empty string"),
                    )
                })?
                .to_string();
            // The adapter always writes a requirement string; absent is how a
            // version published before it did reads, and `>= 0` is what the
            // gemspec would have meant.
            let requirements = match dep.get("requirements").filter(|v| !v.is_null()) {
                Some(value) => value
                    .as_str()
                    .ok_or_else(|| {
                        damaged_rubygems_metadata(
                            package_name,
                            version,
                            &format!("`dependencies[{index}].requirements` is not a string"),
                        )
                    })?
                    .to_string(),
                None => ">= 0".to_string(),
            };
            Ok(rg_core::package_registry::RubyGemsDep { name, requirements })
        })
        .collect()
}

/// Stored gemspec metadata no publish could have produced. The message names
/// the coordinate and the offending element, never the stored value.
pub(crate) fn damaged_rubygems_metadata(
    package_name: &str,
    version: &str,
    detail: &str,
) -> anyhow::Error {
    anyhow::anyhow!(
        "stored rubygems metadata for package '{package_name}' version '{version}' is damaged: {detail}"
    )
}

/// Parse RubyGems gem info from version metadata JSON.
#[derive(Default)]
pub(crate) struct RubyGemsInfo {
    pub(crate) summary: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) homepage: Option<String>,
    pub(crate) license: Option<String>,
}

pub(crate) fn parse_rubygems_info(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<RubyGemsInfo> {
    let Some(doc) =
        parse_stored_metadata_document("rubygems", package_name, version, metadata_json)?
    else {
        return Ok(RubyGemsInfo::default());
    };
    let summary = doc
        .get("summary")
        .and_then(|v| v.as_str())
        .map(String::from);
    let description = doc
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let homepage = doc
        .get("homepage")
        .and_then(|v| v.as_str())
        .map(String::from);
    let license = doc
        .get("licenses")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .or_else(|| {
            doc.get("license")
                .and_then(|v| v.as_str())
                .map(String::from)
        });
    Ok(RubyGemsInfo {
        summary,
        description,
        homepage,
        license,
    })
}
