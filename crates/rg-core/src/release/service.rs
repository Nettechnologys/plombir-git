//! Release service — business logic for releases.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use rg_db::{
    entities::release::{ActiveModel as ReleaseActiveModel, Model as Release},
    entities::release_asset::{ActiveModel as AssetActiveModel, Model as Asset},
};

/// Create a new release.
#[allow(clippy::too_many_arguments)]
pub async fn create_release(
    db: &DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    tag_name: &str,
    title: &str,
    body: Option<&str>,
    target_commitish: &str,
    is_draft: bool,
    is_prerelease: bool,
    _repo_path: &std::path::Path,
) -> Result<Release> {
    // Validate inputs
    if tag_name.is_empty() {
        anyhow::bail!("tag_name cannot be empty");
    }
    if title.is_empty() {
        anyhow::bail!("title cannot be empty");
    }

    // Check for duplicate tag
    if rg_db::ops::release_ops::find_by_repo_and_tag(db, repo_id, tag_name)
        .await?
        .is_some()
    {
        anyhow::bail!("release with tag '{}' already exists", tag_name);
    }

    let now = Utc::now();
    let model = ReleaseActiveModel {
        repo_id: Set(repo_id),
        author_id: Set(author_id),
        tag_name: Set(tag_name.to_string()),
        title: Set(title.to_string()),
        body: Set(body.map(str::to_string)),
        target_commitish: Set(target_commitish.to_string()),
        is_draft: Set(is_draft),
        is_prerelease: Set(is_prerelease),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let release = rg_db::ops::release_ops::create(db, model).await?;

    // Trigger release.created webhook
    let payload = serde_json::json!({
        "id": release.id,
        "repo_id": release.repo_id,
        "tag_name": release.tag_name,
        "title": release.title,
        "body": release.body,
        "is_draft": release.is_draft,
        "is_prerelease": release.is_prerelease,
        "author_id": release.author_id,
    });
    if let Err(e) = crate::webhook::service::trigger_release_created(db, repo_id, &payload).await {
        tracing::warn!(error = %format!("{e:#}"), "failed to trigger release.created webhook");
    }

    Ok(release)
}

/// List releases for a repository.
pub async fn list_releases(
    db: &DatabaseConnection,
    repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Release>, i64)> {
    rg_db::ops::release_ops::list_by_repo(db, repo_id, offset, limit).await
}

/// Get a release by ID.
///
/// The miss is typed (`rg_core::error::NotFound`) so the HTTP layer can tell it
/// apart from a failed lookup: a database outage here must not be reported to
/// the client as "the release was deleted".
pub async fn get_release(db: &DatabaseConnection, id: i64) -> Result<Release> {
    rg_db::ops::release_ops::find_by_id(db, id)
        .await?
        .ok_or_else(|| crate::error::not_found("release"))
}

/// Update a release.
#[allow(clippy::too_many_arguments)]
pub async fn update_release(
    db: &DatabaseConnection,
    id: i64,
    title: Option<&str>,
    body: Option<&str>,
    is_draft: Option<bool>,
    is_prerelease: Option<bool>,
) -> Result<Release> {
    let existing = rg_db::ops::release_ops::find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("release not found"))?;

    let mut model: ReleaseActiveModel = existing.into();
    if let Some(t) = title {
        model.title = Set(t.to_string());
    }
    if let Some(b) = body {
        model.body = Set(Some(b.to_string()));
    }
    if let Some(d) = is_draft {
        model.is_draft = Set(d);
    }
    if let Some(p) = is_prerelease {
        model.is_prerelease = Set(p);
    }
    model.updated_at = Set(Utc::now());

    rg_db::ops::release_ops::update(db, model).await
}

/// Delete a release.
pub async fn delete_release(db: &DatabaseConnection, id: i64) -> Result<()> {
    // Get release info for webhook before deleting
    let release = rg_db::ops::release_ops::find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("release not found"))?;
    let repo_id = release.repo_id;

    rg_db::ops::release_ops::delete_by_id(db, id).await?;

    // Trigger release.deleted webhook
    let payload = serde_json::json!({
        "id": release.id,
        "repo_id": release.repo_id,
        "tag_name": release.tag_name,
        "title": release.title,
    });
    if let Err(e) = crate::webhook::service::trigger_release_deleted(db, repo_id, &payload).await {
        tracing::warn!(error = %format!("{e:#}"), "failed to trigger release.deleted webhook");
    }

    Ok(())
}

/// Upload a release asset (saves file to disk + creates DB record).
#[allow(clippy::too_many_arguments)]
pub async fn upload_asset(
    db: &DatabaseConnection,
    release_id: i64,
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
    filename: &str,
    size: i64,
    content_type: &str,
    uploader_id: i64,
    data: &[u8],
) -> Result<Asset> {
    // Verify release exists and get repo info
    let _release = get_release(db, release_id).await?;

    // Content digest for integrity + provenance, recorded at upload time and
    // re-checked on every download. Mirrors the package-registry idiom.
    let sha256 = hex::encode(Sha256::digest(data));

    // Create DB record first to get asset ID
    let model = AssetActiveModel {
        release_id: Set(release_id),
        filename: Set(filename.to_string()),
        size: Set(size),
        content_type: Set(content_type.to_string()),
        download_count: Set(0),
        uploader_id: Set(uploader_id),
        created_at: Set(Utc::now()),
        sha256: Set(Some(sha256)),
        ..Default::default()
    };
    let asset = rg_db::ops::release_ops::create_asset(db, model).await?;

    // The key is derived from the row that was just inserted, so a failure here
    // leaves the same orphan metadata row a failed `put` would — roll it back on
    // both paths, not only the one that was noticed first.
    let key = match asset_blob_key(owner, repo_name, &asset) {
        Ok(key) => key,
        Err(error) => {
            let _ = rg_db::ops::release_ops::delete_asset_by_id(db, asset.id).await;
            return Err(error);
        }
    };
    if let Err(error) = storage.put(&key, data).await {
        let _ = rg_db::ops::release_ops::delete_asset_by_id(db, asset.id).await;
        return Err(error).context("failed to write release asset");
    }

    // Keep the parameter during the compatibility window: old assets are read
    // from this root, while all new writes use backend-neutral keys.
    let _ = repo_root;

    Ok(asset)
}

/// Download a release asset (increments download count, returns file bytes).
pub async fn download_asset(
    db: &DatabaseConnection,
    asset_id: i64,
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
) -> Result<(Asset, Vec<u8>)> {
    let asset = rg_db::ops::release_ops::find_asset_by_id(db, asset_id)
        .await?
        .ok_or_else(|| crate::error::not_found("asset"))?;

    // Increment download count
    rg_db::ops::release_ops::increment_download_count(db, asset_id).await?;

    let data = read_asset_bytes(storage, repo_root, owner, repo_name, &asset).await?;

    // Integrity check: the stored bytes must still hash to the digest recorded
    // at upload. Legacy assets (uploaded before digest tracking) carry no
    // recorded hash and are served without this guard.
    if let Some(expected) = asset.sha256.as_deref() {
        let actual = hex::encode(Sha256::digest(&data));
        if actual != expected {
            anyhow::bail!(
                "asset integrity check failed: expected sha256 {expected}, got {actual}"
            );
        }
    }

    Ok((asset, data))
}

/// Read an asset's bytes from blob storage, falling back to the legacy on-disk
/// path for assets written before the blob-storage migration.
async fn read_asset_bytes(
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
    asset: &Asset,
) -> Result<Vec<u8>> {
    let key = asset_blob_key(owner, repo_name, asset)?;
    match storage.get(&key).await {
        Ok(data) => Ok(data),
        Err(crate::blob_storage::BlobStorageError::NotFound(_)) => {
            // `asset_file_path` builds the path from `repo_root` and never
            // hands it back, so a bare io error names an asset file the
            // operator cannot locate.
            let file_path = asset_file_path(repo_root, owner, repo_name, asset);
            tokio::fs::read(&file_path).await.map_err(|error| {
                crate::platform::fs::path_error(
                    "legacy release asset",
                    &file_path,
                    &error,
                    crate::platform::fs::BLOB_STORAGE_HINT,
                )
            })
        }
        Err(error) => Err(error).context("failed to read release asset"),
    }
}

/// Result of verifying a stored asset attestation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AttestationReport {
    /// Whether the attestation verified against the instance key and the asset's
    /// current bytes.
    pub verified: bool,
    /// Failure detail when `verified` is false.
    pub reason: Option<String>,
    /// Predicate type of the verified statement.
    pub predicate_type: Option<String>,
    /// `kid` of the signature that verified.
    pub keyid: Option<String>,
    /// SHA-256 recomputed from the stored bytes at verification time.
    pub asset_sha256: String,
}

/// Sign a detached provenance attestation for an asset with the instance key
/// and store the DSSE envelope alongside the asset row (opt-in).
///
/// `builder_id` attributes the build to the issuing instance (its external URL).
/// Fails if the asset has no recorded SHA-256 (legacy upload) — there is nothing
/// to bind the attestation to.
pub async fn sign_asset_attestation(
    db: &DatabaseConnection,
    asset_id: i64,
    secret: &str,
    builder_id: &str,
) -> Result<(Asset, crate::attestation::Envelope)> {
    let asset = get_asset(db, asset_id).await?;
    let sha256 = asset.sha256.as_deref().ok_or_else(|| {
        crate::error::invalid_request(
            "asset has no recorded sha256 digest; re-upload to enable attestation",
        )
    })?;

    let predicate_extra = serde_json::json!({
        "release_id": asset.release_id,
        "uploader_id": asset.uploader_id,
        "created_at": asset.created_at.to_rfc3339(),
    });
    let envelope = crate::attestation::sign_asset_provenance(
        secret,
        &asset.filename,
        sha256,
        builder_id,
        predicate_extra,
    )?;

    let json = serde_json::to_string(&envelope).context("serialize attestation envelope")?;
    let updated = rg_db::ops::release_ops::set_asset_attestation(db, asset_id, Some(json)).await?;
    Ok((updated, envelope))
}

/// Fetch the stored attestation envelope (parsed) for an asset, if any.
pub async fn get_asset_attestation(
    db: &DatabaseConnection,
    asset_id: i64,
) -> Result<Option<crate::attestation::Envelope>> {
    let asset = get_asset(db, asset_id).await?;
    match asset.attestation.as_deref() {
        Some(json) => {
            let env = serde_json::from_str(json).context("parse stored attestation envelope")?;
            Ok(Some(env))
        }
        None => Ok(None),
    }
}

/// Verify an asset's stored attestation against the instance key and the asset's
/// *current* bytes. A tampered asset (bytes no longer matching the signed
/// subject digest) or an invalid signature yields `verified: false` — infra
/// errors (missing asset/attestation, unreadable bytes) are returned as `Err`.
pub async fn verify_asset_attestation(
    db: &DatabaseConnection,
    asset_id: i64,
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
    secret: &str,
) -> Result<AttestationReport> {
    let asset = get_asset(db, asset_id).await?;
    let json = asset
        .attestation
        .as_deref()
        .ok_or_else(|| crate::error::not_found("attestation"))?;
    let envelope: crate::attestation::Envelope =
        serde_json::from_str(json).context("parse stored attestation envelope")?;

    let data = read_asset_bytes(storage, repo_root, owner, repo_name, &asset).await?;
    let actual_sha = hex::encode(Sha256::digest(&data));

    let registry = crate::attestation::VerifierRegistry::with_defaults();
    match crate::attestation::verify_envelope(secret, &envelope, &actual_sha, &registry) {
        Ok(v) => Ok(AttestationReport {
            verified: true,
            reason: None,
            predicate_type: Some(v.statement.predicate_type),
            keyid: Some(v.keyid),
            asset_sha256: actual_sha,
        }),
        Err(e) => Ok(AttestationReport {
            verified: false,
            reason: Some(format!("{e:#}")),
            predicate_type: None,
            keyid: None,
            asset_sha256: actual_sha,
        }),
    }
}

/// Get a release asset by ID (without incrementing download count).
pub async fn get_asset(db: &DatabaseConnection, asset_id: i64) -> Result<Asset> {
    rg_db::ops::release_ops::find_asset_by_id(db, asset_id)
        .await?
        .ok_or_else(|| crate::error::not_found("asset"))
}

/// List assets for a release.
pub async fn list_assets(db: &DatabaseConnection, release_id: i64) -> Result<Vec<Asset>> {
    rg_db::ops::release_ops::list_assets(db, release_id).await
}

/// Delete a release asset (removes DB record + file from disk).
pub async fn delete_asset(
    db: &DatabaseConnection,
    asset_id: i64,
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
) -> Result<()> {
    let asset = get_asset(db, asset_id).await?;

    let key = asset_blob_key(owner, repo_name, &asset)?;
    if let Err(error) = storage.delete(&key).await {
        tracing::warn!(%key, %error, "failed to remove release asset blob");
    }

    // Remove file from disk (ignore errors if file doesn't exist)
    let file_path = asset_file_path(repo_root, owner, repo_name, &asset);
    if let Err(e) = tokio::fs::remove_file(&file_path).await {
        tracing::warn!("Failed to remove asset file {}: {e}", file_path.display());
    }

    // Remove parent directory if empty
    if let Some(parent) = file_path.parent() {
        if let Err(e) = tokio::fs::remove_dir(parent).await {
            tracing::warn!("Failed to remove asset directory {}: {e}", parent.display());
        }
    }

    // Delete DB record
    rg_db::ops::release_ops::delete_asset_by_id(db, asset_id).await?;

    Ok(())
}

/// Get the storage directory for release assets.
fn asset_storage_dir(repo_root: &Path, owner: &str, repo_name: &str) -> PathBuf {
    repo_root.join(format!("{}/{}.releases/assets", owner, repo_name))
}

/// Get the file path for a specific asset.
fn asset_file_path(repo_root: &Path, owner: &str, repo_name: &str, asset: &Asset) -> PathBuf {
    asset_storage_dir(repo_root, owner, repo_name)
        .join(asset.id.to_string())
        .join(&asset.filename)
}

fn asset_blob_key(
    owner: &str,
    repo_name: &str,
    asset: &Asset,
) -> Result<crate::blob_storage::BlobKey> {
    let release_id = asset.release_id.to_string();
    let asset_id = asset.id.to_string();
    crate::blob_storage::BlobKey::from_segments([
        "releases",
        owner,
        repo_name,
        &release_id,
        &asset_id,
        &asset.filename,
    ])
    .map_err(Into::into)
}
