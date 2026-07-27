//! Durable Issue, pull-request and comment attachments.

use crate::blob_storage::{BlobKey, BlobStorage};
use crate::error::{invalid_request, not_found};
use anyhow::{Context, Result};
use chrono::Utc;
use rg_db::entities::attachment::{ActiveModel, Model as Attachment};
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

pub const MAX_ATTACHMENT_SIZE: usize = 100 * 1024 * 1024;
pub const DEFAULT_REPO_ATTACHMENT_QUOTA: i64 = 1024 * 1024 * 1024;

const ALLOWED_EXTENSIONS: &[&str] = &[
    "avif",
    "cpuprofile",
    "csv",
    "dmp",
    "docx",
    "fodg",
    "fodp",
    "fods",
    "fodt",
    "gif",
    "gz",
    "jpeg",
    "jpg",
    "json",
    "jsonc",
    "log",
    "md",
    "mov",
    "mp4",
    "odf",
    "odg",
    "odp",
    "ods",
    "odt",
    "patch",
    "pdf",
    "png",
    "pptx",
    "svg",
    "tgz",
    "txt",
    "webm",
    "webp",
    "xls",
    "xlsx",
    "zip",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentTarget {
    Issue(i64),
    PullRequest(i64),
    IssueComment(i64),
    ReviewComment(i64),
}

impl AttachmentTarget {
    pub fn matches(self, attachment: &Attachment) -> bool {
        match self {
            Self::Issue(id) => attachment.issue_id == Some(id),
            Self::PullRequest(id) => attachment.pull_request_id == Some(id),
            Self::IssueComment(id) => attachment.issue_comment_id == Some(id),
            Self::ReviewComment(id) => attachment.review_comment_id == Some(id),
        }
    }
}

// Wide by design: mirrors the attachment column set (repo/uploader identity + blob metadata).
#[allow(clippy::too_many_arguments)]
pub async fn create_attachment(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_id: i64,
    uploader_id: i64,
    target: AttachmentTarget,
    filename: &str,
    content_type: &str,
    data: &[u8],
) -> Result<Attachment> {
    let prepared = prepare_attachment(db, repo_id, filename, data.len() as u64).await?;
    // Content digest for integrity + provenance, recorded at upload time and
    // re-checked on download. Mirrors the release-asset / package-registry idiom.
    let sha256 = hex::encode(Sha256::digest(data));
    storage
        .put(&prepared.key, data)
        .await
        .context("failed to store attachment blob")?;
    persist_attachment(
        db,
        storage,
        repo_id,
        uploader_id,
        target,
        prepared,
        content_type,
        Some(sha256),
    )
    .await
}

/// Persist an attachment from a bounded temporary file without buffering the
/// complete upload in application memory.
#[allow(clippy::too_many_arguments)]
pub async fn create_attachment_from_file(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_id: i64,
    uploader_id: i64,
    target: AttachmentTarget,
    filename: &str,
    content_type: &str,
    source: &Path,
    size: u64,
) -> Result<Attachment> {
    let actual_size = tokio::fs::metadata(source)
        .await
        .with_context(|| format!("failed to inspect attachment upload {}", source.display()))?
        .len();
    if actual_size != size {
        anyhow::bail!("attachment upload size changed before storage");
    }
    let prepared = prepare_attachment(db, repo_id, filename, size).await?;
    // Stream the digest over the bounded temporary file instead of buffering the
    // whole upload in memory (the reason this variant exists).
    let sha256 = hash_file(source)
        .await
        .context("failed to hash attachment upload")?;
    storage
        .put_file(&prepared.key, source)
        .await
        .context("failed to store attachment blob")?;
    persist_attachment(
        db,
        storage,
        repo_id,
        uploader_id,
        target,
        prepared,
        content_type,
        Some(sha256),
    )
    .await
}

/// Compute the hex-encoded SHA-256 of a file by streaming it in bounded chunks,
/// so a 100 MiB upload is never held in application memory just to be hashed.
async fn hash_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

struct PreparedAttachment {
    filename: String,
    uuid: String,
    key: BlobKey,
    size: i64,
}

async fn prepare_attachment(
    db: &DatabaseConnection,
    repo_id: i64,
    filename: &str,
    size: u64,
) -> Result<PreparedAttachment> {
    let filename = validate_filename(filename)?;
    if size == 0 {
        return Err(invalid_request("attachment cannot be empty"));
    }
    if size > MAX_ATTACHMENT_SIZE as u64 {
        return Err(invalid_request("attachment exceeds the 100 MiB file limit"));
    }
    let size = i64::try_from(size)
        .map_err(|_| invalid_request("attachment exceeds the 100 MiB file limit"))?;
    let current_size = rg_db::ops::attachment_ops::repo_size(db, repo_id).await?;
    if current_size.saturating_add(size) > DEFAULT_REPO_ATTACHMENT_QUOTA {
        return Err(invalid_request("repository attachment quota exceeded"));
    }

    let uuid = Uuid::new_v4().to_string();
    let repo_segment = repo_id.to_string();
    let key = BlobKey::from_segments([
        "attachments",
        repo_segment.as_str(),
        uuid.as_str(),
        filename.as_str(),
    ])?;
    Ok(PreparedAttachment {
        filename,
        uuid,
        key,
        size,
    })
}

#[allow(clippy::too_many_arguments)]
async fn persist_attachment(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_id: i64,
    uploader_id: i64,
    target: AttachmentTarget,
    prepared: PreparedAttachment,
    content_type: &str,
    sha256: Option<String>,
) -> Result<Attachment> {
    let PreparedAttachment {
        filename,
        uuid,
        key,
        size,
    } = prepared;

    let (issue_id, pull_request_id, issue_comment_id, review_comment_id) = match target {
        AttachmentTarget::Issue(id) => (Some(id), None, None, None),
        AttachmentTarget::PullRequest(id) => (None, Some(id), None, None),
        AttachmentTarget::IssueComment(id) => (None, None, Some(id), None),
        AttachmentTarget::ReviewComment(id) => (None, None, None, Some(id)),
    };
    let model = ActiveModel {
        uuid: Set(uuid),
        repo_id: Set(repo_id),
        uploader_id: Set(uploader_id),
        issue_id: Set(issue_id),
        pull_request_id: Set(pull_request_id),
        issue_comment_id: Set(issue_comment_id),
        review_comment_id: Set(review_comment_id),
        filename: Set(filename),
        blob_key: Set(key.to_string()),
        content_type: Set(normalize_content_type(content_type)),
        size: Set(size),
        download_count: Set(0),
        created_at: Set(Utc::now()),
        sha256: Set(sha256),
        ..Default::default()
    };
    match rg_db::ops::attachment_ops::create(db, model).await {
        Ok(attachment) => Ok(attachment),
        Err(error) => {
            // Compensation, not the outcome: the original DB error is what the
            // caller must see, so a failed rollback can only be reported here.
            if let Err(cleanup_error) = storage.delete(&key).await {
                tracing::warn!(
                    blob_key = %key,
                    repo_id,
                    error = %cleanup_error,
                    "orphaned attachment blob: metadata insert failed and the rollback delete failed too — the blob stays in storage with no row pointing at it"
                );
            }
            Err(error).context("failed to persist attachment metadata")
        }
    }
}

pub async fn list_attachments(
    db: &DatabaseConnection,
    repo_id: i64,
    target: AttachmentTarget,
) -> Result<Vec<Attachment>> {
    match target {
        AttachmentTarget::Issue(id) => {
            rg_db::ops::attachment_ops::list_by_issue(db, repo_id, id).await
        }
        AttachmentTarget::PullRequest(id) => {
            rg_db::ops::attachment_ops::list_by_pull_request(db, repo_id, id).await
        }
        AttachmentTarget::IssueComment(id) => {
            rg_db::ops::attachment_ops::list_by_issue_comment(db, repo_id, id).await
        }
        AttachmentTarget::ReviewComment(id) => {
            rg_db::ops::attachment_ops::list_by_review_comment(db, repo_id, id).await
        }
    }
}

pub async fn get_attachment(
    db: &DatabaseConnection,
    repo_id: i64,
    target: AttachmentTarget,
    attachment_id: i64,
) -> Result<Attachment> {
    // Typed, not a message: the handlers used to sniff `"not found"` out of the
    // rendered string, which quietly answers 404 to any other error whose text
    // happens to contain those words.
    let attachment = rg_db::ops::attachment_ops::find_by_id(db, attachment_id)
        .await?
        .ok_or_else(|| not_found("attachment"))?;
    if attachment.repo_id != repo_id || !target.matches(&attachment) {
        return Err(not_found("attachment"));
    }
    Ok(attachment)
}

pub async fn download_attachment(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_id: i64,
    target: AttachmentTarget,
    attachment_id: i64,
) -> Result<(Attachment, Vec<u8>)> {
    let attachment = get_attachment(db, repo_id, target, attachment_id).await?;
    let key = BlobKey::new(attachment.blob_key.clone())?;
    let data = storage
        .get(&key)
        .await
        .context("failed to read attachment blob")?;
    // Integrity check: the stored bytes must still hash to the digest recorded
    // at upload. Legacy attachments (uploaded before digest tracking) carry no
    // recorded hash and are served without this guard.
    if let Some(expected) = attachment.sha256.as_deref() {
        let actual = hex::encode(Sha256::digest(&data));
        if actual != expected {
            anyhow::bail!(
                "attachment integrity check failed: expected sha256 {expected}, got {actual}"
            );
        }
    }
    rg_db::ops::attachment_ops::increment_download_count(db, attachment.id).await?;
    Ok((attachment, data))
}

pub async fn delete_attachment(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_id: i64,
    target: AttachmentTarget,
    attachment_id: i64,
) -> Result<()> {
    let attachment = get_attachment(db, repo_id, target, attachment_id).await?;
    let key = BlobKey::new(attachment.blob_key.clone())?;
    let backup = if let Some(source) = storage.local_path(&key) {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-attachment-delete-{}.tmp",
            Uuid::new_v4()
        ));
        tokio::fs::copy(&source, &path)
            .await
            .context("failed to back up attachment before deletion")?;
        AttachmentBackup::File(path)
    } else {
        AttachmentBackup::Bytes(storage.get(&key).await.ok())
    };
    if let Err(error) = storage.delete(&key).await {
        backup.cleanup().await;
        return Err(error).context("failed to delete attachment blob");
    }
    if let Err(error) = rg_db::ops::attachment_ops::delete_by_id(db, attachment.id).await {
        // The blob is already gone. If putting it back fails, the row survives
        // pointing at nothing and the bytes are lost for good — that outcome
        // must be named in the log, since the caller only ever sees the DB error.
        let restore_failure: Option<String> = match &backup {
            AttachmentBackup::File(path) => storage
                .put_file(&key, path)
                .await
                .err()
                .map(|error| error.to_string()),
            AttachmentBackup::Bytes(Some(data)) => storage
                .put(&key, data)
                .await
                .err()
                .map(|error| error.to_string()),
            AttachmentBackup::Bytes(None) => {
                Some("no backup was captured before the blob was deleted".to_string())
            }
        };
        if let Some(reason) = restore_failure {
            tracing::warn!(
                attachment_id = attachment.id,
                blob_key = %key,
                repo_id,
                error = %reason,
                "attachment lost: the blob was deleted, the metadata delete failed, and restoring the blob failed too — the row now points at a missing blob"
            );
        }
        backup.cleanup().await;
        return Err(error).context("failed to delete attachment metadata");
    }
    backup.cleanup().await;
    Ok(())
}

enum AttachmentBackup {
    File(std::path::PathBuf),
    Bytes(Option<Vec<u8>>),
}

impl AttachmentBackup {
    async fn cleanup(&self) {
        if let Self::File(path) = self {
            crate::platform::fs::discard_file_async("attachment blob backup", path).await;
        }
    }
}

/// Every rejection here is the uploader's to fix, so each one is typed as such:
/// the handler classifies on [`InvalidRequest`] rather than on "the service
/// returned an error", which is what used to make an unwritable blob store look
/// like a bad filename.
fn validate_filename(filename: &str) -> Result<String> {
    let filename = filename.trim();
    if filename.is_empty() || filename.len() > 255 || filename.chars().any(char::is_control) {
        return Err(invalid_request("invalid attachment filename"));
    }
    if filename.contains('/') || filename.contains('\\') || matches!(filename, "." | "..") {
        return Err(invalid_request(
            "attachment filename must not contain a path",
        ));
    }
    let extension = filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    if !ALLOWED_EXTENSIONS.contains(&extension.as_str()) {
        return Err(invalid_request("attachment file type is not allowed"));
    }
    Ok(filename.to_string())
}

fn normalize_content_type(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value.len() > 255 || value.contains(['\r', '\n']) {
        "application/octet-stream".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_content_type, validate_filename};

    #[test]
    fn validates_gitea_default_extensions() {
        assert_eq!(validate_filename("trace.JSON").unwrap(), "trace.JSON");
        assert!(validate_filename("payload.exe").is_err());
        assert!(validate_filename("../report.pdf").is_err());
    }

    #[test]
    fn sanitizes_content_type_headers() {
        assert_eq!(normalize_content_type(""), "application/octet-stream");
        assert_eq!(
            normalize_content_type("text/plain\r\nx: y"),
            "application/octet-stream"
        );
    }
}
