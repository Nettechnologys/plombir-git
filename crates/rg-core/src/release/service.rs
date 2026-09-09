//! Release service — business logic for releases.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

use rg_db::{
    entities::release::{ActiveModel as ReleaseActiveModel, Model as Release},
    entities::release_asset::{ActiveModel as AssetActiveModel, Model as Asset},
};

use crate::blob_storage::{BlobKey, BlobStorage};
use crate::deletion_recovery;

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
        return Err(crate::error::invalid_request("tag_name cannot be empty"));
    }
    if title.is_empty() {
        return Err(crate::error::invalid_request("title cannot be empty"));
    }

    // The one answer both the pre-read and a losing insert give, so a caller
    // cannot tell which of the two noticed — and so no constraint text leaks.
    //
    // `Conflict`, not `InvalidRequest`: the tag is well-formed (the empty-tag
    // check above is the request's own fault and stays a 400) and an existing
    // release refuses it. Editing the request cannot help; publishing under
    // another tag, or deleting that release, can.
    let already_exists =
        || crate::error::conflict(format!("release with tag '{tag_name}' already exists"));

    // Check for duplicate tag. This read is the fast path only — the row can
    // still appear between here and the insert below, which is why the insert
    // classifies its own failure rather than trusting this answer.
    if rg_db::ops::release_ops::find_by_repo_and_tag(db, repo_id, tag_name)
        .await?
        .is_some()
    {
        return Err(already_exists());
    }

    let now = Utc::now();
    let model = ReleaseActiveModel {
        repo_id: Set(repo_id),
        author_id: Set(Some(author_id)),
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

    // Losing the `idx_releases_repo_tag_unique` race is the same outcome the
    // read above reports, reached a moment later: someone else published the
    // tag first. Only that one loss is folded — any other write failure stays
    // an error.
    let release = match rg_db::ops::release_ops::create(db, model).await {
        Ok(release) => release,
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => return Err(already_exists()),
        Err(error) => return Err(error),
    };

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
    update_release_after_read(db, id, title, body, is_draft, is_prerelease, || {
        std::future::ready(Ok(()))
    })
    .await
}

/// Testable boundary between the identity read and the conditional write.
#[allow(clippy::too_many_arguments)]
async fn update_release_after_read<F, Fut>(
    db: &DatabaseConnection,
    id: i64,
    title: Option<&str>,
    body: Option<&str>,
    is_draft: Option<bool>,
    is_prerelease: Option<bool>,
    after_read: F,
) -> Result<Release>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let existing = rg_db::ops::release_ops::find_by_id(db, id)
        .await?
        .ok_or_else(|| crate::error::not_found("release"))?;
    after_read().await?;

    rg_db::ops::release_ops::update(
        db,
        existing.id,
        title.map(str::to_string),
        body.map(str::to_string),
        is_draft,
        is_prerelease,
        Utc::now(),
    )
    .await?
    .ok_or_else(|| crate::error::not_found("release"))
}

#[derive(Debug)]
struct StagedReleaseBlob {
    live: BlobKey,
    staged: BlobKey,
}

#[derive(Debug)]
struct StagedLegacyAsset {
    asset_id: i64,
    live: PathBuf,
    staged: PathBuf,
}

#[derive(Debug, Default)]
struct ReleaseDeletionStaging {
    blob: Option<StagedReleaseBlob>,
    legacy: Vec<StagedLegacyAsset>,
    /// The id the journal entry of this deletion is filed under. Empty only for
    /// the default value nothing ever stages through.
    deletion_id: String,
}

impl ReleaseDeletionStaging {
    async fn prepare(
        storage: &dyn BlobStorage,
        blob: StagedReleaseBlob,
        legacy: Vec<StagedLegacyAsset>,
        deletion_kind: &'static str,
        item_id: i64,
        deletion_id: &str,
    ) -> Result<Self> {
        // Declared before the first move, and complete: both halves of what
        // this deletion is about to take out of the live namespace are already
        // named by the caller, so a stop between any two renames leaves a
        // record of every one of them rather than of the ones that got far
        // enough to be pushed onto a vector.
        let mut journal = vec![deletion_recovery::StagedBytes::blob_prefix(
            &blob.live,
            &blob.staged,
        )];
        for asset in &legacy {
            journal.push(deletion_recovery::StagedBytes::path(
                &asset.live,
                &asset.staged,
            )?);
        }
        deletion_recovery::open(storage, deletion_id, deletion_kind, journal).await?;

        let mut staging = Self {
            deletion_id: deletion_id.to_string(),
            ..Self::default()
        };
        match storage.move_prefix(&blob.live, &blob.staged).await {
            Ok(true) => staging.blob = Some(blob),
            Ok(false) => {}
            Err(error) => {
                deletion_recovery::close(storage, deletion_id).await;
                return Err(error).with_context(|| {
                    format!(
                        "failed to stage {deletion_kind} blob prefix {} at {}",
                        blob.live, blob.staged
                    )
                });
            }
        }

        for path in legacy {
            let exists = match tokio::fs::try_exists(&path.live).await {
                Ok(exists) => exists,
                Err(error) => {
                    staging.restore(storage, deletion_kind, item_id).await;
                    return Err(crate::platform::fs::path_error(
                        "legacy release asset directory",
                        &path.live,
                        &error,
                        crate::platform::fs::BLOB_STORAGE_HINT,
                    ));
                }
            };
            if !exists {
                continue;
            }
            if let Err(error) = tokio::fs::rename(&path.live, &path.staged).await {
                staging.restore(storage, deletion_kind, item_id).await;
                return Err(crate::platform::fs::path_error(
                    "legacy release asset directory",
                    &path.live,
                    &error,
                    crate::platform::fs::BLOB_STORAGE_HINT,
                ))
                .with_context(|| {
                    format!(
                        "failed to stage legacy release asset {} at {}",
                        path.asset_id,
                        path.staged.display()
                    )
                });
            }
            staging.legacy.push(path);
        }

        Ok(staging)
    }

    async fn restore(&self, storage: &dyn BlobStorage, deletion_kind: &'static str, item_id: i64) {
        self.restore_representations(storage, deletion_kind, item_id)
            .await;
        deletion_recovery::close(storage, &self.deletion_id).await;
    }

    async fn restore_representations(
        &self,
        storage: &dyn BlobStorage,
        deletion_kind: &'static str,
        item_id: i64,
    ) {
        for path in self.legacy.iter().rev() {
            if let Err(error) = tokio::fs::rename(&path.staged, &path.live).await {
                tracing::warn!(
                    deletion_kind,
                    item_id,
                    asset_id = path.asset_id,
                    staged_at = %path.staged.display(),
                    belongs_at = %path.live.display(),
                    %error,
                    "failed to restore a legacy release asset after deletion aborted — live metadata may now point at missing bytes until the directory is moved back by hand"
                );
            }
        }
        if let Some(blob) = &self.blob {
            match storage.move_prefix(&blob.staged, &blob.live).await {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    deletion_kind,
                    item_id,
                    staged_prefix = %blob.staged,
                    live_prefix = %blob.live,
                    "failed to restore release blobs after deletion aborted — the staged prefix disappeared and live metadata may now point at missing bytes"
                ),
                Err(error) => tracing::warn!(
                    deletion_kind,
                    item_id,
                    staged_prefix = %blob.staged,
                    live_prefix = %blob.live,
                    %error,
                    "failed to restore release blobs after deletion aborted — live metadata cannot reach them until the prefix is moved back by hand"
                ),
            }
        }
    }

    async fn retire(
        self,
        storage: &dyn BlobStorage,
        deletion_kind: &'static str,
        item_id: i64,
    ) -> Result<()> {
        let mut cleanup_error = None;

        // Before the first removal: this marker is what stops a startup pass
        // from putting these bytes back into a namespace whose metadata is
        // already gone. See `StagedPackageVersion::retire` for why a failure
        // here is reported but does not hold up the retirement.
        if let Err(error) = deletion_recovery::mark_committed(storage, &self.deletion_id).await {
            tracing::warn!(
                deletion_kind,
                item_id,
                error = %format!("{error:#}"),
                "release metadata is deleted, but the deletion could not be marked committed"
            );
            cleanup_error = Some(error);
        }

        for path in self.legacy {
            match tokio::fs::remove_dir_all(&path.staged).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(
                        deletion_kind,
                        item_id,
                        asset_id = path.asset_id,
                        staged_at = %path.staged.display(),
                        %error,
                        "release metadata is deleted, but staged legacy asset bytes remain and must be removed by hand"
                    );
                    if cleanup_error.is_none() {
                        cleanup_error = Some(
                            crate::platform::fs::path_error(
                                "staged legacy release asset directory",
                                &path.staged,
                                &error,
                                crate::platform::fs::BLOB_STORAGE_HINT,
                            )
                            .context("failed to retire deleted legacy release asset"),
                        );
                    }
                }
            }
        }

        if let Some(blob) = self.blob {
            if let Err(error) = storage.delete_prefix(&blob.staged).await {
                tracing::warn!(
                    deletion_kind,
                    item_id,
                    staged_prefix = %blob.staged,
                    live_prefix = %blob.live,
                    %error,
                    "release metadata is deleted and its live blob namespace is free, but staged blobs remain and must be removed by hand"
                );
                if cleanup_error.is_none() {
                    cleanup_error = Some(anyhow::Error::new(error).context(format!(
                        "failed to retire staged release blobs at {}",
                        blob.staged
                    )));
                }
            }
        }

        // Closed only when nothing is left staged — see
        // `StagedPackageVersion::retire` for why a failed removal keeps the
        // entry rather than dropping it.
        if cleanup_error.is_none() {
            deletion_recovery::close(storage, &self.deletion_id).await;
        }

        cleanup_error.map_or(Ok(()), Err)
    }
}

fn release_blob_prefix(owner: &str, repo_name: &str, release_id: i64) -> Result<BlobKey> {
    let release_id = release_id.to_string();
    BlobKey::from_segments(["releases", owner, repo_name, release_id.as_str()]).map_err(Into::into)
}

fn asset_blob_prefix(owner: &str, repo_name: &str, asset: &Asset) -> Result<BlobKey> {
    let release_id = asset.release_id.to_string();
    let asset_id = asset.id.to_string();
    BlobKey::from_segments([
        "releases",
        owner,
        repo_name,
        release_id.as_str(),
        asset_id.as_str(),
    ])
    .map_err(Into::into)
}

fn staged_release_blob_prefix(
    deletion_kind: &str,
    item_id: i64,
    deletion_id: &str,
) -> Result<BlobKey> {
    let item_id = item_id.to_string();
    BlobKey::from_segments([
        "_deleted",
        "release-deletions",
        deletion_kind,
        item_id.as_str(),
        deletion_id,
    ])
    .map_err(Into::into)
}

fn legacy_asset_staging(
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
    assets: &[Asset],
    deletion_id: &str,
) -> Vec<StagedLegacyAsset> {
    assets
        .iter()
        .map(|asset| {
            let live = asset_storage_dir(repo_root, owner, repo_name).join(asset.id.to_string());
            let staged = live.with_file_name(format!("{}.deleted-{deletion_id}", asset.id));
            StagedLegacyAsset {
                asset_id: asset.id,
                live,
                staged,
            }
        })
        .collect()
}

/// Delete a release and every asset representation it owns.
#[allow(clippy::too_many_arguments)]
pub async fn delete_release(
    db: &DatabaseConnection,
    id: i64,
    storage: &dyn BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
) -> Result<()> {
    // Get release info for webhook before deleting
    let release = rg_db::ops::release_ops::find_by_id(db, id)
        .await?
        .ok_or_else(|| crate::error::not_found("release"))?;
    let repo_id = release.repo_id;

    let assets = rg_db::ops::release_ops::list_assets(db, release.id).await?;
    let deletion_id = uuid::Uuid::new_v4().simple().to_string();
    let staging = ReleaseDeletionStaging::prepare(
        storage,
        StagedReleaseBlob {
            live: release_blob_prefix(owner, repo_name, release.id)?,
            staged: staged_release_blob_prefix("release", release.id, &deletion_id)?,
        },
        legacy_asset_staging(repo_root, owner, repo_name, &assets, &deletion_id),
        "release",
        release.id,
        &deletion_id,
    )
    .await?;

    match rg_db::ops::release_ops::delete_by_id(db, id).await {
        Ok(true) => {}
        // The lookup and this statement are separate, so a concurrent delete can
        // take the row in between. That request owns the deletion — it fires the
        // webhook and retires its own staging — so this one must not report a
        // success it did not perform. The staged blobs are still retired rather
        // than restored: the release row is gone either way, and putting the
        // bytes back would leave them with nothing pointing at them.
        Ok(false) => {
            staging.retire(storage, "release", release.id).await?;
            return Err(crate::error::not_found("release"));
        }
        Err(error) => {
            staging.restore(storage, "release", release.id).await;
            return Err(error).context("failed to delete release metadata");
        }
    }

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

    staging.retire(storage, "release", release.id).await
}

/// Roll back the metadata row inserted before the blob was written.
///
/// Compensation on an error path: the caller must still see the original
/// failure, so a failed rollback can only be reported. When it fails the row
/// survives describing an asset whose bytes were never stored, and every later
/// download of it answers "blob not found".
async fn warn_orphan_asset_row(db: &DatabaseConnection, asset: &Asset, cause: &str) {
    if let Err(cleanup_error) = rg_db::ops::release_ops::delete_asset_by_id(db, asset.id).await {
        tracing::warn!(
            asset_id = asset.id,
            release_id = asset.release_id,
            filename = %asset.filename,
            error = %format!("{cleanup_error:#}"),
            "orphaned release asset row: {cause} and the rollback delete failed too — the row now describes an asset with no bytes behind it"
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_asset_upload(
    db: &DatabaseConnection,
    release_id: i64,
    owner: &str,
    repo_name: &str,
    filename: &str,
    size: i64,
    content_type: &str,
    uploader_id: i64,
    sha256: String,
) -> Result<(Asset, BlobKey)> {
    // Verify release exists and get repo info
    let _release = get_release(db, release_id).await?;

    // Create DB record first to get asset ID
    let model = AssetActiveModel {
        release_id: Set(release_id),
        filename: Set(filename.to_string()),
        size: Set(size),
        content_type: Set(content_type.to_string()),
        download_count: Set(0),
        uploader_id: Set(Some(uploader_id)),
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
            warn_orphan_asset_row(db, &asset, "the asset key could not be built").await;
            return Err(error);
        }
    };

    Ok((asset, key))
}

/// Upload a release asset from a bounded staging file without materialising
/// the complete body in application memory.
#[allow(clippy::too_many_arguments)]
pub async fn upload_asset_from_file(
    db: &DatabaseConnection,
    release_id: i64,
    storage: &dyn crate::blob_storage::BlobStorage,
    owner: &str,
    repo_name: &str,
    filename: &str,
    content_type: &str,
    uploader_id: i64,
    source: &Path,
    size: u64,
) -> Result<Asset> {
    let actual_size = tokio::fs::metadata(source)
        .await
        .with_context(|| {
            format!(
                "failed to inspect release asset upload {}",
                source.display()
            )
        })?
        .len();
    if actual_size != size {
        anyhow::bail!(
            "release asset upload size changed before storage: expected {size}, got {actual_size}"
        );
    }
    let recorded_size = i64::try_from(size).context("release asset size exceeds database range")?;
    let sha256 = hash_release_asset_file(source)
        .await
        .context("failed to hash release asset upload")?;
    let (asset, key) = prepare_asset_upload(
        db,
        release_id,
        owner,
        repo_name,
        filename,
        recorded_size,
        content_type,
        uploader_id,
        sha256,
    )
    .await?;

    if let Err(error) = storage.put_file(&key, source).await {
        warn_orphan_asset_row(db, &asset, "writing the asset blob failed").await;
        return Err(error).context("failed to write release asset");
    }

    Ok(asset)
}

async fn hash_release_asset_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Where a release asset's bytes come from, resolved before the handler
/// decides how to serve them.
///
/// `LocalFile` is the case a local backend (or a legacy pre-migration file)
/// produces: the handler can hash and stream the payload straight off disk,
/// and the asset size sets a disk bound instead of a heap one. `Buffered` is
/// the remote-backend fallback — a blob store that exposes no local path has
/// to travel through memory once, and `LocalBlobStorage` never falls here.
pub enum AssetSource {
    LocalFile { path: PathBuf, size: u64 },
    Buffered(Vec<u8>),
}

/// Download a release asset: increments the download count and locates the
/// bytes without pulling them through memory when they already live on disk.
///
/// Integrity verification is the caller's — the handler hashes the returned
/// [`AssetSource`] itself, streaming for `LocalFile` and in-memory for the
/// `Buffered` fallback. Reading the whole payload into a `Vec` here just to
/// hash it (card_7e762d2c19b0) made every download cost the server the
/// asset's own size in heap, on the local backend that ships as the default.
pub async fn download_asset(
    db: &DatabaseConnection,
    asset_id: i64,
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
) -> Result<(Asset, AssetSource)> {
    let asset = rg_db::ops::release_ops::find_asset_by_id(db, asset_id)
        .await?
        .ok_or_else(|| crate::error::not_found("asset"))?;

    // Increment before locating the bytes, but only if the row still exists.
    // A DELETE that won after the read above is an ordinary missing asset, not
    // a backend-shaped update failure.
    if !rg_db::ops::release_ops::increment_download_count(db, asset_id).await? {
        return Err(crate::error::not_found("asset"));
    }

    let source = resolve_asset_source(storage, repo_root, owner, repo_name, &asset).await?;
    Ok((asset, source))
}

/// Locate an asset's bytes, preferring a zero-copy path off disk over pulling
/// the whole payload through memory.
///
/// The blob-storage backend answers first: a local backend exposes a
/// [`local_path`](crate::blob_storage::BlobStorage::local_path) the handler
/// can hash and stream straight off disk, and a remote backend falls back to
/// [`get`](crate::blob_storage::BlobStorage::get), which pulls the payload
/// through memory once. Assets uploaded before the blob-storage migration
/// live under `repo_root` in the historical layout — always local, always
/// returned as `LocalFile`.
pub async fn resolve_asset_source(
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
    asset: &Asset,
) -> Result<AssetSource> {
    let key = asset_blob_key(owner, repo_name, asset)?;
    if let Some(path) = storage.local_path(&key) {
        match tokio::fs::metadata(&path).await {
            Ok(meta) => {
                return Ok(AssetSource::LocalFile {
                    path,
                    size: meta.len(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Blob not there — try the legacy on-disk fallback below.
            }
            Err(error) => {
                return Err(crate::platform::fs::path_error(
                    "release asset",
                    &path,
                    &error,
                    crate::platform::fs::BLOB_STORAGE_HINT,
                ));
            }
        }
    } else {
        match storage.get(&key).await {
            Ok(data) => return Ok(AssetSource::Buffered(data)),
            Err(crate::blob_storage::BlobStorageError::NotFound(_)) => {
                // Blob not there — try the legacy on-disk fallback below.
            }
            Err(error) => return Err(error).context("failed to read release asset"),
        }
    }

    // `asset_file_path` builds the path from `repo_root` and never hands it
    // back, so a bare io error names an asset file the operator cannot locate.
    let file_path = asset_file_path(repo_root, owner, repo_name, asset);
    let meta = tokio::fs::metadata(&file_path).await.map_err(|error| {
        crate::platform::fs::path_error(
            "legacy release asset",
            &file_path,
            &error,
            crate::platform::fs::BLOB_STORAGE_HINT,
        )
    })?;
    Ok(AssetSource::LocalFile {
        path: file_path,
        size: meta.len(),
    })
}

/// Result of verifying a stored asset attestation.
///
/// There is deliberately no `verified: bool` here. A boolean has room for two
/// answers and this check has three: it held, it is contradicted, or this
/// instance could not reach a verdict at all. Collapsing the third into `false`
/// tells a reader that an asset's bytes no longer match what was signed —
/// the loudest thing this feature can say — because of a predicate type the
/// server does not happen to know (card_4579598691ce).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AttestationReport {
    /// Whether the attestation verified, is contradicted by the asset's current
    /// bytes, or could not be checked by this instance.
    pub status: crate::attestation::VerificationStatus,
    /// Detail behind a non-verified `status`; `None` when it verified.
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
    key: &crate::auth::instance_key::InstanceKey,
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
        key,
        &asset.filename,
        sha256,
        builder_id,
        predicate_extra,
    )?;

    let json = serde_json::to_string(&envelope).context("serialize attestation envelope")?;
    let updated = rg_db::ops::release_ops::set_asset_attestation(db, asset_id, Some(json))
        .await?
        .ok_or_else(|| crate::error::not_found("asset"))?;
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
/// *current* bytes.
///
/// A tampered asset (bytes no longer matching the signed subject digest) or a
/// signature that fails under this instance's own key yields
/// [`VerificationStatus::Mismatch`](crate::attestation::VerificationStatus::Mismatch);
/// an envelope this instance cannot interpret — unknown predicate type,
/// unreadable payload, key rotated away — yields `Undeterminable`, because none
/// of that observes the asset's bytes. Infra errors (missing asset/attestation,
/// unreadable bytes) are returned as `Err`.
pub async fn verify_asset_attestation(
    db: &DatabaseConnection,
    asset_id: i64,
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
    key: &crate::auth::instance_key::InstanceKey,
) -> Result<AttestationReport> {
    let asset = get_asset(db, asset_id).await?;
    let json = asset
        .attestation
        .as_deref()
        .ok_or_else(|| crate::error::not_found("attestation"))?;
    let envelope: crate::attestation::Envelope =
        serde_json::from_str(json).context("parse stored attestation envelope")?;

    // Hash the stored bytes without pulling them through memory when they
    // already live on disk (card_7e762d2c19b0): the local backend streams the
    // file through the hasher, and only the remote-backend fallback still
    // buffers.
    let source = resolve_asset_source(storage, repo_root, owner, repo_name, &asset).await?;
    let actual_sha = match &source {
        AssetSource::LocalFile { path, .. } => hash_release_asset_file(path)
            .await
            .with_context(|| format!("failed to hash release asset {}", path.display()))?,
        AssetSource::Buffered(data) => hex::encode(Sha256::digest(data)),
    };

    let registry = crate::attestation::VerifierRegistry::with_defaults();
    match crate::attestation::verify_envelope(key, &envelope, &actual_sha, &registry) {
        Ok(v) => Ok(AttestationReport {
            status: crate::attestation::VerificationStatus::Verified,
            reason: None,
            predicate_type: Some(v.statement.predicate_type),
            keyid: Some(v.keyid),
            asset_sha256: actual_sha,
        }),
        // The status comes from the verifier, never from re-reading its
        // message: "could not check" and "checked, and it does not hold" are
        // different answers to the caller, and a caller must not have to
        // recover which one it got by matching on prose.
        Err(e) => Ok(AttestationReport {
            status: e.status(),
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

/// Delete a release asset from both backend-neutral/legacy storage and the DB.
pub async fn delete_asset(
    db: &DatabaseConnection,
    asset_id: i64,
    storage: &dyn crate::blob_storage::BlobStorage,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
) -> Result<()> {
    let asset = get_asset(db, asset_id).await?;
    let deletion_id = uuid::Uuid::new_v4().simple().to_string();
    let staging = ReleaseDeletionStaging::prepare(
        storage,
        StagedReleaseBlob {
            live: asset_blob_prefix(owner, repo_name, &asset)?,
            staged: staged_release_blob_prefix("asset", asset.id, &deletion_id)?,
        },
        legacy_asset_staging(
            repo_root,
            owner,
            repo_name,
            std::slice::from_ref(&asset),
            &deletion_id,
        ),
        "release asset",
        asset.id,
        &deletion_id,
    )
    .await?;

    match rg_db::ops::release_ops::delete_asset_by_id(db, asset_id).await {
        Ok(true) => {}
        // Same split as `delete_release`: a concurrent delete that took the row
        // between the lookup and this statement owns the deletion, so this call
        // reports `not_found` instead of a 204 it did not earn. The staged bytes
        // are retired anyway — the row they belonged to is gone.
        Ok(false) => {
            staging.retire(storage, "release asset", asset.id).await?;
            return Err(crate::error::not_found("release asset"));
        }
        Err(error) => {
            staging.restore(storage, "release asset", asset.id).await;
            return Err(error).context("failed to delete release asset metadata");
        }
    }

    staging.retire(storage, "release asset", asset.id).await
}

/// The historical on-disk root of one repository's release assets.
///
/// Backend-neutral keys replaced this layout, but installations that predate
/// the migration still serve from it — [`resolve_asset_source`] falls back to it
/// whenever the blob store reports the key missing. That makes it
/// repository-owned storage bound to the `<owner>/<repo>` pair, so it has to
/// move with a transfer and be staged by a deletion like every other
/// namespace-bound directory. Both of those callers live in `repo::service`;
/// the path is spelled here once so a third copy cannot drift away from the
/// one the reader actually uses.
pub(crate) fn legacy_asset_root(repo_root: &Path, owner: &str, repo_name: &str) -> PathBuf {
    repo_root.join(format!("{owner}/{repo_name}.releases"))
}

/// Get the storage directory for release assets.
fn asset_storage_dir(repo_root: &Path, owner: &str, repo_name: &str) -> PathBuf {
    legacy_asset_root(repo_root, owner, repo_name).join("assets")
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

#[cfg(test)]
mod update_delete_tests {
    use super::*;
    use sea_orm::NotSet;

    async fn fixture() -> (tempfile::TempDir, DatabaseConnection, i64) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("release.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            4,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let owner = rg_db::ops::user_ops::create_user(
            &db,
            "release-race-owner",
            "release-race-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .expect("create owner");
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("release-race-repo".to_string()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .expect("create repository");
        let release = rg_db::ops::release_ops::create(
            &db,
            ReleaseActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                tag_name: Set("v1.0.0".to_string()),
                target_commitish: Set("main".to_string()),
                title: Set("One".to_string()),
                body: Set(None),
                is_draft: Set(false),
                is_prerelease: Set(false),
                author_id: Set(Some(owner.id)),
                created_at: Set(now),
                updated_at: Set(now),
            },
        )
        .await
        .expect("create release");

        (dir, db, release.id)
    }

    #[tokio::test]
    async fn delete_after_the_service_read_is_typed_not_found() {
        let (_dir, db, release_id) = fixture().await;

        let error = update_release_after_read(
            &db,
            release_id,
            Some("Too late"),
            Some("gone"),
            Some(true),
            Some(true),
            || async {
                assert!(rg_db::ops::release_ops::delete_by_id(&db, release_id)
                    .await
                    .expect("the competing release delete succeeds"));
                Ok(())
            },
        )
        .await
        .expect_err("a winning delete must not become a successful release update");

        let typed = error
            .downcast_ref::<crate::error::NotFound>()
            .expect("the lost race must stay classifiable as HTTP 404");
        assert_eq!(typed.resource, "release");
    }
}

/// Guards for the download-path resolver — the release-asset half of the
/// hash-first-then-stream refactor (card_7e762d2c19b0).
///
/// The handler's own memory cost is measured through the primitive it composes
/// (`http_stream::hash_local_file` + `file_body_with_idle`), which already has
/// its own VmHWM regression in `crates/rg-http/src/http_stream.rs`. What lives
/// here is the composition: an asset uploaded via the default local backend
/// has to come back as [`AssetSource::LocalFile`] (so the handler picks the
/// zero-copy path) and never as `Buffered`, because the buffered branch is the
/// remote-backend fallback and would put the asset back in heap.
#[cfg(test)]
mod download_source_tests {
    use super::*;
    use crate::blob_storage::LocalBlobStorage;
    use sea_orm::NotSet;

    async fn fixture() -> (
        tempfile::TempDir,
        DatabaseConnection,
        LocalBlobStorage,
        String,
        String,
        Asset,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("release.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            4,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let owner_row = rg_db::ops::user_ops::create_user(
            &db,
            "asset-download-owner",
            "asset-download-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .expect("create owner");
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner_row.id),
                name: Set("asset-download-repo".to_string()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .expect("create repository");
        let release = rg_db::ops::release_ops::create(
            &db,
            ReleaseActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                tag_name: Set("v1.0.0".to_string()),
                target_commitish: Set("main".to_string()),
                title: Set("One".to_string()),
                body: Set(None),
                is_draft: Set(false),
                is_prerelease: Set(false),
                author_id: Set(Some(owner_row.id)),
                created_at: Set(now),
                updated_at: Set(now),
            },
        )
        .await
        .expect("create release");
        let asset = rg_db::ops::release_ops::create_asset(
            &db,
            AssetActiveModel {
                id: NotSet,
                release_id: Set(release.id),
                filename: Set("payload.bin".to_string()),
                size: Set(0),
                content_type: Set("application/octet-stream".to_string()),
                download_count: Set(0),
                uploader_id: Set(Some(owner_row.id)),
                created_at: Set(now),
                sha256: Set(None),
                attestation: Set(None),
            },
        )
        .await
        .expect("create asset row");

        let repo_root = dir.path().join("repos");
        tokio::fs::create_dir_all(&repo_root)
            .await
            .expect("create repo_root");
        let storage = LocalBlobStorage::new(&repo_root);

        let owner = owner_row.username.clone();
        let repo_name = repo.name.clone();
        (dir, db, storage, owner, repo_name, asset)
    }

    async fn seed_asset_bytes(
        storage: &LocalBlobStorage,
        owner: &str,
        repo_name: &str,
        asset: &Asset,
        bytes: &[u8],
    ) {
        let key = asset_blob_key(owner, repo_name, asset).expect("build asset key");
        let path = storage
            .local_path(&key)
            .expect("local backend must expose a path");
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("create asset dir");
        }
        tokio::fs::write(&path, bytes)
            .await
            .expect("write asset bytes");
    }

    /// The local backend has to come back as `LocalFile` so the handler can
    /// stream and hash off disk. A mutation that always returns
    /// `AssetSource::Buffered(storage.get(&key).await?)` would put the asset
    /// back in heap — this test reads it before the VmHWM one ever runs.
    #[tokio::test]
    async fn resolve_asset_source_returns_local_file_for_the_local_backend() {
        let (_dir, _db, storage, owner, repo_name, asset) = fixture().await;
        seed_asset_bytes(&storage, &owner, &repo_name, &asset, b"payload").await;

        let source = resolve_asset_source(
            &storage,
            std::path::Path::new("does-not-matter"),
            &owner,
            &repo_name,
            &asset,
        )
        .await
        .expect("resolve local asset");

        match source {
            AssetSource::LocalFile { path, size } => {
                assert_eq!(
                    size, 7,
                    "the resolver's size must match the on-disk metadata"
                );
                assert!(
                    path.starts_with(storage.root()),
                    "local backend asset must live under the storage root, got {}",
                    path.display()
                );
            }
            AssetSource::Buffered(_) => panic!(
                "the local backend returned Buffered — a mutation put the asset back in heap"
            ),
        }
    }

    /// An asset whose key is missing in blob storage but present in the
    /// pre-migration `<owner>/<repo>.releases/assets/…` layout must still come
    /// back as `LocalFile`, so legacy installations keep the same disk-bound
    /// download path as the current one.
    #[tokio::test]
    async fn resolve_asset_source_falls_back_to_the_legacy_on_disk_layout() {
        let (dir, _db, storage, owner, repo_name, asset) = fixture().await;
        // Blob storage stays empty — force the fallback branch.
        let repo_root = dir.path().join("repos");
        let legacy = asset_file_path(&repo_root, &owner, &repo_name, &asset);
        tokio::fs::create_dir_all(legacy.parent().unwrap())
            .await
            .expect("create legacy dir");
        tokio::fs::write(&legacy, b"legacy asset bytes")
            .await
            .expect("write legacy asset");

        let source = resolve_asset_source(&storage, &repo_root, &owner, &repo_name, &asset)
            .await
            .expect("resolve legacy asset");

        match source {
            AssetSource::LocalFile { path, size } => {
                assert_eq!(path, legacy, "legacy fallback must return the legacy path");
                assert_eq!(size, b"legacy asset bytes".len() as u64);
            }
            AssetSource::Buffered(_) => {
                panic!("legacy fallback must return LocalFile, not Buffered")
            }
        }
    }

    /// Peak resident set size, in bytes — the same probe every VmHWM
    /// regression in this workspace uses.
    ///
    /// The *peak* (`VmHWM`), not the current RSS: a `Vec` collected and then
    /// dropped is back off the books by the time the reading is taken. The
    /// high-water mark is the number a `.get() -> Vec<u8>` mutation moves.
    fn peak_resident_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status
            .lines()
            .find(|line| line.starts_with("VmHWM:"))?
            .strip_prefix("VmHWM:")?;
        let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
        Some(kib * 1024)
    }

    /// The measurement the fix exists for: downloading a large asset through
    /// the service pipeline (resolve → hash → open) must not grow the process
    /// by the asset's own size.
    ///
    /// Mutating `resolve_asset_source` to always return
    /// `AssetSource::Buffered(storage.get(&key).await?)` on the local backend
    /// would put the whole asset back in heap and turn this red.
    #[cfg_attr(not(target_os = "linux"), ignore = "reads /proc/self/status")]
    #[tokio::test]
    async fn downloading_a_large_asset_does_not_grow_the_process_by_its_size() {
        const CHUNK: usize = 1024 * 1024;
        const CHUNKS: usize = 256;
        const TOTAL: usize = CHUNK * CHUNKS;

        let (_dir, db, storage, owner, repo_name, asset) = fixture().await;

        // Seed the asset on disk in bounded chunks — allocating one `Vec` of
        // TOTAL would already grow VmHWM by TOTAL and hide what the download
        // pipeline costs.
        let key = asset_blob_key(&owner, &repo_name, &asset).expect("build asset key");
        let path = storage
            .local_path(&key)
            .expect("local backend must expose a path");
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("create asset dir");
        }
        {
            use tokio::io::AsyncWriteExt as _;
            let mut file = tokio::fs::File::create(&path).await.unwrap();
            let chunk = vec![b'r'; CHUNK];
            for _ in 0..CHUNKS {
                file.write_all(&chunk).await.unwrap();
            }
            file.flush().await.unwrap();
        }

        let before = peak_resident_bytes().expect("no /proc/self/status to measure against");

        let (_asset, source) = download_asset(
            &db,
            asset.id,
            &storage,
            std::path::Path::new("does-not-matter"),
            &owner,
            &repo_name,
        )
        .await
        .expect("download the large asset");

        let (path, size) = match source {
            AssetSource::LocalFile { path, size } => (path, size),
            AssetSource::Buffered(bytes) => panic!(
                "the local backend must return LocalFile — Buffered {} bytes indicates \
                 the resolver was mutated to `.get()`",
                bytes.len()
            ),
        };
        assert_eq!(size as usize, TOTAL);
        // Hash-first-then-stream: the pipeline the handler runs. Both halves
        // are streaming primitives; a mutation that reverts either to
        // `tokio::fs::read` puts the asset back in heap.
        let sha = hash_release_asset_file(&path).await.expect("hash asset");
        assert_eq!(sha.len(), 64);
        // The handler follows the streaming hash with `File::open` +
        // `metadata().len()` — spelt out here so a mutation that reintroduces a
        // whole-file read on the way to `Content-Length` (`tokio::fs::read`,
        // `read_to_end`) shows up in the VmHWM assertion below.
        let file = tokio::fs::File::open(&path)
            .await
            .expect("open asset for stream");
        let on_disk_size = file.metadata().await.expect("stat asset for stream").len();
        assert_eq!(on_disk_size as usize, TOTAL);
        drop(file);

        let after = peak_resident_bytes().expect("no /proc/self/status to measure against");
        let grew = after.saturating_sub(before);
        let ceiling = (TOTAL / 4) as u64;
        assert!(
            grew < ceiling,
            "a {} MiB asset grew the process by {} MiB — the resolver returned \
             Buffered instead of streaming from disk",
            TOTAL / (1024 * 1024),
            grew / (1024 * 1024)
        );
    }
}
