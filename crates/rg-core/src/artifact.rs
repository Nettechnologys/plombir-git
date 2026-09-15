//! CI artifact publication across blob storage and its metadata row.
//!
//! Both the runner-facing HTTP route and the embedded runner publish through
//! this module. The recovery intent must exist before the final blob does:
//! graceful compensation cannot run after `SIGKILL`, and the artifact row is
//! otherwise the only durable handle by which retention can find the bytes.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::blob_storage::{BlobKey, BlobStorage};

/// Publish a staged artifact file and create the row that owns its blob.
///
/// The generated blob key is request-private, so recovery can safely use an
/// exact row lookup as the ownership proof. A failed cleanup deliberately
/// keeps the journal entry: startup can retry it after the age bound instead
/// of losing the only name of the orphan. Ownership of `source` transfers once
/// its final blob write succeeds; before that point the caller keeps the staged
/// file and can retry or let its own spool sweep reclaim it.
#[allow(clippy::too_many_arguments)]
pub async fn publish_from_file(
    db: &rg_db::DatabaseConnection,
    storage: &dyn BlobStorage,
    job_id: i64,
    name: &str,
    source: &Path,
    size: i64,
    sha256: Option<String>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<rg_db::entities::artifact::Model> {
    let job_segment = job_id.to_string();
    let object = format!("{}-{name}", Uuid::new_v4());
    let key = BlobKey::from_segments(["artifacts", "jobs", &job_segment, &object])?;
    let publication_id = Uuid::new_v4().simple().to_string();

    crate::deletion_recovery::open_artifact_creation(storage, &publication_id, &key).await?;
    if let Err(error) = storage.put_file(&key, source).await {
        cleanup_uncommitted_artifact(storage, &key, &publication_id, "blob write").await;
        return Err(error).context("failed to store CI artifact blob");
    }
    crate::platform::fs::discard_file_async("staged CI artifact archive", source).await;

    match rg_db::ops::artifact_ops::create_artifact(
        db,
        job_id,
        name,
        key.as_str(),
        size,
        sha256,
        expires_at,
    )
    .await
    {
        Ok(artifact) => {
            if let Err(error) =
                crate::deletion_recovery::mark_committed(storage, &publication_id).await
            {
                tracing::warn!(
                    artifact_id = artifact.id,
                    job_id,
                    blob_key = %key,
                    error = %format!("{error:#}"),
                    "CI artifact publication committed, but its recovery entry could not be \
                     marked committed"
                );
            }
            crate::deletion_recovery::close(storage, &publication_id).await;
            Ok(artifact)
        }
        Err(error) => {
            cleanup_uncommitted_artifact(storage, &key, &publication_id, "metadata insert").await;
            Err(error).context("failed to persist CI artifact metadata")
        }
    }
}

/// Finish the graceful failure path without weakening the crash path.
async fn cleanup_uncommitted_artifact(
    storage: &dyn BlobStorage,
    key: &BlobKey,
    publication_id: &str,
    failed_step: &'static str,
) {
    match storage.delete(key).await {
        Ok(_) => crate::deletion_recovery::close(storage, publication_id).await,
        Err(cleanup_error) => tracing::warn!(
            blob_key = %key,
            failed_step,
            error = %cleanup_error,
            "CI artifact publication failed and cleanup could not prove the blob absent; the \
             recovery entry remains for startup"
        ),
    }
}
