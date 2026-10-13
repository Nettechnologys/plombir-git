//! PyPI simple index and legacy twine upload.

use super::*;

#[derive(Default)]
pub(crate) struct TwineUpload {
    pub(crate) action: Option<String>,
    pub(crate) protocol_version: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) sha256_digest: Option<String>,
    pub(crate) filename: Option<String>,
    pub(crate) content: Option<PackageArtifact>,
    /// The PEP 740 `attestations` field, verbatim: a JSON array of attestation
    /// objects describing the file in the same form. `None` means the publisher
    /// sent none, which is a different answer from "sent some and we lost them".
    pub(crate) attestations: Option<String>,
}

pub(crate) fn set_twine_field<T>(
    slot: &mut Option<T>,
    field: &str,
    value: T,
) -> Result<(), AppError> {
    if slot.replace(value).is_some() {
        return Err(AppError::bad_request(format!(
            "Twine upload repeats the `{field}` field"
        )));
    }
    Ok(())
}

pub(crate) fn required_twine_field(value: Option<String>, field: &str) -> Result<String, AppError> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::bad_request(format!("Twine upload is missing `{field}`")))
}

/// How much of a Twine text field is read.
///
/// The coordinates and the digest are tens of bytes; `attestations` is a PEP
/// 740 array of sigstore bundles, which is tens of kilobytes. None of them is
/// the artifact, and none of them is spooled — so unlike the `content` field
/// they are read into memory, and the bound is what stops that being a way to
/// spend the multipart envelope's whole allowance on a form field.
pub(crate) const MAX_TWINE_TEXT_FIELD_BYTES: usize = 1024 * 1024;

pub(crate) async fn decode_twine_text_field(
    mut field: axum::extract::multipart::Field<'_>,
    field_name: &str,
) -> Result<String, AppError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|error| {
        package_multipart_error(error, &format!("cannot read Twine `{field_name}` field"))
    })? {
        if bytes.len() + chunk.len() > MAX_TWINE_TEXT_FIELD_BYTES {
            return Err(AppError::payload_too_large(format!(
                "Twine `{field_name}` field exceeds the {MAX_TWINE_TEXT_FIELD_BYTES}-byte limit \
                 a form field is read under"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|error| {
        AppError::bad_request(format!(
            "Twine `{field_name}` field is not valid UTF-8: {error}"
        ))
    })
}

/// Decode the multipart form emitted by `twine upload`.
///
/// Twine sends many descriptive metadata fields as well; the package adapter
/// reads the authoritative metadata from the wheel/sdist itself, so this
/// boundary only consumes the protocol controls, coordinates, digest and file.
///
/// `attestations` is the exception to that rule: it is supply-chain evidence
/// the publisher deliberately attached and that no other part of the request
/// carries, so dropping it into the ignored-field arm loses it for good while
/// the upload still answers 200 (`card_b25bd1cbc60c`).
///
/// A detached `gpg_signature` is evidence for the same reason, but Plombir Git
/// does not currently verify, store, or serve GPG sidecars. Reject it before
/// publication instead of claiming that a `twine upload --sign` succeeded.
pub(crate) async fn decode_twine_upload(
    mut multipart: Multipart,
    repo_root: &FsPath,
    max_bytes: usize,
) -> Result<TwineUpload, AppError> {
    let mut upload = TwineUpload::default();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| package_multipart_error(error, "invalid Twine multipart body"))?
    {
        let Some(field_name) = field.name().map(str::to_owned) else {
            continue;
        };
        match field_name.as_str() {
            "content" => {
                let filename = field
                    .file_name()
                    .map(str::to_owned)
                    .filter(|filename| !filename.is_empty())
                    .ok_or_else(|| {
                        AppError::bad_request("Twine `content` field has no filename")
                    })?;
                let content = stage_multipart_artifact(
                    field,
                    repo_root,
                    max_bytes,
                    "cannot read Twine `content` field",
                )
                .await?;
                set_twine_field(&mut upload.filename, "content", filename)?;
                set_twine_field(&mut upload.content, "content", content)?;
            }
            "gpg_signature" => {
                return Err(AppError::bad_request(
                    "Twine GPG signatures are not supported; upload again without `--sign`",
                ));
            }
            ":action" | "protocol_version" | "name" | "version" | "sha256_digest"
            | "attestations" => {
                let value = decode_twine_text_field(field, &field_name).await?;
                let slot = match field_name.as_str() {
                    ":action" => &mut upload.action,
                    "protocol_version" => &mut upload.protocol_version,
                    "name" => &mut upload.name,
                    "version" => &mut upload.version,
                    "sha256_digest" => &mut upload.sha256_digest,
                    "attestations" => &mut upload.attestations,
                    _ => unreachable!("matched above"),
                };
                set_twine_field(slot, &field_name, value)?;
            }
            _ => {}
        }
    }

    Ok(upload)
}

// ── PyPI Protocol Endpoints ───────────────────────────────

/// Base of the PyPI Simple Repository API for one repository, trailing slash
/// excluded — `{base}/simple`, the prefix a client is pointed at.
pub(crate) fn pypi_simple_base(base_url: &str, owner: &str, repo: &str) -> String {
    format!(
        "{}/api/v1/repos/{}/{}/packages/pypi/simple",
        base_url.trim_end_matches('/'),
        owner,
        repo,
    )
}

/// Is this file one of the distributions a PyPI client installs from?
///
/// The Simple page lists installable artifacts; a checksum or signature stored
/// beside them is not one, and offering it as a download makes pip choose a
/// file it cannot install.
pub(crate) fn is_pypi_distribution(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    lower.ends_with(".whl")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".zip")
}

/// Resolve the project a client asked for to the name it was published under.
///
/// PEP 503 has the *client* normalize the name before putting it in the URL, so
/// `pip install Matrix_PyPI` asks for `matrix-pypi/`, while the registry stores
/// whatever `Name:` the wheel metadata carried. Only reached after a lookup on
/// the literal spelling missed, so the common case still costs one query.
///
/// `Ok(None)` is the answer "the scan ran and no published project normalizes to
/// this name" — the only outcome that may become a `404`. A failed scan is an
/// `Err` and stays one: swallowing it (`.ok()?`) told pip the project does not
/// exist, which is precisely the answer it will not retry.
pub(crate) async fn resolve_pypi_project(
    db: &sea_orm::DatabaseConnection,
    owner: &str,
    repo: &str,
    requested: &str,
) -> anyhow::Result<Option<String>> {
    let wanted = rg_core::package_registry::normalize_project_name(requested);

    Ok(
        rg_core::package_registry::service::list_packages(db, owner, repo, "pypi")
            .await?
            .into_iter()
            .find(|pkg| rg_core::package_registry::normalize_project_name(&pkg.name) == wanted)
            .map(|pkg| pkg.name),
    )
}

/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}/
/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}
///
/// PyPI Simple Repository API (PEP 503) — the project page.
/// Returns an HTML page with download links for all versions.
///
/// Both spellings are routed here because PEP 503 defines the project URL
/// *with* the trailing slash and that is what pip, poetry and uv send; the bare
/// one is kept for a hand-typed URL or a proxy that strips the slash.
pub async fn pypi_simple_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    // The spelling in the URL first — one query, and the common case. Only a
    // miss pays for the normalized scan.
    let (project, versions) = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "pypi", &pkg_name,
    )
    .await
    {
        Ok(versions) => (pkg_name.clone(), versions),
        // Only a genuine absence earns the second, more expensive lookup. Any
        // other failure — the database being down, above all — is ours, and
        // retrying it under a different spelling would just fail again and then
        // be reported as "no such project": an answer `pip`/`uv` cache and never
        // retry. `AppError::from` keeps the outage a 5xx and the detail in the
        // operator log rather than in the response body (H-05).
        Err(miss) if miss.downcast_ref::<rg_core::error::NotFound>().is_some() => {
            match resolve_pypi_project(&state.db, &owner, &name, &pkg_name).await {
                Ok(Some(stored)) => match rg_core::package_registry::service::list_versions(
                    &state.db, &owner, &name, "pypi", &stored,
                )
                .await
                {
                    Ok(versions) => (stored, versions),
                    Err(e) => return AppError::from(e).into_response(),
                },
                // The scan ran and nothing normalizes to this name: the client's
                // original miss was the truth, and it is a `NotFound`, so it
                // renders as the fixed "… not found" with no `db: …` chain.
                Ok(None) => return AppError::from(miss).into_response(),
                Err(e) => return AppError::from(e).into_response(),
            }
        }
        Err(e) => return AppError::from(e).into_response(),
    };

    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };

    // The download route matches on the *stored* package name, so the link has
    // to carry that one and not the normalized spelling the client asked with.
    let link_to = |version: &str, filename: &str| {
        format!(
            "{}/api/v1/repos/{}/{}/packages/pypi/{}/{}/{}",
            base_url.trim_end_matches('/'),
            encode_path_segment(&owner),
            encode_path_segment(&name),
            encode_path_segment(&project),
            encode_path_segment(version),
            encode_path_segment(filename),
        )
    };

    // One link per distribution *file*, not per version. A release routinely
    // carries a wheel and an sdist — `twine upload dist/*` publishes both — and
    // a page with one link per version hid every artifact but the first from
    // pip entirely. The digest beside each link is that file's own, because
    // that is what the client hashes after downloading it.
    let requires_python_by_version = versions
        .iter()
        .map(|v| parse_pypi_requires_python(v.metadata.as_deref(), &project, &v.version))
        .collect::<anyhow::Result<Vec<_>>>();
    let requires_python_by_version = match requires_python_by_version {
        Ok(metadata) => metadata,
        Err(error) => return package_error_response(error),
    };
    let entries: Vec<rg_core::package_registry::PyPIVersionEntry> = versions
        .iter()
        .zip(requires_python_by_version)
        .flat_map(|(v, requires_python)| {
            // Both attributes describe the *version*, so every file of it
            // carries the same pair. A yanked version stays on the page — that
            // is what keeps an exact pin resolvable — and `data-yanked` is what
            // takes it out of every other resolution (PEP 592).
            let yanked = !v.is_install_candidate();

            // PEP 740's `data-provenance` may only name a URL this server
            // answers, so it is derived from the stored file list rather than
            // from a naming convention: the attribute appears exactly when the
            // publish that wrote the distribution also wrote its provenance.
            let stored: std::collections::HashSet<&str> =
                v.files.iter().map(|f| f.filename.as_str()).collect();

            let files: Vec<rg_core::package_registry::PyPIVersionEntry> = v
                .files
                .iter()
                .filter(|f| is_pypi_distribution(&f.filename))
                .map(|f| {
                    let provenance =
                        rg_core::package_registry::pypi_provenance_filename(&f.filename);
                    rg_core::package_registry::PyPIVersionEntry {
                        version: v.version.clone(),
                        filename: f.filename.clone(),
                        sha256: v.sha256_of(f),
                        download_url: link_to(&v.version, &f.filename),
                        requires_python: requires_python.clone(),
                        yanked,
                        provenance_url: stored
                            .contains(provenance.as_str())
                            .then(|| link_to(&v.version, &provenance)),
                    }
                })
                .collect();

            if !files.is_empty() {
                return files;
            }

            // A version with no recognisable distribution file still has to
            // appear: the name is the conventional sdist one, which is what the
            // download route falls back to as well.
            let filename = format!("{}-{}.tar.gz", project, v.version);
            let download_url = link_to(&v.version, &filename);
            vec![rg_core::package_registry::PyPIVersionEntry {
                version: v.version.clone(),
                filename,
                sha256: v.sha256.clone(),
                download_url,
                requires_python,
                yanked,
                // This entry names a file the version does not actually hold,
                // so there is nothing whose provenance could be advertised.
                provenance_url: None,
            }]
        })
        .collect();

    let html = rg_core::package_registry::build_simple_repository_html(&project, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple/
/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple
///
/// PyPI Simple Repository API (PEP 503) — the root index: one link per project,
/// each pointing at that project's page.
///
/// This is the URL a user configures as `--index-url`, so it has to answer even
/// when the repository has no PyPI packages yet: an empty index is a valid
/// answer, a 404 (which in production falls through to the SPA and returns
/// HTML) is not.
pub async fn pypi_simple_root_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, "pypi")
            .await
        {
            Ok(packages) => packages,
            Err(error) if package_is_absent(&error) => Vec::new(),
            Err(error) => return package_error_response(error),
        };

    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };
    let base = pypi_simple_base(&base_url, &owner, &name);

    let projects: Vec<rg_core::package_registry::PyPIProjectEntry> = packages
        .iter()
        .map(|pkg| rg_core::package_registry::PyPIProjectEntry {
            name: pkg.name.clone(),
            url: format!(
                "{}/{}/",
                base,
                rg_core::package_registry::normalize_project_name(&pkg.name)
            ),
        })
        .collect();

    let html = rg_core::package_registry::build_simple_root_html(&projects);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// The `Requires-Python` a PyPI version recorded at publish.
///
/// Absent for anything published before the adapter read the field, and for a
/// distribution that declared none — both mean the same thing to the page: no
/// attribute, so no claim is made either way.
pub(crate) fn parse_pypi_requires_python(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<Option<String>> {
    let Some(doc) = parse_stored_metadata_document("pypi", package_name, version, metadata_json)?
    else {
        return Ok(None);
    };
    Ok(doc
        .get("requires_python")
        .and_then(|value| value.as_str())
        .filter(|spec| !spec.is_empty())
        .map(String::from))
}
