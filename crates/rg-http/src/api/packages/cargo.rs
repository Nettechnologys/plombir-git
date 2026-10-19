//! Cargo sparse index, publish, yank and unyank.

use super::*;

/// How much `cargo publish` frame metadata is read as JSON.
///
/// The first block of the frame is the index entry cargo expects back out of
/// the sparse index — dependencies, features, a checksum — and it is kilobytes
/// by construction. It is the one block that has to be parsed in memory, so the
/// length prefix that says how much to read is bounded rather than believed: a
/// prefix claiming the whole envelope would otherwise be a way to spend the
/// request ceiling of heap on a route whose artifact is spooled.
pub(crate) const MAX_CARGO_PUBLISH_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// GET /api/v1/repos/:owner/:name/packages/cargo/index/config.json
///
/// The first request Cargo makes against a sparse registry, and the one that
/// decides whether it will talk to it at all: without a `config.json` carrying
/// a `dl` URL the index is not a registry. Registered at the index root, so the
/// URL a user configures is
/// `sparse+{base}/api/v1/repos/{owner}/{name}/packages/cargo/index/`.
pub async fn cargo_index_config(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };
    let json = rg_core::package_registry::build_cargo_index_config(&base_url, &owner, &name);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/:owner/:name/packages/cargo/index/{prefix…}/{crate}
///
/// Cargo sparse index protocol (RFC 2789 / Cargo ≥ 1.68).
/// Returns line-delimited JSON, one line per version.
///
/// Cargo never asks for a crate by its bare name: the name is spelled out as
/// the index prefix — `1/a`, `2/ab`, `3/a/abc`, `se/rd/serde` — so the routes
/// capture two or three segments (see `cargo_index_routes` in `crate::routes`)
/// and the crate name is read off the end here. The prefix is checked against
/// the name rather than ignored: an unverified prefix would serve any crate
/// under any path, and the index would stop being addressable.
///
/// A single segment is Plombir Git's own flat spelling, `index/{crate}`, which
/// its API and UI use. The layout never produces one segment, so the two cannot
/// be confused.
pub async fn cargo_sparse_index(
    State(state): State<AppState>,
    params: axum::extract::RawPathParams,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let (owner, name, segments) = layout_segments(&params);

    let Some((pkg_name, prefix)) = segments.split_last() else {
        return err_text(StatusCode::NOT_FOUND, "empty Cargo index path");
    };
    if !prefix.is_empty() && !matches_index_prefix(prefix, pkg_name) {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!(
                "'{}' is not the index prefix of crate '{}'",
                prefix.join("/"),
                pkg_name
            ),
        );
    }

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "cargo", pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
    };

    let entries: Vec<rg_core::package_registry::CargoIndexVersion<'_>> = versions
        .iter()
        .map(|v| rg_core::package_registry::CargoIndexVersion {
            version: v.version.as_str(),
            sha256: v.sha256.as_deref(),
            yanked: !v.is_install_candidate(),
            // Dependencies and features live only in the manifest inside the
            // `.crate`; the adapter lifted them here at publish, and this is
            // where cargo's resolver reads them back.
            metadata: v.metadata.as_deref(),
        })
        .collect();

    // A row this registry cannot read is this registry's failure. Serving the
    // entry anyway would tell cargo the crate has no dependencies — resolution
    // then succeeds on that and the build dies at `unresolved import`.
    let body = match rg_core::package_registry::build_sparse_index(pkg_name, &entries) {
        Ok(body) => body,
        Err(e) => return package_error_response(e),
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            ("x-cargo-registry-type".parse().unwrap(), "sparse"),
        ],
        body,
    )
        .into_response()
}

// ── Cargo write API ───────────────────────────────────────
//
// `cargo publish` / `cargo yank` derive these URLs from the `api` key of the
// sparse index's `config.json`. Reading the index was served long before any of
// this was, so a crate could be resolved from Plombir Git but never put there by
// the tool that builds it (card_5a790cc6ac35).

/// Split cargo's publish body into its metadata and its `.crate`.
///
/// The frame is two length-prefixed blocks — `u32-LE len`, JSON, `u32-LE len`,
/// archive — and cargo sends nothing else, so a body this cannot read is a
/// protocol mismatch worth naming rather than a bad crate.
///
/// The two blocks are separated *without* the envelope ever being a `Vec`: the
/// metadata is JSON small enough to bound outright, and the `.crate` is copied
/// straight from the envelope's spool into one of its own, so the rest of the
/// publish path treats it exactly like a body upload. Slicing them out of a
/// collected buffer instead would put both the envelope and the archive in
/// heap, which is what the ingress spool exists to avoid.
pub(crate) async fn split_cargo_publish_frame(
    envelope: &PackageArtifact,
    repo_root: &FsPath,
    max_archive_bytes: usize,
) -> Result<(Vec<u8>, PackageArtifact), String> {
    use tokio::io::{AsyncReadExt as _, BufReader};

    let file = match envelope.spool_path() {
        Some(path) => tokio::fs::File::open(path)
            .await
            .map_err(|error| format!("cannot read the staged cargo publish body: {error}"))?,
        None => return Err("the cargo publish body was not staged".to_string()),
    };
    let mut reader = BufReader::with_capacity(64 * 1024, file);

    async fn read_len(
        reader: &mut (impl tokio::io::AsyncRead + Unpin),
        what: &str,
    ) -> Result<usize, String> {
        let mut prefix = [0_u8; 4];
        reader
            .read_exact(&mut prefix)
            .await
            .map_err(|_| format!("body ends before the {what} length prefix"))?;
        Ok(u32::from_le_bytes(prefix) as usize)
    }

    let metadata_len = read_len(&mut reader, "metadata").await?;
    if metadata_len > MAX_CARGO_PUBLISH_METADATA_BYTES {
        return Err(format!(
            "the metadata length prefix claims {metadata_len} bytes, more than the \
             {MAX_CARGO_PUBLISH_METADATA_BYTES}-byte limit cargo publish metadata is read under"
        ));
    }
    let mut metadata = vec![0_u8; metadata_len];
    reader.read_exact(&mut metadata).await.map_err(|_| {
        format!("the metadata length prefix claims {metadata_len} bytes, the body has fewer")
    })?;

    let archive_len = read_len(&mut reader, "crate").await?;
    if archive_len > max_archive_bytes {
        return Err(format!(
            "the crate length prefix claims {archive_len} bytes, more than the configured \
             {max_archive_bytes}-byte artifact limit"
        ));
    }
    let mut spool = UploadSpool::create(repo_root, archive_len)
        .await
        .map_err(|error| format!("cannot stage the uploaded .crate: {error}"))?;
    let mut remaining = archive_len;
    let mut buffer = vec![0_u8; 128 * 1024];
    while remaining > 0 {
        let want = remaining.min(buffer.len());
        let read = reader
            .read(&mut buffer[..want])
            .await
            .map_err(|error| format!("cannot read the staged cargo publish body: {error}"))?;
        if read == 0 {
            return Err(format!(
                "the crate length prefix claims {archive_len} bytes, the body has fewer"
            ));
        }
        spool
            .write(&buffer[..read])
            .await
            .map_err(|error| format!("cannot stage the uploaded .crate: {error}"))?;
        remaining -= read;
    }

    let trailing = reader
        .read(&mut buffer[..1])
        .await
        .map_err(|error| format!("cannot read the staged cargo publish body: {error}"))?;
    if trailing != 0 {
        let counted = envelope.len() as usize - (8 + metadata_len + archive_len);
        return Err(format!("{counted} trailing byte(s) after the crate"));
    }

    let archive = spool
        .finish()
        .await
        .map_err(|error| format!("cannot stage the uploaded .crate: {error}"))?;
    Ok((metadata, archive))
}

/// PUT /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/new
///
/// What `cargo publish` sends: one request whose body is the index metadata and
/// the `.crate` archive, each behind a `u32-LE` length. The name and version are
/// taken from that metadata rather than from the archive, because it is the
/// metadata cargo will expect back out of the index.
pub async fn cargo_publish_new(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    body: Body,
) -> axum::response::Response {
    let body = match stage_request_artifact(
        &state,
        body,
        package_upload_envelope_limit(state.package_upload_max_bytes),
    )
    .await
    {
        Ok(body) => body,
        Err(response) => return response,
    };
    let (metadata, archive) =
        match split_cargo_publish_frame(&body, &state.repo_root, state.package_upload_max_bytes)
            .await
        {
            Ok(split) => split,
            Err(reason) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    &format!("malformed cargo publish body: {reason}"),
                )
            }
        };
    // The envelope's spool has served its purpose; retire it before the publish
    // rather than holding two copies of the upload on disk through it.
    drop(body);

    let metadata: serde_json::Value = match serde_json::from_slice(&metadata) {
        Ok(value) => value,
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("cargo publish metadata is not JSON: {error}"),
            )
        }
    };
    let (Some(crate_name), Some(version)) = (
        metadata["name"].as_str().filter(|v| !v.is_empty()),
        metadata["vers"].as_str().filter(|v| !v.is_empty()),
    ) else {
        return err(
            StatusCode::BAD_REQUEST,
            "cargo publish metadata must carry a non-empty `name` and `vers`",
        );
    };

    // card_c10c3f4d28e5: the envelope is a claim, `Cargo.toml` inside the
    // `.crate` is the artifact's own identity — the same reasoning
    // `npm_publish` states, and cargo used to take the generic
    // query-parameter override that comment warns about instead. The override
    // exists for multi-artifact formats: Maven publishes a version as a `.pom`
    // plus classifier `.jar`s that carry no manifest of their own. A `.crate`
    // always has exactly one manifest, so there is nothing here for it to
    // stand in for, and letting it win published a version row, a stored
    // `{name}-{version}.crate` and a sparse-index entry — deps, features,
    // `links`, `rust_version` — describing a crate the archive is not. The
    // index looks self-consistent, `cargo` verifies only the `cksum`, and the
    // failure lands at unpacking, where the tarball unfolds into the real
    // crate's directory.
    let cargo_adapter = rg_core::package_registry::get_adapter("cargo")
        .expect("cargo is a built-in package adapter");
    let filename = format!("{crate_name}-{version}.crate");
    let (archive, inspection) = match archive
        .inspect_with_adapter(cargo_adapter, filename.clone())
        .await
    {
        Ok(result) => result,
        Err(error) => {
            return AppError::internal(format!(
                "cargo package inspection task did not complete: {error}"
            ))
            .into_response()
        }
    };
    let metadata = match inspection {
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("invalid package payload: {error:#}"),
            )
        }
        Ok(Ok(metadata)) => metadata,
        Ok(Err(error)) => return err(StatusCode::BAD_REQUEST, &format!("{error:#}")),
    };
    if metadata.name != crate_name || metadata.version != version {
        return err(
            StatusCode::BAD_REQUEST,
            &format!(
                "cargo publish metadata names `{crate_name} {version}`, but the uploaded \
                 .crate carries `{} {}`",
                metadata.name, metadata.version
            ),
        );
    }

    // `publish_package` is bypassed because the identity check above already
    // ran the adapter, but its artifact limit is not: the frame was collected
    // under the larger envelope budget, so the `.crate` inside it still has to
    // be measured against the artifact one.
    if archive.len() > state.package_upload_max_bytes as u64 {
        return AppError::payload_too_large(format!(
            "package artifact exceeds the configured {}-byte limit",
            state.package_upload_max_bytes
        ))
        .into_response();
    }

    let query = PublishPackageQuery {
        name: Some(crate_name.to_string()),
        version: Some(version.to_string()),
        description: None,
        homepage: None,
        repository_url: None,
        semver: None,
    };

    let published = persist_package(
        state,
        user_id,
        &headers,
        owner,
        name,
        "cargo".to_string(),
        query,
        vec![(filename, archive)],
        Some(metadata),
        None,
    )
    .await;

    // cargo reads the body of a 2xx as its warnings document and reports
    // anything else as a publish failure, so the generic publish envelope
    // cannot be passed through.
    if !published.status().is_success() {
        return published;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "warnings": { "invalid_categories": [], "invalid_badges": [], "other": [] }
        })),
    )
        .into_response()
}

/// DELETE /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate}/{version}/yank
pub async fn cargo_yank(
    state: State<AppState>,
    write: RepoWrite,
    path: Path<(String, String, String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    cargo_set_yanked(state, write, path, headers, true).await
}

/// PUT /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate}/{version}/unyank
pub async fn cargo_unyank(
    state: State<AppState>,
    write: RepoWrite,
    path: Path<(String, String, String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    cargo_set_yanked(state, write, path, headers, false).await
}

/// The shared body of [`cargo_yank`] / [`cargo_unyank`].
///
/// cargo separates the two by verb rather than by a flag in a body, and answers
/// `{"ok": true}` — the generic yank route's `{"yanked": …}` envelope is not
/// what it reads.
pub(crate) async fn cargo_set_yanked(
    State(state): State<AppState>,
    RepoWrite { actor_id, .. }: RepoWrite,
    Path((owner, name, crate_name, version)): Path<(String, String, String, String)>,
    headers: axum::http::HeaderMap,
    yanked: bool,
) -> axum::response::Response {
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::package_registry::service::yank_version(
        &state.db,
        &owner,
        &name,
        "cargo",
        &crate_name,
        &version,
        yanked,
    )
    .await
    {
        Ok(_) => {
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                if yanked {
                    "package.yank"
                } else {
                    "package.unyank"
                },
                Some("package_version"),
                None,
                Some(&format!("{owner}/{name}/{crate_name}@{version}")),
                Some(&headers),
                Some(serde_json::json!({
                    "repo": format!("{owner}/{name}"),
                    "pkg_type": "cargo",
                    "pkg_name": crate_name,
                    "version": version,
                    "yanked": yanked,
                })),
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}
