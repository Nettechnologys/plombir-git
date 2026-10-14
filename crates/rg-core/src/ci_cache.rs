//! CI cache publication across the filesystem and its mutable metadata row.
//!
//! The HTTP runner route and the embedded runner both spool an archive beside
//! `_ci_cache/<repo_id>/`, then hand it to [`publish_from_spool`]. The durable
//! recovery intent must exist before the same-directory rename makes the final
//! request-private path visible: graceful rollback cannot run after `SIGKILL`,
//! and row-driven retention has no other way to discover that path.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::blob_storage::LocalBlobStorage;

/// Which ref's cache a job reads and writes (card_b7a25458b98b).
///
/// A cache key alone used to be the whole address, per repository: any writer
/// could save `target/` under the key a release job on `main` restores, from a
/// branch protection keeps out of `main`, and the release then built with it.
/// Now every entry belongs to the ref of the pipeline that saved it. A job
/// reads its own ref's entry and falls back to the default branch's, so a new
/// branch still starts warm; it writes only its own. Both executors — the
/// embedded runner and the external runners' HTTP route — address entries
/// through this one type, so they cannot disagree about where a key lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheScope {
    own: String,
    fallback: Option<String>,
}

impl CacheScope {
    /// The scope of a pipeline on `ref_name` in a repository whose default
    /// branch is `default_branch`. A bare name is read as a branch, the way
    /// a manual run spells it.
    pub fn for_pipeline(ref_name: &str, default_branch: &str) -> Self {
        let own = qualified_ref(ref_name);
        let default = qualified_ref(default_branch);
        let fallback = (default != own).then_some(default);
        Self { own, fallback }
    }

    /// Where a save under `key` goes: this ref's entry, never another's.
    pub fn save_hash(&self, key: &str) -> String {
        scoped_key_hash(&self.own, key)
    }

    /// Where a restore of `key` looks, in order: this ref, then the default
    /// branch.
    pub fn restore_hashes(&self, key: &str) -> Vec<String> {
        std::iter::once(&self.own)
            .chain(self.fallback.as_ref())
            .map(|scope| scoped_key_hash(scope, key))
            .collect()
    }
}

fn qualified_ref(name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_string()
    } else {
        format!("refs/heads/{name}")
    }
}

/// The row key of `key` under `scope`. The NUL cannot occur in a ref name, so
/// no ref/key pair can be spelled as another.
fn scoped_key_hash(scope: &str, key: &str) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0]);
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

/// The cache scope of `pipeline_id`, read from its pipeline and repository.
pub async fn scope_of_pipeline(
    db: &rg_db::DatabaseConnection,
    pipeline_id: i64,
) -> Result<CacheScope> {
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id)
        .await?
        .with_context(|| format!("pipeline {pipeline_id} not found for its cache scope"))?;
    let repository = rg_db::ops::repo_ops::find_by_id(db, pipeline.repo_id)
        .await?
        .with_context(|| {
            format!(
                "repository {} not found for its cache scope",
                pipeline.repo_id
            )
        })?;
    Ok(CacheScope::for_pipeline(
        &pipeline.ref_name,
        &repository.default_branch,
    ))
}

/// Create a request-private spool beside the cache archive it may become.
///
/// Keeping source and destination in one directory makes publication an atomic
/// rename rather than a copy that can fail halfway across filesystems. The name
/// is also registered with the startup spool sweep.
pub fn spool_in(directory: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(crate::staging::CI_CACHE_SPOOL_PREFIX)
        .suffix(crate::staging::CI_CACHE_SPOOL_SUFFIX)
        .tempfile_in(directory)
}

/// Whether an archive of `size` bytes may be stored under a key whose current
/// entry is `replaced`, against `[limits]`.
///
/// Replacing an entry adds no key and only the difference in bytes, so a
/// refresh at the limit is not refused for room the old archive already holds.
/// With neither ceiling set this asks the database nothing.
async fn refuse_over_budget(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    replaced: Option<&rg_db::entities::ci_cache_entry::Model>,
    size: i64,
    limits: &crate::storage_quota::StorageLimits,
) -> Result<()> {
    if let (None, Some(max_entries)) = (replaced, limits.ci_cache_max_entries_per_repo) {
        let (entries, _bytes) = rg_db::ops::ci_retention_ops::repo_cache_usage(db, repo_id).await?;
        if entries >= max_entries {
            // No eviction: the owner cannot list caches, so a key dropped to
            // make room would vanish without a trace. Entries leave on their
            // retention window.
            return Err(crate::error::payload_too_large(format!(
                "the repository already holds the maximum of {max_entries} CI cache entries \
                 ([limits].ci_cache_max_entries_per_repo); older entries leave when their \
                 retention window expires"
            )));
        }
    }
    let held = replaced.map_or(0, |entry| u64::try_from(entry.size).unwrap_or(0));
    let incoming = u64::try_from(size).unwrap_or(0).saturating_sub(held);
    crate::storage_quota::check_room(db, repo_id, incoming, limits)
        .await
        .map_err(crate::storage_quota::QuotaError::into_service_error)
}

/// Publish a complete cache spool and upsert the row that owns it.
///
/// Ownership of `spool` transfers only after its final rename succeeds. Every
/// caller therefore gets the same ordering: policy/read preparation, durable
/// intent, final write, metadata upsert, commit marker, intent close. A failed
/// cleanup deliberately leaves the intent behind so startup can retry after
/// proving the exact `(repo_id, file_path)` has no owner.
///
/// `limits` is checked here rather than by each caller: the HTTP route and the
/// embedded runner both publish through this function, and a check that lived
/// in the route alone let a job on the embedded runner store caches past every
/// ceiling. A refusal drops `spool`, which deletes it.
#[allow(clippy::too_many_arguments)]
pub async fn publish_from_spool(
    db: &rg_db::DatabaseConnection,
    repo_root: &Path,
    repo_id: i64,
    key_hash: &str,
    spool: tempfile::TempPath,
    size: i64,
    sha256: &str,
    limits: &crate::storage_quota::StorageLimits,
) -> Result<rg_db::entities::ci_cache_entry::Model> {
    let directory = archive_dir(repo_root, repo_id);
    tokio::fs::create_dir_all(&directory)
        .await
        .with_context(|| {
            format!(
                "failed to create CI cache directory `{}`",
                directory.display()
            )
        })?;

    let policy = rg_db::ops::ci_retention_ops::get_policy(db, repo_id).await?;
    let replaced = rg_db::ops::ci_retention_ops::find_cache_entry(db, repo_id, key_hash).await?;
    refuse_over_budget(db, repo_id, replaced.as_ref(), size, limits).await?;
    let replaced = replaced.map(|entry| entry.file_path);
    let publication_id = Uuid::new_v4().simple().to_string();
    let archive = directory.join(format!("{key_hash}.{publication_id}.tar"));
    let journal = LocalBlobStorage::new(repo_root.to_path_buf());

    crate::deletion_recovery::open_cache_creation(&journal, &publication_id, repo_id, &archive)
        .await?;

    if let Err(error) = spool.persist(&archive) {
        crate::deletion_recovery::close(&journal, &publication_id).await;
        return Err(error.error).with_context(|| {
            format!("failed to publish CI cache archive `{}`", archive.display())
        });
    }

    let recorded = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        db,
        repo_id,
        key_hash,
        archive.to_string_lossy().as_ref(),
        size,
        Some(sha256),
        policy.cache_retention_days,
    )
    .await;

    let recorded = match recorded {
        Ok(recorded) => recorded,
        Err(error) => {
            cleanup_uncommitted(&journal, &publication_id, repo_id, key_hash, &archive).await;
            return Err(error).context("failed to persist CI cache metadata");
        }
    };

    // A concurrent publisher may replace this request between its upsert and
    // the convergence read inside `upsert_cache_entry`. In that case the row
    // returned here names the winner, so this request's archive is already the
    // orphan and must follow the same guarded cleanup path as an ordinary DB
    // failure.
    if recorded.file_path != archive.to_string_lossy() {
        cleanup_uncommitted(&journal, &publication_id, repo_id, key_hash, &archive).await;
        return Ok(recorded);
    }

    if let Err(error) = crate::deletion_recovery::mark_committed(&journal, &publication_id).await {
        tracing::warn!(
            repo_id,
            cache_key_hash = key_hash,
            path = %archive.display(),
            error = %format!("{error:#}"),
            "CI cache publication committed, but its recovery entry could not be marked committed"
        );
    }
    crate::deletion_recovery::close(&journal, &publication_id).await;

    if let Some(previous) = replaced
        .as_deref()
        .and_then(|recorded| recorded_archive(&directory, recorded))
    {
        if previous != archive {
            discard_replaced(&previous, repo_id, key_hash).await;
        }
    }

    Ok(recorded)
}

fn archive_dir(repo_root: &Path, repo_id: i64) -> PathBuf {
    repo_root.join("_ci_cache").join(repo_id.to_string())
}

fn recorded_archive(directory: &Path, recorded: &str) -> Option<PathBuf> {
    Path::new(recorded)
        .file_name()
        .map(|name| directory.join(name))
}

async fn cleanup_uncommitted(
    journal: &LocalBlobStorage,
    publication_id: &str,
    repo_id: i64,
    key_hash: &str,
    archive: &Path,
) {
    match tokio::fs::remove_file(archive).await {
        Ok(()) => crate::deletion_recovery::close(journal, publication_id).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::deletion_recovery::close(journal, publication_id).await;
        }
        Err(error) => tracing::warn!(
            repo_id,
            cache_key_hash = key_hash,
            path = %archive.display(),
            %error,
            "CI cache publication failed and cleanup could not prove the archive absent; the \
             recovery entry remains for startup"
        ),
    }
}

async fn discard_replaced(path: &Path, repo_id: i64, key_hash: &str) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            repo_id,
            cache_key_hash = key_hash,
            path = %path.display(),
            %error,
            "superseded CI cache archive not deleted — the file stays on disk after the entry \
             moved to the newly published archive"
        ),
    }
}

#[cfg(test)]
mod scope_tests {
    use super::CacheScope;

    /// card_b7a25458b98b: a branch never writes where the default branch
    /// reads first, and still reads the default branch's entry second.
    #[test]
    fn a_branch_writes_only_its_own_scope_and_reads_the_default_second() {
        let feature = CacheScope::for_pipeline("refs/heads/feature", "main");
        let main = CacheScope::for_pipeline("refs/heads/main", "main");

        assert_ne!(feature.save_hash("k"), main.save_hash("k"));
        assert_eq!(main.restore_hashes("k"), vec![main.save_hash("k")]);
        assert_eq!(
            feature.restore_hashes("k"),
            vec![feature.save_hash("k"), main.save_hash("k")]
        );
        assert!(!main.restore_hashes("k").contains(&feature.save_hash("k")));
        // A bare branch name is the same scope as its full ref.
        assert_eq!(CacheScope::for_pipeline("main", "main"), main);
        // A pull request's pipeline is its own scope too.
        let pull = CacheScope::for_pipeline("refs/pull/7/head", "main");
        assert_ne!(pull.save_hash("k"), main.save_hash("k"));
    }
}
