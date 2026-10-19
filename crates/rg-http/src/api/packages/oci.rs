//! OCI image listing and deletion.

use super::*;

#[derive(Debug, Serialize, ToSchema)]
pub struct OciImageSummary {
    pub namespace: String,
    pub tag: String,
    pub digest: String,
    pub size_bytes: u64,
    pub pushed_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OciImageListResponse {
    pub images: Vec<OciImageSummary>,
    pub next: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OciImageListQuery {
    pub last: Option<String>,
}

/// Logical bytes of one tagged image, including every platform manifest and
/// each distinct config/layer blob once. Index JSON alone is not the image size.
pub(crate) async fn oci_image_size(
    db: &rg_db::DatabaseConnection,
    oci_repo_id: i64,
    root: rg_db::entities::oci_manifest::Model,
) -> anyhow::Result<u64> {
    let mut pending = vec![root];
    let mut seen_manifests = std::collections::HashSet::new();
    let mut seen_blobs = std::collections::HashSet::new();
    let mut total = 0u64;
    while let Some(row) = pending.pop() {
        if !seen_manifests.insert(row.digest.clone()) {
            continue;
        }
        let parsed =
            rg_core::package_registry::oci::ParsedManifest::parse(row.manifest_json.as_bytes())?;
        total = total
            .checked_add(parsed.size)
            .ok_or_else(|| anyhow::anyhow!("OCI image size overflow"))?;
        for blob in parsed
            .manifest
            .config
            .iter()
            .chain(parsed.manifest.layers.iter())
        {
            if seen_blobs.insert(blob.digest.clone()) {
                total = total
                    .checked_add(blob.size)
                    .ok_or_else(|| anyhow::anyhow!("OCI image size overflow"))?;
            }
        }
        for child in parsed.manifest.manifests {
            if !seen_manifests.contains(&child.digest) {
                let row =
                    rg_db::ops::oci_ops::find_manifest_by_digest(db, oci_repo_id, &child.digest)
                        .await?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "OCI index references missing manifest {}",
                                child.digest
                            )
                        })?;
                pending.push(row);
            }
        }
    }
    Ok(total)
}

/// Delete an OCI image from the browser using the ordinary repository write
/// gate. The registry's Docker endpoint has its own token authentication.
#[utoipa::path(delete, path = "/repos/{owner}/{name}/packages/oci/images/{digest}", tag = "Packages", responses((status = 202, description = "Image retired; unreferenced bytes await GC"), (status = 404, description = "Image absent")))]
pub async fn delete_oci_image(
    State(state): State<AppState>,
    Path((owner, name, digest)): Path<(String, String, String)>,
    RepoWrite { repo, actor_id }: RepoWrite,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let reference = match rg_core::package_registry::oci::Reference::parse(&digest) {
        Ok(reference @ rg_core::package_registry::oci::Reference::Digest(_)) => reference,
        _ => {
            return AppError::bad_request("OCI image digest must be sha256:<64 lowercase hex>")
                .into_response()
        }
    };
    let Some(oci_repo) = (match rg_db::ops::oci_ops::find_repo_by_id(&state.db, repo.id).await {
        Ok(row) => row,
        Err(error) => return AppError::from(error).into_response(),
    }) else {
        return AppError::from(rg_core::error::not_found("OCI image")).into_response();
    };
    match crate::oci::retire_manifest(
        &state,
        oci_repo.id,
        &owner,
        &name,
        &reference,
        Some(actor_id),
        &headers,
    )
    .await
    {
        Ok(rg_db::ops::oci_ops::ManifestDeleteOutcome::Deleted) => {
            StatusCode::ACCEPTED.into_response()
        }
        Ok(rg_db::ops::oci_ops::ManifestDeleteOutcome::Missing) => {
            AppError::from(rg_core::error::not_found("OCI image")).into_response()
        }
        Ok(rg_db::ops::oci_ops::ManifestDeleteOutcome::ReferencedByIndex) => {
            AppError::conflict("OCI manifest is referenced by an image index").into_response()
        }
        Err(crate::oci::ManifestRetireError::Lease(error)) => {
            AppError::internal(format!("OCI metadata is busy: {error:#}")).into_response()
        }
        Err(crate::oci::ManifestRetireError::LeaseRecovered) => {
            AppError::service_unavailable("OCI metadata lease was recovered; retry deletion")
                .into_response()
        }
        Err(crate::oci::ManifestRetireError::Store(error)) => AppError::from(error).into_response(),
    }
}
