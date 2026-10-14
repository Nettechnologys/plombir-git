//! One repository's storage accounting, and the ceilings the upload paths
//! consult before they store another byte.
//!
//! Bytes land in several unrelated stores — LFS, release assets, attachments,
//! CI caches and artifacts, packages, OCI blobs (git does not count: the
//! object database is the repository itself) — and each one had its own
//! per-request ceiling or none, so an account could fill the volume that holds
//! the git data and the database through whichever store had no total.
//! [`usage`] answers the sum, [`check_room`] answers whether an incoming write
//! fits, and the routes answer 413 (LFS: the spec's per-object 507) when it
//! does not.
//!
//! A write reaches a store by more doors than its upload route: the embedded
//! CI runner, a mirror or an import fetching LFS objects, a fork copying them,
//! a pull request merge adopting them, an OCI cross-repository mount. Each of
//! those checks the same budget, and the check lives in the core function the
//! door calls wherever the door is not a route of its own.
//!
//! **Every ceiling is opt-in.** An instance that sets none of them behaves as
//! before, and none of these checks reaches the database: an upgrade must not
//! start refusing pushes to a repository that already holds more than a number
//! nobody chose.
//!
//! The numbers are a *budget*, not a reservation. Summing is a read, so two
//! concurrent uploads can each pass the same check; the overshoot that admits
//! is bounded by the uploads in flight at that moment.

use anyhow::Result;
use sea_orm::DatabaseConnection;

/// The `[limits]` ceilings, resolved once at startup. `None` is no ceiling,
/// and [`Default`] — every one `None` — is what an instance gets without a
/// `[limits]` table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct StorageLimits {
    /// Total bytes one repository may hold across LFS, releases, attachments,
    /// CI caches and artifacts, packages and OCI blobs.
    pub repo_quota_bytes: Option<u64>,
    /// Cumulative bytes of **one OCI upload session**, across all its PATCHes
    /// and the final PUT. The HTTP body limit bounds a single request, not a
    /// session.
    pub oci_blob_max_bytes: Option<u64>,
    /// CI cache entries one repository may keep. The per-archive ceiling
    /// bounds one cache, not the number of keys.
    pub ci_cache_max_entries_per_repo: Option<u64>,
    /// Release assets one release may hold — the same unbounded-keys axis.
    pub release_assets_max_per_release: Option<u64>,
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
    pub artifact_bytes: u64,
    pub artifacts: u64,
    pub package_bytes: u64,
    pub package_files: u64,
    pub oci_bytes: u64,
    pub oci_blobs: u64,
}

impl StorageUsage {
    /// Every stored byte, saturating rather than wrapping: a sum that wrapped
    /// would read as room where there is none.
    pub fn total_bytes(&self) -> u64 {
        [
            self.lfs_bytes,
            self.release_bytes,
            self.attachment_bytes,
            self.ci_cache_bytes,
            self.artifact_bytes,
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
/// push` or a CI log needs to know which budget was hit and by how much.
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

/// The arithmetic behind [`check_room`], without a database: `used +
/// incoming` beyond `limit` is a refusal, and the message names all three.
pub fn exceeds(used: u64, incoming: u64, limit: u64) -> Option<QuotaExceeded> {
    if used.saturating_add(incoming) <= limit {
        return None;
    }
    Some(QuotaExceeded {
        message: format!(
            "repository storage quota exceeded: {used} byte(s) used + {incoming} byte(s) \
             incoming exceeds the {limit}-byte limit ([limits].repo_quota_mb)"
        ),
        used,
        incoming,
        limit,
    })
}

/// A failure that kept the check from running, kept apart from a check that
/// said no: a database outage is a 5xx, never a 413.
#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error(transparent)]
    Db(#[from] anyhow::Error),
    #[error(transparent)]
    Exceeded(#[from] QuotaExceeded),
}

impl QuotaError {
    /// The error a core service returns: a refusal becomes the 413 the HTTP
    /// layer maps, and an outage keeps its chain.
    pub fn into_service_error(self) -> anyhow::Error {
        match self {
            QuotaError::Exceeded(exceeded) => crate::error::payload_too_large(exceeded.message),
            QuotaError::Db(error) => error.context("failed to check the repository storage budget"),
        }
    }
}

/// Whether `incoming_bytes` fit in `repo_id`'s budget. With no
/// `repo_quota_bytes` this answers yes without asking the database.
pub async fn check_room(
    db: &DatabaseConnection,
    repo_id: i64,
    incoming_bytes: u64,
    limits: &StorageLimits,
) -> Result<(), QuotaError> {
    let Some(limit) = limits.repo_quota_bytes else {
        return Ok(());
    };
    let used = usage(db, repo_id).await?.total_bytes();
    match exceeds(used, incoming_bytes, limit) {
        Some(exceeded) => Err(QuotaError::Exceeded(exceeded)),
        None => Ok(()),
    }
}

/// Every byte and row `repo_id` currently holds, store by store.
pub async fn usage(db: &DatabaseConnection, repo_id: i64) -> Result<StorageUsage> {
    let (lfs_objects, lfs_bytes) = rg_db::ops::lfs_object_ops::usage(db, repo_id).await?;
    let (release_assets, release_bytes) =
        rg_db::ops::release_ops::repo_asset_usage(db, repo_id).await?;
    let attachment_bytes = rg_db::ops::attachment_ops::repo_size(db, repo_id).await?;
    let (ci_cache_entries, ci_cache_bytes) =
        rg_db::ops::ci_retention_ops::repo_cache_usage(db, repo_id).await?;
    let (artifacts, artifact_bytes) = rg_db::ops::artifact_ops::repo_usage(db, repo_id).await?;
    let (package_files, package_bytes) =
        rg_db::ops::package_file_ops::repo_usage(db, repo_id).await?;
    let (oci_blobs, oci_bytes) = rg_db::ops::oci_ops::repo_blob_usage(db, repo_id).await?;

    let bytes = |value: i64| u64::try_from(value).unwrap_or(0);
    Ok(StorageUsage {
        lfs_bytes: bytes(lfs_bytes),
        lfs_objects,
        release_bytes: bytes(release_bytes),
        release_assets,
        attachment_bytes: bytes(attachment_bytes),
        ci_cache_bytes: bytes(ci_cache_bytes),
        ci_cache_entries,
        artifact_bytes: bytes(artifact_bytes),
        artifacts,
        package_bytes: bytes(package_bytes),
        package_files,
        oci_bytes: bytes(oci_bytes),
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
        assert_eq!(
            (exceeded.used, exceeded.incoming, exceeded.limit),
            (100, 51, 150)
        );
        let message = exceeded.to_string();
        for needle in ["100", "51", "150", "repo_quota_mb"] {
            assert!(
                message.contains(needle),
                "the refusal must name {needle}: {message}"
            );
        }
    }

    #[test]
    fn the_sum_saturates_instead_of_wrapping() {
        assert_eq!(exceeds(u64::MAX, 10, u64::MAX), None);
        assert!(exceeds(u64::MAX, 10, u64::MAX - 1).is_some());
    }

    #[test]
    fn the_total_sums_every_store() {
        let usage = StorageUsage {
            lfs_bytes: 1,
            release_bytes: 2,
            attachment_bytes: 3,
            ci_cache_bytes: 4,
            artifact_bytes: 7,
            package_bytes: 5,
            oci_bytes: 6,
            ..Default::default()
        };
        assert_eq!(usage.total_bytes(), 28);
    }

    #[test]
    fn no_ceiling_is_the_default() {
        assert_eq!(
            StorageLimits::default(),
            StorageLimits {
                repo_quota_bytes: None,
                oci_blob_max_bytes: None,
                ci_cache_max_entries_per_repo: None,
                release_assets_max_per_release: None,
            }
        );
    }

    /// Unlimited asks nothing: a database that cannot answer must not turn an
    /// instance that never chose a quota into one that refuses uploads.
    #[tokio::test]
    async fn an_unlimited_budget_never_reaches_the_database() {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        // No migrations: any query would fail on a missing table.
        check_room(&db, 1, u64::MAX, &StorageLimits::default())
            .await
            .expect("no ceiling, no question");
        let error = check_room(
            &db,
            1,
            1,
            &StorageLimits {
                repo_quota_bytes: Some(10),
                ..Default::default()
            },
        )
        .await
        .expect_err("a configured ceiling has to read the usage");
        assert!(matches!(error, QuotaError::Db(_)), "{error:?}");
    }

    #[test]
    fn a_refusal_maps_to_413_and_an_outage_does_not() {
        let refusal = QuotaError::Exceeded(exceeds(10, 10, 15).unwrap()).into_service_error();
        assert!(
            refusal
                .downcast_ref::<crate::error::PayloadTooLarge>()
                .is_some(),
            "{refusal:#}"
        );
        let outage = QuotaError::Db(anyhow::anyhow!("db down")).into_service_error();
        assert!(outage
            .downcast_ref::<crate::error::PayloadTooLarge>()
            .is_none());
    }
}
