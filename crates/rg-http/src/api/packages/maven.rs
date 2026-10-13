//! Maven repository layout: metadata and artifact upload and download.

use super::*;

// ── Maven Protocol Endpoints ──────────────────────────────

/// The repository and the layout segments of one Maven request.
///
/// A Maven client writes a `groupId` with one path segment per dot, so
/// `com.example:matrix-maven` is fetched from `com/example/matrix-maven/…` and
/// the number of segments depends on the group. The routes therefore capture
/// the layout as `{m1}`, `{m2}`, … (see `maven_layout_routes` in
/// `crate::routes`) and the split back into coordinates happens here, from the
/// end, where the shape is fixed.
pub(crate) struct MavenRequest {
    pub(crate) owner: String,
    pub(crate) repo: String,
    /// Everything below `.../packages/maven/`, in order.
    pub(crate) segments: Vec<String>,
}

impl MavenRequest {
    /// Read the captures by name — see [`layout_segments`], which Cargo's index
    /// routes share.
    pub(crate) fn from_params(params: &axum::extract::RawPathParams) -> Self {
        let (owner, repo, segments) = layout_segments(params);

        Self {
            owner,
            repo,
            segments,
        }
    }

    /// `<group…>/<artifact>` → the `groupId:artifactId` the registry stores.
    ///
    /// The group is everything before the artifact, joined back with the dots
    /// the client replaced by slashes — so the flat spelling Plombir Git's own
    /// API uses (`com.example/matrix-maven`) resolves to the same name.
    pub(crate) fn coordinates(group_and_artifact: &[String]) -> Option<(String, String)> {
        let (artifact_id, group) = group_and_artifact.split_last()?;
        if group.is_empty() || artifact_id.is_empty() {
            return None;
        }
        Some((group.join("."), artifact_id.clone()))
    }
}

/// GET /api/v1/repos/{owner}/{name}/packages/maven/{group…}/{artifact}/maven-metadata.xml
///
/// Maven metadata XML endpoint — returns version list in Maven's standard format.
pub async fn maven_metadata(
    State(state): State<AppState>,
    params: axum::extract::RawPathParams,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let request = MavenRequest::from_params(&params);
    let Some((group_id, artifact_id)) = MavenRequest::coordinates(&request.segments) else {
        return err(
            StatusCode::NOT_FOUND,
            "Maven metadata path carries no groupId/artifactId",
        );
    };
    let (owner, name) = (request.owner, request.repo);

    // Maven package names are stored as "{groupId}:{artifactId}"
    let pkg_name = format!("{}:{}", group_id, artifact_id);

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "maven", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(error) if package_is_absent(&error) => {
            // Return empty metadata rather than 404 — Maven/Gradle handle gracefully
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<metadata>\n  <groupId>{}</groupId>\n  <artifactId>{}</artifactId>\n  <versioning>\n    <versions/>\n  </versioning>\n</metadata>\n",
                    escape_xml(&group_id),
                    escape_xml(&artifact_id),
                ),
            ).into_response();
        }
        Err(error) => return package_error_response(error),
    };

    let entries: Result<Vec<rg_core::package_registry::MavenVersionEntry>, anyhow::Error> =
        versions
            .iter()
            // Maven metadata has no spelling for a withdrawn release. Omitting it
            // removes it from both `<versions>` and the derived `<release>`.
            .filter(|v| v.is_install_candidate())
            .map(|v| {
                let updated = chrono::DateTime::parse_from_rfc3339(&v.created_at)
                    .map_err(|error| {
                        anyhow::anyhow!(
                        "stored creation time for Maven package version '{}' is invalid: {error}",
                        v.version
                    )
                    })?
                    .to_utc()
                    .format("%Y%m%d%H%M%S")
                    .to_string();
                Ok(rg_core::package_registry::MavenVersionEntry {
                    version: v.version.clone(),
                    is_snapshot: v.version.ends_with("-SNAPSHOT"),
                    updated,
                })
            })
            .collect();
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) => return package_error_response(error),
    };

    let xml =
        rg_core::package_registry::build_maven_metadata_xml(&group_id, &artifact_id, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/maven/{group…}/{artifact}/{version}/{file}
///
/// The artifact itself, at the path `mvn` and Gradle build from the coordinate:
/// the group's dots are slashes, and the version is a directory. Everything
/// before the last three segments is the group.
pub async fn maven_download(
    State(state): State<AppState>,
    params: axum::extract::RawPathParams,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let request = MavenRequest::from_params(&params);

    // `<group…>/<artifact>/<version>/<file>` — at least four segments, and the
    // coordinate is read off the end so the group can be any length.
    let Some((filename, head)) = request.segments.split_last() else {
        return err(StatusCode::NOT_FOUND, "empty Maven path");
    };
    let Some((version, head)) = head.split_last() else {
        return err(StatusCode::NOT_FOUND, "Maven path carries no version");
    };
    let Some((group_id, artifact_id)) = MavenRequest::coordinates(head) else {
        return err(
            StatusCode::NOT_FOUND,
            "Maven path carries no groupId/artifactId",
        );
    };

    let pkg_name = format!("{}:{}", group_id, artifact_id);

    // A resolver fetches `<artifact>.jar.sha1` right after the artifact to check
    // it. The sidecars are not stored — see `maven_upload`, which verifies them
    // on the way in rather than keeping a second copy of a number the bytes
    // already determine — so they are computed here from the file itself. A
    // registry that 404s them makes every `mvn` build print a checksum warning
    // for artifacts that are in fact intact.
    if let Some((target, algorithm)) =
        rg_core::package_registry::MavenChecksum::split_sidecar(filename)
    {
        return match hash_package_file(
            &state,
            &request.owner,
            &request.repo,
            &pkg_name,
            version,
            target,
            algorithm,
        )
        .await
        {
            Ok(digest) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                digest,
            )
                .into_response(),
            Err(response) => response,
        };
    }

    serve_package_file(
        &state,
        &request.owner,
        &request.repo,
        "maven",
        &pkg_name,
        version,
        filename,
    )
    .await
}

/// PUT /api/v1/repos/{owner}/{name}/packages/maven/{group…}/{artifact}/maven-metadata.xml
///
/// Accepted, and deliberately not stored. `mvn deploy` finishes by uploading its
/// own copy of the version list, but the list this registry serves is derived
/// from its own rows (see [`maven_metadata`]) — keeping the client's copy would
/// create a second, immediately stale source of the same answer, and refusing it
/// would fail the whole deploy over a document nobody reads back.
///
/// Separate from [`maven_upload`] because on this route the filename is part of
/// the pattern rather than a capture: the shared handler would read `{m3}` as
/// the file and mistake the coordinate for one segment short.
pub async fn maven_upload_metadata(
    State(state): State<AppState>,
    _write: RepoWrite,
    _params: axum::extract::RawPathParams,
    body: Body,
) -> axum::response::Response {
    if let Err(response) =
        stage_request_artifact(&state, body, state.package_upload_max_bytes).await
    {
        return response;
    }
    (StatusCode::OK, "").into_response()
}

/// PUT /api/v1/repos/{owner}/{name}/packages/maven/{group…}/{artifact}/{version}/{file}
///
/// The way `mvn deploy` publishes: one `PUT` per file, addressed by the very
/// layout the resolver later reads. The registry only had `POST
/// .../packages/maven/publish`, a spelling no Maven client knows, so the
/// deploy half of the round trip could not be driven by the real tool at all
/// (card_11d8655a9cd8).
///
/// Three kinds of upload arrive on this path and each is answered differently:
///
///   * `maven-metadata.xml` is **accepted and not stored**. The version list
///     this registry serves is derived from its own rows (`maven_metadata`), so
///     storing the client's copy would create a second, immediately stale
///     source of the same answer. Refusing it instead would fail the deploy over
///     a document we do not need.
///   * `*.sha1` / `*.md5` are **verified, not stored**. A checksum is a claim
///     about bytes we already hold, so the useful thing to do with it is check
///     it — a mismatch fails the deploy loudly, which is the entire point of
///     sending one. `maven_download` computes them back on the way out.
///   * everything else is the artifact, published under the coordinate the URL
///     spells out. The URL wins over anything the POM says, because the URL is
///     where the resolver will come looking.
pub async fn maven_upload(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    params: axum::extract::RawPathParams,
    body: Body,
) -> axum::response::Response {
    let body = match stage_request_artifact(&state, body, state.package_upload_max_bytes).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let request = MavenRequest::from_params(&params);

    let Some((filename, head)) = request.segments.split_last() else {
        return err(StatusCode::NOT_FOUND, "empty Maven path");
    };

    // A SNAPSHOT deploy sends `<group…>/<artifact>/<version>/maven-metadata.xml`,
    // and its checksums arrive as ordinary files because the sidecar suffix
    // stops the static route from matching. Both are the derived document —
    // accepted, not stored, same as the version-less spelling that
    // `maven_upload_metadata` answers.
    let base = rg_core::package_registry::MavenChecksum::split_sidecar(filename)
        .map_or(filename.as_str(), |(target, _)| target);
    if base == "maven-metadata.xml" {
        return (StatusCode::OK, "").into_response();
    }

    let Some((version, head)) = head.split_last() else {
        return err(StatusCode::NOT_FOUND, "Maven path carries no version");
    };
    let Some((group_id, artifact_id)) = MavenRequest::coordinates(head) else {
        return err(
            StatusCode::NOT_FOUND,
            "Maven path carries no groupId/artifactId",
        );
    };
    let pkg_name = format!("{group_id}:{artifact_id}");

    if let Some((target, algorithm)) =
        rg_core::package_registry::MavenChecksum::split_sidecar(filename)
    {
        let expected = match hash_package_file(
            &state,
            &request.owner,
            &request.repo,
            &pkg_name,
            version,
            target,
            algorithm,
        )
        .await
        {
            Ok(digest) => digest,
            Err(response) => return response,
        };
        // A checksum sidecar is one hex digest and at most a filename after it.
        // Bounded rather than collected: the route's ceiling is the artifact
        // one, and nothing says a client cannot PUT half a gigabyte named
        // `x.jar.sha1`.
        let claimed = match body.head(256) {
            Ok(head) => head,
            Err(error) => {
                return AppError::internal(format!(
                    "cannot read the staged checksum upload: {error}"
                ))
                .into_response()
            }
        };
        let claimed = String::from_utf8_lossy(&claimed);
        // Maven writes the bare hex digest, but some clients append a filename
        // the way `sha1sum` does.
        let claimed = claimed.split_whitespace().next().unwrap_or("");
        if !claimed.eq_ignore_ascii_case(&expected) {
            return err(
                StatusCode::BAD_REQUEST,
                &format!(
                    "checksum mismatch for {target}: the upload claims {claimed}, \
                     the stored file is {expected}"
                ),
            );
        }
        return (StatusCode::OK, "").into_response();
    }

    let query = PublishPackageQuery {
        name: Some(pkg_name),
        version: Some(version.clone()),
        description: None,
        homepage: None,
        repository_url: None,
        semver: None,
    };

    publish_package(
        state,
        user_id,
        request.owner,
        request.repo,
        "maven".to_string(),
        query,
        filename.clone(),
        body,
    )
    .await
}

/// Simple XML string escaping.
pub(crate) fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
