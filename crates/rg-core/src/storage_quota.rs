//! One repository's storage accounting, and the ceilings every write path
//! consults before it stores another byte.
//!
//! A repository is the unit the instance can enforce cheaply: it is what the
//! request path already resolves and what the user owns. Bytes land in several
//! unrelated stores (git does not, on purpose — the object database is the
//! repository itself), and before this module each one had its own ceiling or
//! none at all, so an account could fill the volume that holds the git data and
//! the database through whichever store had no total budget. [`usage`] answers
//! the sum, [`check_room`] answers whether an incoming write fits, and the
//! routes answer 413 (LFS: the spec's per-object 507) when it does not.
//!
//! The numbers are a *budget*, not a reservation. Summing is a read, so two
//! concurrent uploads can each pass the same check; the recheck after the bytes
//! are written (the attachment and OCI paths) is what narrows that window,
//! and the ceiling is generous enough that the overshoot it admits is smaller
//! than one upload.

use anyhow::Result;
use sea_orm::DatabaseConnection;

/// The `[limits]` ceilings, resolved once at startup.
///
/// [`Default`] is the set the shipped templates document; a test may build its
/// own with smaller numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct StorageLimits {
    /// Total bytes one repository may hold across LFS, releases, attachments,
    /// CI caches, packages and OCI blobs.
    pub repo_quota_bytes: u64,
    /// Cumulative bytes of **one OCI upload session**, across all its PATCHes
    /// and the final PUT. The HTTP body limit alone bounded a single request,
    /// not a session, so a client could append 10 GiB indefinitely.
    pub oci_blob_max_bytes: u64,
    /// CI cache entries one repository may keep. The per-archive byte ceiling
    /// bounded one cache, not the number of keys, so a pipeline loop could
    /// accumulate archives without limit until their TTLs expired.
    pub ci_cache_max_entries_per_repo: u64,
    /// Release assets one release may hold — the same unbounded-keys problem,
    /// where each request honoured the per-file ceiling.
    pub release_assets_max_per_release: u64,
}

/// The defaults the config layer also states as `DEFAULT_*` constants:
/// 20 GiB per repository, 10 GiB per OCI upload session, 200 cache entries,
/// 100 assets per release.
pub const DEFAULT_REPO_QUOTA_BYTES: u64 = 20 * 1024 * 1024 * 1024;
pub const DEFAULT_OCI_BLOB_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const DEFAULT_CI_CACHE_MAX_ENTRIES_PER_REPO: u64 = 200;
pub const DEFAULT_RELEASE_ASSETS_MAX_PER_RELEASE: u64 = 100;

impl Default for StorageLimits {
    fn default() -> Self {
        Self {
            repo_quota_bytes: DEFAULT_REPO_QUOTA_BYTES,
            oci_blob_max_bytes: DEFAULT_OCI_BLOB_MAX_BYTES,
            ci_cache_max_entries_per_repo: DEFAULT_CI_CACHE_MAX_ENTRIES_PER_REPO,
            release_assets_max_per_release: DEFAULT_RELEASE_ASSETS_MAX_PER_RELEASE,
        }
    }
}

/// What one repository currently holds, per store.
///
/// Every field is a `SUM` over rows that already exist, so the number is what
/// the database will still be holding after the upload: no bytes are counted
/// twice and none are counted that a later request cannot reach.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct StorageUsage {
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
}

impl StorageUsage {
    /// Every stored byte, saturating rather than wrapping: a quota sum that
    /// wrapped would read as room where there is none.
    pub fn total_bytes(&self) -> u64 {
        [
            self.lfs_bytes,
            self.release_bytes,
            self.attachment_bytes,
            self.ci_cache_bytes,
            self.package_bytes,
            self.oci_bytes,
        ]
        .into_iter()
        .fold(0_u64, u64::saturating_add)
    }
}

/// A write refused because the repository budget has no room for it.
///
/// Carries the numbers on purpose: an operator reading a 413 in a `docker
/// push` or a CI log needs to know which budget was hit and by how much, and
/// logging them here keeps every call site from formatting its own version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaExceeded {
    pub message: String,
    pub used: u64,
    pub incoming: u64,
    pub limit: u64,
}

impl std::fmt::Display for QuotaExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QuotaExceeded {}

/// The arithmetic behind [`check_room`], separated so it can be tested without
/// a database: `used + incoming` beyond `limit` is a refusal, and the message
/// names all three numbers.
pub fn exceeds(used: u64, incoming: u64, limit: u64) -> Option<QuotaExceeded> {
    if used.saturating_add(incoming) <= limit {
        return None;
    }
    Some(QuotaExceeded {
        message: format!(
            "repository storage quota exceeded: {used} byte(s) used + {incoming} byte(s) \
             incoming exceeds the {limit}-byte limit"
        ),
        used,
        incoming,
        limit,
    })
}

/// A failure that prevented the check from running, kept apart from a check
/// that said no: a database outage is a 5xx, never a 413.
#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error(transparent)]
    Db(#[from] anyhow::Error),
    #[error(transparent)]
    Exceeded(#[from] QuotaExceeded),
}

impl QuotaError {
    /// The refusal, if this is one.
    pub fn exceeded(&self) -> Option<&QuotaExceeded> {
        match self {
            QuotaError::Db(_) => None,
            QuotaError::Exceeded(exceeded) => Some(exceeded),
        }
    }
}

/// Whether `incoming_bytes` fit in `repo_id`'s budget.
pub async fn check_room(
    db: &DatabaseConnection,
    repo_id: i64,
    incoming_bytes: u64,
    limits: &StorageLimits,
) -> Result<(), QuotaError> {
    let used = usage(db, repo_id).await?.total_bytes();
    if let Some(exceeded) = exceeds(used, incoming_bytes, limits.repo_quota_bytes) {
        return Err(QuotaError::Exceeded(exceeded));
    }
    Ok(())
}

/// Every byte and row `repo_id` currently holds, store by store.
pub async fn usage(db: &DatabaseConnection, repo_id: i64) -> Result<StorageUsage> {
    let (lfs_objects, lfs_bytes) = rg_db::ops::lfs_object_ops::usage(db, repo_id).await?;
    let (release_assets, release_bytes) =
        rg_db::ops::release_ops::repo_asset_usage(db, repo_id).await?;
    let attachment_bytes = rg_db::ops::attachment_ops::repo_size(db, repo_id).await?;
    let (ci_cache_entries, ci_cache_bytes) =
        rg_db::ops::ci_retention_ops::repo_cache_usage(db, repo_id).await?;
    let (package_files, package_bytes) =
        rg_db::ops::package_file_ops::repo_usage(db, repo_id).await?;
    let (oci_blobs, oci_bytes) = rg_db::ops::oci_ops::repo_blob_usage(db, repo_id).await?;

    Ok(StorageUsage {
        lfs_bytes: lfs_bytes.max(0) as u64,
        lfs_objects,
        release_bytes: release_bytes.max(0) as u64,
        release_assets,
        attachment_bytes: attachment_bytes.max(0) as u64,
        ci_cache_bytes: ci_cache_bytes.max(0) as u64,
        ci_cache_entries,
        package_bytes: package_bytes.max(0) as u64,
        package_files,
        oci_bytes: oci_bytes.max(0) as u64,
        oci_blobs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_that_fits_is_not_refused() {
        assert_eq!(exceeds(100, 50, 150), None, "the limit is inclusive");
        assert_eq!(exceeds(0, 0, 0), None);
    }

    #[test]
    fn a_write_beyond_the_budget_names_the_numbers() {
        let exceeded = exceeds(100, 51, 150).expect("151 > 150 must be refused");
        assert_eq!(exceeded.used, 100);
        assert_eq!(exceeded.incoming, 51);
        assert_eq!(exceeded.limit, 150);
        let message = exceeded.to_string();
        for number in ["100", "51", "150"] {
            assert!(
                message.contains(number),
                "the refusal must name {number}: {message}"
            );
        }
    }

    #[test]
    fn the_sum_saturates_instead_of_wrapping() {
        assert_eq!(
            exceeds(u64::MAX, 10, u64::MAX),
            None,
            "saturated arithmetic must still see the overflow as beyond every limit"
        );
        assert!(exceeds(u64::MAX, 10, u64::MAX - 1).is_some());
    }

    #[test]
    fn the_total_sums_every_store() {
        let usage = StorageUsage {
            lfs_bytes: 1,
            release_bytes: 2,
            attachment_bytes: 3,
            ci_cache_bytes: 4,
            package_bytes: 5,
            oci_bytes: 6,
            ..Default::default()
        };
        assert_eq!(usage.total_bytes(), 21);
    }

    #[test]
    fn a_quota_error_keeps_a_refusal_readable_from_either_side() {
        let error = QuotaError::Exceeded(exceeds(10, 10, 15).unwrap());
        assert!(error.exceeded().is_some());
        assert!(error.to_string().contains("15"));
    }
}
