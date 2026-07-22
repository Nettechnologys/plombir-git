//! Git LFS service — implements the LFS batch API.
//!
//! Git LFS (Large File Storage) replaces large files with pointer files in Git,
//! while storing the actual content separately. This service implements the
//! LFS batch API for upload/download operations.
//!
//! Storage layout: `<repo_root>/<owner>/<repo>.lfs/<oid_prefix>/<oid>`
//!
//! ## Compression
//!
//! LFS objects are compressed using zstd by default. Storage format:
//! - Compressed: `<oid>.zst` (zstd compressed)
//! - Uncompressed (legacy): `<oid>` (raw)
//!
//! The `compression` field in DB tracks the algorithm used.

use anyhow::{Context, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sea_orm::{ActiveModelTrait, DatabaseConnection};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::io::Write;
use std::path::PathBuf;

use crate::blob_storage::{BlobKey, BlobStorage};
use rg_db::entities::lfs_object;
use rg_db::ops::lfs_object_ops;

/// Compression level for zstd (1-22, default 3)
const ZSTD_LEVEL: i32 = 3;

/// Compression algorithm name
const COMPRESSION_ALGO: &str = "zstd";

/// Signed download URLs are deliberately short-lived to limit leakage.
pub const DOWNLOAD_URL_TTL_SECONDS: i64 = 60 * 60;
/// Upload URLs allow enough time for large objects on slow connections.
pub const UPLOAD_URL_TTL_SECONDS: i64 = 6 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfsActionKind {
    Download,
    Upload,
}

impl LfsActionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Upload => "upload",
        }
    }

    fn ttl_seconds(self) -> i64 {
        match self {
            Self::Download => DOWNLOAD_URL_TTL_SECONDS,
            Self::Upload => UPLOAD_URL_TTL_SECONDS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LfsActionSignatureError {
    #[error("LFS action URL has expired")]
    Expired,
    #[error("invalid LFS action URL signature")]
    Invalid,
}

type HmacSha256 = Hmac<Sha256>;

fn action_signature_payload(
    action: LfsActionKind,
    repo_id: i64,
    oid: &str,
    expires_at: i64,
) -> String {
    format!(
        "ironforge-lfs-v1:{}:{}:{}:{}",
        action.as_str(),
        repo_id,
        oid,
        expires_at
    )
}

/// Sign an LFS action URL. The signature is bound to action, repository,
/// object and expiry so a URL cannot be reused for another purpose.
pub fn sign_action_url(
    secret: &[u8],
    action: LfsActionKind,
    repo_id: i64,
    oid: &str,
    expires_at: i64,
) -> String {
    let mut mac = HmacSha256::new_from_slice(secret)
        .expect("HMAC-SHA256 accepts keys of any non-negative length");
    mac.update(action_signature_payload(action, repo_id, oid, expires_at).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Verify a signed LFS action URL at a caller-provided timestamp.
/// Supplying `now` keeps expiry behavior deterministic in tests.
pub fn verify_action_url(
    secret: &[u8],
    action: LfsActionKind,
    repo_id: i64,
    oid: &str,
    expires_at: i64,
    signature: &str,
    now: i64,
) -> std::result::Result<(), LfsActionSignatureError> {
    if expires_at <= now {
        return Err(LfsActionSignatureError::Expired);
    }
    let signature = hex::decode(signature).map_err(|_| LfsActionSignatureError::Invalid)?;
    let mut mac = HmacSha256::new_from_slice(secret)
        .expect("HMAC-SHA256 accepts keys of any non-negative length");
    mac.update(action_signature_payload(action, repo_id, oid, expires_at).as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| LfsActionSignatureError::Invalid)
}

/// Git LFS SHA-256 object identifiers are exactly 64 lowercase hex bytes.
pub fn is_valid_oid(oid: &str) -> bool {
    oid.len() == 64
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// ── LFS API types ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsBatchRequest {
    pub operation: String, // "upload" or "download"
    pub objects: Vec<LfsObjectRequest>,
    pub transfers: Option<Vec<String>>, // e.g. ["basic"]
    pub refname: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsObjectRequest {
    pub oid: String,
    pub size: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsBatchResponse {
    pub transfer: String,
    pub objects: Vec<LfsObjectResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsObjectResponse {
    pub oid: String,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<LfsActions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<LfsError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsActions {
    pub download: Option<LfsAction>,
    pub upload: Option<LfsAction>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsAction {
    pub href: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "expires_in")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsError {
    pub code: i32,
    pub message: String,
}

// ── LFS service ───────────────────────────────────────────────────────────

/// Get the storage path for an LFS object.
fn lfs_object_path(lfs_root: &std::path::Path, oid: &str) -> PathBuf {
    // Shard by first 2 hex chars: <lfs_root>/ab/<full-oid>
    let prefix = &oid[..2];
    lfs_root.join(prefix).join(oid)
}

/// Stable storage key for an LFS object. New objects use backend-neutral keys;
/// the historical `<owner>.lfs/<repo>` layout remains a read/delete fallback.
pub fn lfs_object_key(owner: &str, repo: &str, oid: &str, compressed: bool) -> Result<BlobKey> {
    if !is_valid_oid(oid) {
        anyhow::bail!("invalid LFS object identifier");
    }
    let filename = if compressed {
        format!("{oid}.zst")
    } else {
        oid.to_string()
    };
    BlobKey::from_segments(["lfs", owner, repo, &oid[..2], &filename]).map_err(Into::into)
}

/// Get the LFS root directory for a repository.
pub fn lfs_root(repo_root: &std::path::Path, owner: &str, repo: &str) -> PathBuf {
    repo_root.join(format!("{}.lfs", owner)).join(repo)
}

/// Handle a batch upload/download request.
/// Processes all objects concurrently using `join_all` to avoid serial DB round-trips.
pub async fn batch(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    lfs_root: &std::path::Path,
    base_url: &str,
    owner: &str,
    repo: &str,
    req: &LfsBatchRequest,
    signing_secret: &[u8],
) -> Result<LfsBatchResponse> {
    let transfer = req
        .transfers
        .as_ref()
        .and_then(|t| t.first().cloned())
        .unwrap_or_else(|| "basic".to_string());

    let operation = req.operation.as_str();
    let futures: Vec<_> = req
        .objects
        .iter()
        .map(|obj_req| {
            let oid = &obj_req.oid;
            let size = obj_req.size;
            async move {
                if !is_valid_oid(oid) || size < 0 {
                    return Ok(LfsObjectResponse {
                        oid: oid.to_string(),
                        size,
                        actions: None,
                        error: Some(LfsError {
                            code: 422,
                            message: "invalid LFS object identifier or size".to_string(),
                        }),
                    });
                }
                match operation {
                    "upload" => {
                        handle_upload(
                            db,
                            repo_id,
                            storage,
                            lfs_root,
                            base_url,
                            owner,
                            repo,
                            oid,
                            size,
                            signing_secret,
                        )
                        .await
                    }
                    "download" => {
                        handle_download(
                            db,
                            repo_id,
                            lfs_root,
                            base_url,
                            owner,
                            repo,
                            oid,
                            size,
                            signing_secret,
                        )
                        .await
                    }
                    _ => Ok(LfsObjectResponse {
                        oid: oid.to_string(),
                        size,
                        actions: None,
                        error: Some(LfsError {
                            code: 422,
                            message: format!("unsupported operation: {}", operation),
                        }),
                    }),
                }
            }
        })
        .collect();

    let objects = futures::future::join_all(futures)
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    Ok(LfsBatchResponse { transfer, objects })
}

#[allow(clippy::too_many_arguments)]
async fn handle_upload(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    lfs_root: &std::path::Path,
    base_url: &str,
    owner: &str,
    repo: &str,
    oid: &str,
    size: i64,
    signing_secret: &[u8],
) -> Result<LfsObjectResponse> {
    // Check if object already exists
    let existing = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await?;

    if let Some(obj) = &existing {
        if obj.uploaded {
            let compressed_key = lfs_object_key(owner, repo, oid, true)?;
            let raw_key = lfs_object_key(owner, repo, oid, false)?;
            let obj_path = lfs_object_path(lfs_root, oid);
            if storage.exists(&compressed_key).await?
                || storage.exists(&raw_key).await?
                || obj_path.exists()
                || obj_path.with_extension("zst").exists()
            {
                // Already uploaded — no action needed
                return Ok(LfsObjectResponse {
                    oid: oid.to_string(),
                    size,
                    actions: None,
                    error: None,
                });
            }
        }
    }

    // Register object if not yet tracked
    if existing.is_none() {
        let model = lfs_object::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: sea_orm::Set(repo_id),
            oid: sea_orm::Set(oid.to_string()),
            size: sea_orm::Set(size),
            uploaded: sea_orm::Set(false),
            compression: sea_orm::Set(None),
            compressed_size: sea_orm::Set(None),
            created_at: sea_orm::Set(Utc::now()),
        };
        lfs_object_ops::create(db, model).await?;
    }

    // Return upload URL
    let expires_at = Utc::now().timestamp() + LfsActionKind::Upload.ttl_seconds();
    let signature = sign_action_url(
        signing_secret,
        LfsActionKind::Upload,
        repo_id,
        oid,
        expires_at,
    );
    let upload_href = format!(
        "{}/api/v1/repos/{}/{}/lfs/objects/{}?expires={}&signature={}",
        base_url, owner, repo, oid, expires_at, signature
    );

    Ok(LfsObjectResponse {
        oid: oid.to_string(),
        size,
        actions: Some(LfsActions {
            download: None,
            upload: Some(LfsAction {
                href: upload_href,
                header: None,
                expires_in: Some(UPLOAD_URL_TTL_SECONDS),
            }),
        }),
        error: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn handle_download(
    db: &DatabaseConnection,
    repo_id: i64,
    _lfs_root: &std::path::Path,
    base_url: &str,
    owner: &str,
    repo: &str,
    oid: &str,
    size: i64,
    signing_secret: &[u8],
) -> Result<LfsObjectResponse> {
    let existing = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid)
        .await?
        .ok_or_else(|| anyhow::anyhow!("LFS object {} not found", oid))?;

    if !existing.uploaded {
        return Ok(LfsObjectResponse {
            oid: oid.to_string(),
            size,
            actions: None,
            error: Some(LfsError {
                code: 404,
                message: "object not uploaded yet".to_string(),
            }),
        });
    }

    let expires_at = Utc::now().timestamp() + LfsActionKind::Download.ttl_seconds();
    let signature = sign_action_url(
        signing_secret,
        LfsActionKind::Download,
        repo_id,
        oid,
        expires_at,
    );
    let download_href = format!(
        "{}/api/v1/repos/{}/{}/lfs/objects/{}?expires={}&signature={}",
        base_url, owner, repo, oid, expires_at, signature
    );

    Ok(LfsObjectResponse {
        oid: oid.to_string(),
        size,
        actions: Some(LfsActions {
            download: Some(LfsAction {
                href: download_href,
                header: None,
                expires_in: Some(DOWNLOAD_URL_TTL_SECONDS),
            }),
            upload: None,
        }),
        error: None,
    })
}

/// Store an uploaded LFS object to disk and mark as uploaded in DB.
pub async fn store_object(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    owner: &str,
    repo: &str,
    oid: &str,
    data: &[u8],
) -> Result<()> {
    // Find or create the DB record first
    let existing = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await?;
    let _obj_id = if let Some(obj) = existing {
        obj.id
    } else {
        let model = lfs_object::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: sea_orm::Set(repo_id),
            oid: sea_orm::Set(oid.to_string()),
            size: sea_orm::Set(data.len() as i64),
            uploaded: sea_orm::Set(false),
            compression: sea_orm::Set(None),
            compressed_size: sea_orm::Set(None),
            created_at: sea_orm::Set(Utc::now()),
        };
        let new_obj = lfs_object_ops::create(db, model).await?;
        new_obj.id
    };

    // Compress data with zstd
    let compressed = compress_data(data)?;
    let compressed_size = compressed.len() as i64;

    let key = lfs_object_key(owner, repo, oid, true)?;
    storage.put(&key, &compressed).await?;

    tracing::info!(
        oid = %oid,
        original_size = data.len(),
        compressed_size = compressed_size,
        ratio = format!("{:.1}%", (compressed_size as f64 / data.len() as f64) * 100.0),
        "LFS object compressed and stored"
    );

    // Update DB with compression info and mark as uploaded
    let obj = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid)
        .await?
        .ok_or_else(|| anyhow::anyhow!("LFS object {} not found after create", oid))?;

    let mut model: lfs_object::ActiveModel = obj.into();
    model.uploaded = sea_orm::Set(true);
    model.compression = sea_orm::Set(Some(COMPRESSION_ALGO.to_string()));
    model.compressed_size = sea_orm::Set(Some(compressed_size));
    if let Err(error) = model.update(db).await {
        let _ = storage.delete(&key).await;
        return Err(error).context("db: update LFS object after store");
    }

    Ok(())
}

/// Store an LFS object from an uncompressed file on disk.
/// Streams the file through zstd compression—never loads the entire
/// object into memory.
pub async fn store_object_from_file(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    owner: &str,
    repo: &str,
    oid: &str,
    uncompressed_path: &std::path::Path,
    original_size: i64,
) -> Result<()> {
    // Find or create the DB record first
    let existing = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await?;
    let _obj_id = if let Some(_obj) = existing {
        None
    } else {
        let model = lfs_object::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: sea_orm::Set(repo_id),
            oid: sea_orm::Set(oid.to_string()),
            size: sea_orm::Set(original_size),
            uploaded: sea_orm::Set(false),
            compression: sea_orm::Set(None),
            compressed_size: sea_orm::Set(None),
            created_at: sea_orm::Set(Utc::now()),
        };
        let new_obj = lfs_object_ops::create(db, model).await?;
        Some(new_obj.id)
    };

    // Stream-compress from file (uses chunked I/O, not full file read)
    let compressed_path = uncompressed_path.with_extension(format!("{}.zst", uuid::Uuid::new_v4()));
    let src_file = std::fs::File::open(uncompressed_path)
        .with_context(|| format!("open uncompressed file {:?}", uncompressed_path))?;
    let dst_file = std::fs::File::create(&compressed_path)
        .with_context(|| format!("create compressed file {:?}", compressed_path))?;

    let mut encoder = zstd::stream::Encoder::new(dst_file, ZSTD_LEVEL)
        .context("failed to create zstd stream encoder")?;
    std::io::copy(&mut std::io::BufReader::new(src_file), &mut encoder)
        .context("failed to stream-compress LFS object")?;
    let finished = encoder
        .finish()
        .context("failed to finish zstd stream encoding")?;

    let compressed_size = finished.metadata().map(|m| m.len() as i64).unwrap_or(0);

    let key = lfs_object_key(owner, repo, oid, true)?;
    if let Err(error) = storage.put_file(&key, &compressed_path).await {
        let _ = std::fs::remove_file(&compressed_path);
        let _ = std::fs::remove_file(uncompressed_path);
        return Err(error.into());
    }

    // Remove uncompressed temp file
    let _ = std::fs::remove_file(uncompressed_path);
    let _ = std::fs::remove_file(&compressed_path);

    tracing::info!(
        oid = %oid,
        original_size = original_size,
        compressed_size = compressed_size,
        ratio = format!("{:.1}%", if original_size > 0 { (compressed_size as f64 / original_size as f64) * 100.0 } else { 0.0 }),
        "LFS object stream-compressed and stored"
    );

    // Update DB with compression info and mark as uploaded
    let obj = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid)
        .await?
        .ok_or_else(|| anyhow::anyhow!("LFS object {} not found after create", oid))?;

    let mut model: lfs_object::ActiveModel = obj.into();
    model.uploaded = sea_orm::Set(true);
    model.compression = sea_orm::Set(Some(COMPRESSION_ALGO.to_string()));
    model.compressed_size = sea_orm::Set(Some(compressed_size));
    if let Err(error) = model.update(db).await {
        let _ = storage.delete(&key).await;
        return Err(error).context("db: update LFS object after store");
    }

    Ok(())
}

/// Read an LFS object from disk.
pub async fn read_object(lfs_root: &std::path::Path, oid: &str) -> Result<Vec<u8>> {
    let obj_path = lfs_object_path(lfs_root, oid);

    // Try compressed version first (.zst)
    let compressed_path = obj_path.with_extension("zst");
    if compressed_path.exists() {
        let compressed = std::fs::read(&compressed_path)
            .with_context(|| format!("read compressed LFS object {:?}", compressed_path))?;
        return decompress_data(&compressed);
    }

    // Fallback to uncompressed (legacy)
    if obj_path.exists() {
        return std::fs::read(&obj_path).with_context(|| format!("read LFS object {:?}", obj_path));
    }

    anyhow::bail!("LFS object {} not found", oid)
}

/// Get the file paths needed for streaming an LFS object.
/// Returns `(file_path, is_compressed)` — the caller should stream-decompress
/// if `is_compressed` is true.
pub fn read_object_path(lfs_root: &std::path::Path, oid: &str) -> Result<(PathBuf, bool)> {
    let obj_path = lfs_object_path(lfs_root, oid);

    // Try compressed version first (.zst)
    let compressed_path = obj_path.with_extension("zst");
    if compressed_path.exists() {
        return Ok((compressed_path, true));
    }

    // Fallback to uncompressed (legacy)
    if obj_path.exists() {
        return Ok((obj_path, false));
    }

    anyhow::bail!("LFS object {} not found", oid)
}

/// Backend-neutral download source. Local storage retains streaming file I/O;
/// remote backends may return bytes until STORAGE-002 adds signed/streamed reads.
pub enum LfsObjectSource {
    Local { path: PathBuf, compressed: bool },
    Bytes { data: Vec<u8>, compressed: bool },
}

pub async fn read_object_source(
    storage: &dyn BlobStorage,
    legacy_lfs_root: &std::path::Path,
    owner: &str,
    repo: &str,
    oid: &str,
) -> Result<LfsObjectSource> {
    for compressed in [true, false] {
        let key = lfs_object_key(owner, repo, oid, compressed)?;
        if storage.exists(&key).await? {
            if let Some(path) = storage.local_path(&key) {
                return Ok(LfsObjectSource::Local { path, compressed });
            }
            return Ok(LfsObjectSource::Bytes {
                data: storage.get(&key).await?,
                compressed,
            });
        }
    }

    let (path, compressed) = read_object_path(legacy_lfs_root, oid)?;
    Ok(LfsObjectSource::Local { path, compressed })
}

// ── Compression helpers ───────────────────────────────────────────────────────

/// Compress data using zstd.
fn compress_data(data: &[u8]) -> Result<Vec<u8>> {
    let mut compressed = Vec::with_capacity(data.len());
    let mut encoder =
        zstd::Encoder::new(&mut compressed, ZSTD_LEVEL).context("failed to create zstd encoder")?;
    encoder
        .write_all(data)
        .context("failed to write data to zstd encoder")?;
    encoder.finish().context("failed to finish zstd encoding")?;
    Ok(compressed)
}

/// Decompress zstd data.
fn decompress_data(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decompressed = Vec::new();
    let mut decoder = zstd::Decoder::new(compressed).context("failed to create zstd decoder")?;
    std::io::copy(&mut decoder, &mut decompressed).context("failed to decompress zstd data")?;
    Ok(decompressed)
}

// ── Lazy compression utility ──────────────────────────────────────────────────

/// Compress existing uncompressed LFS objects in a repository.
/// Returns the number of objects compressed.
pub async fn compress_existing(
    db: &DatabaseConnection,
    repo_id: i64,
    lfs_root: &std::path::Path,
    batch_size: u64,
) -> Result<usize> {
    let uncompressed = lfs_object_ops::list_uncompressed(db, repo_id, batch_size).await?;
    let mut count = 0;

    for obj in uncompressed {
        let obj_path = lfs_object_path(lfs_root, &obj.oid);

        // Skip if already compressed
        if obj_path.with_extension("zst").exists() {
            continue;
        }

        // Skip if original file doesn't exist
        if !obj_path.exists() {
            tracing::warn!(oid = %obj.oid, "LFS object file not found, skipping");
            continue;
        }

        // Read and compress
        match std::fs::read(&obj_path) {
            Ok(data) => {
                let compressed = match compress_data(&data) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!(oid = %obj.oid, err = %e, "failed to compress LFS object");
                        continue;
                    }
                };

                let compressed_path = obj_path.with_extension("zst");
                if let Err(e) = std::fs::write(&compressed_path, &compressed) {
                    tracing::error!(oid = %obj.oid, err = %e, "failed to write compressed file");
                    continue;
                }

                // Update DB
                if let Err(e) = lfs_object_ops::update_compression(
                    db,
                    obj.id,
                    COMPRESSION_ALGO,
                    compressed.len() as i64,
                )
                .await
                {
                    tracing::error!(oid = %obj.oid, err = %e, "failed to update DB");
                    // Clean up compressed file on DB error
                    let _ = std::fs::remove_file(&compressed_path);
                    continue;
                }

                // Remove original uncompressed file
                if let Err(e) = std::fs::remove_file(&obj_path) {
                    tracing::warn!(oid = %obj.oid, err = %e, "failed to remove original file");
                }

                tracing::info!(
                    oid = %obj.oid,
                    original = data.len(),
                    compressed = compressed.len(),
                    ratio = format!("{:.1}%", (compressed.len() as f64 / data.len() as f64) * 100.0),
                    "compressed existing LFS object"
                );
                count += 1;
            }
            Err(e) => {
                tracing::error!(oid = %obj.oid, err = %e, "failed to read original file");
            }
        }
    }

    Ok(count)
}

/// Delete an LFS object from disk and DB.
pub async fn delete_object(
    db: &DatabaseConnection,
    repo_id: i64,
    lfs_root: &std::path::Path,
    oid: &str,
) -> Result<()> {
    let obj_path = lfs_object_path(lfs_root, oid);

    // Delete compressed version
    let compressed_path = obj_path.with_extension("zst");
    if compressed_path.exists() {
        std::fs::remove_file(&compressed_path)
            .with_context(|| format!("delete compressed LFS object {:?}", compressed_path))?;
    }

    // Delete uncompressed version (legacy)
    if obj_path.exists() {
        std::fs::remove_file(&obj_path)
            .with_context(|| format!("delete LFS object {:?}", obj_path))?;
    }

    // Delete from DB
    if let Some(obj) = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await? {
        lfs_object_ops::delete_by_id(db, obj.id).await?;
    }

    Ok(())
}

/// Delete both backend-neutral and historical local representations before
/// removing the database row.
pub async fn delete_object_from_storage(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    legacy_lfs_root: &std::path::Path,
    owner: &str,
    repo: &str,
    oid: &str,
) -> Result<()> {
    for compressed in [true, false] {
        let key = lfs_object_key(owner, repo, oid, compressed)?;
        storage.delete(&key).await?;
    }

    let legacy = lfs_object_path(legacy_lfs_root, oid);
    for path in [legacy.with_extension("zst"), legacy] {
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("delete legacy LFS object {path:?}"))?;
        }
    }

    if let Some(obj) = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await? {
        lfs_object_ops::delete_by_id(db, obj.id).await?;
    }
    Ok(())
}
