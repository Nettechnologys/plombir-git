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

/// Publish a complete cache spool and upsert the row that owns it.
///
/// Ownership of `spool` transfers only after its final rename succeeds. Every
/// caller therefore gets the same ordering: policy/read preparation, durable
/// intent, final write, metadata upsert, commit marker, intent close. A failed
/// cleanup deliberately leaves the intent behind so startup can retry after
/// proving the exact `(repo_id, file_path)` has no owner.
pub async fn publish_from_spool(
    db: &rg_db::DatabaseConnection,
    repo_root: &Path,
    repo_id: i64,
    key_hash: &str,
    spool: tempfile::TempPath,
    size: i64,
    sha256: &str,
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
    let replaced = rg_db::ops::ci_retention_ops::find_cache_entry(db, repo_id, key_hash)
        .await?
        .map(|entry| entry.file_path);
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
