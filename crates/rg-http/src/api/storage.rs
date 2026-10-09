//! Repository storage read-out: GET /repos/{owner}/{name}/storage.
//!
//! One place to answer "how much is this repository holding, and how much may
//! it hold". Every upload path enforces the ceiling through the same
//! [`rg_core::storage_quota`] module, so this route is the read side of that
//! policy: without it the refusal message would be the only evidence a
//! repository is near its budget, and an owner could not see which store is
//! responsible before deleting something.
//!
//! Reads are not free — six `SUM`s — but they are index scans bounded by one
//! repository, and the caller is a settings page or a CI script, not a hot path.

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

use crate::api::repo_access::RepoRead;
use crate::error::AppError;
use crate::AppState;
use rg_core::storage_quota::StorageUsage;

/// What the repository holds, store by store, in bytes and rows.
///
/// Mirrors [`StorageUsage`] as an OpenAPI schema; rg-core has no utoipa
/// dependency, and moving one there to describe this response would make every
/// core build carry the API document.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Usage {
    pub lfs_bytes: u64,
    pub lfs_objects: u64,
    pub release_bytes: u64,
    pub release_assets: u64,
    pub attachment_bytes: u64,
    pub ci_cache_bytes: u64,
    pub ci_cache_entries: u64,
    pub package_bytes: u64,
    pub package_files: u64,
    pub oci_bytes: u64,
    pub oci_blobs: u64,
    /// The six byte counts above, summed. Compared against `limits.repo_quota_bytes`.
    pub total_bytes: u64,
}

impl From<StorageUsage> for Usage {
    fn from(usage: StorageUsage) -> Self {
        Self {
            lfs_bytes: usage.lfs_bytes,
            lfs_objects: usage.lfs_objects,
            release_bytes: usage.release_bytes,
            release_assets: usage.release_assets,
            attachment_bytes: usage.attachment_bytes,
            ci_cache_bytes: usage.ci_cache_bytes,
            ci_cache_entries: usage.ci_cache_entries,
            package_bytes: usage.package_bytes,
            package_files: usage.package_files,
            oci_bytes: usage.oci_bytes,
            oci_blobs: usage.oci_blobs,
            total_bytes: usage.total_bytes(),
        }
    }
}

/// The `[limits]` ceilings in bytes, as the server resolved them.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Limits {
    pub repo_quota_bytes: u64,
    pub oci_blob_max_bytes: u64,
    pub ci_cache_max_entries_per_repo: u64,
    pub release_assets_max_per_release: u64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct StorageReport {
    pub usage: Usage,
    pub limits: Limits,
}

/// Storage read-out: GET /repos/:owner/:name/storage
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/storage",
    tag = "Repositories",
    params(("owner" = String, Path), ("name" = String, Path)),
    responses(
        (status = 200, description = "Usage per store plus the configured ceilings", body = StorageReport),
        (status = 403, description = "No read access to the repository", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = serde_json::Value),
    ),
)]
pub async fn get_storage(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    match rg_core::storage_quota::usage(&state.db, repo.id).await {
        Ok(usage) => {
            let limits = &state.storage_limits;
            (
                axum::http::StatusCode::OK,
                Json(StorageReport {
                    usage: usage.into(),
                    limits: Limits {
                        repo_quota_bytes: limits.repo_quota_bytes,
                        oci_blob_max_bytes: limits.oci_blob_max_bytes,
                        ci_cache_max_entries_per_repo: limits.ci_cache_max_entries_per_repo,
                        release_assets_max_per_release: limits.release_assets_max_per_release,
                    },
                }),
            )
                .into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}
