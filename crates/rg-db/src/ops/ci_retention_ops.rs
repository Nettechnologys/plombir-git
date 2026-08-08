use crate::entities::{ci_cache_entry, ci_retention_policy};
use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sea_orm::*;

pub const DEFAULT_ARTIFACT_RETENTION_DAYS: i32 = 30;
pub const DEFAULT_CACHE_RETENTION_DAYS: i32 = 7;

pub async fn get_policy(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<ci_retention_policy::Model> {
    Ok(ci_retention_policy::Entity::find_by_id(repo_id)
        .one(db)
        .await
        .context("db: get CI retention policy")?
        .unwrap_or(ci_retention_policy::Model {
            repo_id,
            artifact_retention_days: DEFAULT_ARTIFACT_RETENTION_DAYS,
            cache_retention_days: DEFAULT_CACHE_RETENTION_DAYS,
            updated_at: Utc::now(),
        }))
}

/// Store a repository's retention policy, creating the row on first use.
///
/// `repo_id` is the table's primary key, and the lookup below is a separate
/// statement from the insert that follows it. Two admins saving the settings
/// form at once both read `None` and both insert; one meets the key. That loss
/// says the policy row this call wanted now exists, so it is resolved by
/// re-reading it and writing this call's days onto it — last save wins, both
/// numbers from the same submission, never one field from each.
///
/// Only a UNIQUE/primary-key violation is treated this way. A foreign key
/// failure (no such repository) or a broken connection stays an error: the
/// policy genuinely was not stored, and answering `200` would tell an admin
/// their retention window changed when it did not.
pub async fn upsert_policy(
    db: &DatabaseConnection,
    repo_id: i64,
    artifact_days: i32,
    cache_days: i32,
) -> Result<ci_retention_policy::Model> {
    if let Some(model) = ci_retention_policy::Entity::find_by_id(repo_id)
        .one(db)
        .await?
    {
        return apply_policy(db, model, artifact_days, cache_days).await;
    }

    let insert = ci_retention_policy::ActiveModel {
        repo_id: Set(repo_id),
        artifact_retention_days: Set(artifact_days),
        cache_retention_days: Set(cache_days),
        updated_at: Set(Utc::now()),
    }
    .insert(db)
    .await;

    match insert {
        Ok(created) => Ok(created),
        Err(error) if crate::is_unique_violation(&error) => {
            match ci_retention_policy::Entity::find_by_id(repo_id)
                .one(db)
                .await?
            {
                Some(model) => apply_policy(db, model, artifact_days, cache_days).await,
                // Not there after all, so the collision was on some other
                // constraint. Report the original failure.
                None => Err(error).context("db: create CI retention policy"),
            }
        }
        Err(error) => Err(error).context("db: create CI retention policy"),
    }
}

/// Write this call's retention days onto an existing policy row.
async fn apply_policy(
    db: &DatabaseConnection,
    model: ci_retention_policy::Model,
    artifact_days: i32,
    cache_days: i32,
) -> Result<ci_retention_policy::Model> {
    let mut active: ci_retention_policy::ActiveModel = model.into();
    active.artifact_retention_days = Set(artifact_days);
    active.cache_retention_days = Set(cache_days);
    active.updated_at = Set(Utc::now());
    active
        .update(db)
        .await
        .context("db: update CI retention policy")
}

pub fn expires_after(days: i32) -> chrono::DateTime<Utc> {
    Utc::now() + Duration::days(days as i64)
}

/// Register the cache blob stored under `key_hash`, creating the row on first
/// use.
///
/// `(repo_id, key_hash)` is UNIQUE (`uq_ci_cache_entries_repo_key`), and the
/// lookup below is a separate statement from the insert that follows it. Two
/// jobs of one pipeline finishing together upload the same cache key — the
/// ordinary shape of a build matrix sharing a dependency cache — so both read
/// `None` and both insert; one meets the constraint. That loss says the entry
/// this call wanted now exists, so it is resolved by re-reading the winner's
/// row and pointing it at this call's blob.
///
/// Last writer wins, and it wins *whole*: `file_path`, `size` and `sha256`
/// describe one uploaded blob, so the re-read path reuses the same update as
/// the existing-row branch rather than merging two uploads into a row whose
/// digest belongs to a different file than its path.
///
/// Only a UNIQUE violation is treated this way. A foreign key failure (no such
/// repository) or a broken connection stays an error: nothing was registered,
/// and a fabricated success would leave the next job restoring a cache entry
/// that points at no blob.
pub async fn upsert_cache_entry(
    db: &DatabaseConnection,
    repo_id: i64,
    key_hash: &str,
    file_path: &str,
    size: i64,
    sha256: Option<&str>,
    retention_days: i32,
) -> Result<ci_cache_entry::Model> {
    let now = Utc::now();
    let expires_at = now + Duration::days(retention_days as i64);
    if let Some(model) = find_cache_entry(db, repo_id, key_hash).await? {
        return apply_cache_entry(db, model, file_path, size, sha256, now, expires_at).await;
    }

    let insert = ci_cache_entry::ActiveModel {
        repo_id: Set(repo_id),
        key_hash: Set(key_hash.to_string()),
        file_path: Set(file_path.to_string()),
        size: Set(size),
        sha256: Set(sha256.map(|s| s.to_string())),
        created_at: Set(now),
        last_accessed_at: Set(now),
        expires_at: Set(expires_at),
        ..Default::default()
    }
    .insert(db)
    .await;

    match insert {
        Ok(created) => Ok(created),
        Err(error) if crate::is_unique_violation(&error) => {
            match find_cache_entry(db, repo_id, key_hash).await? {
                Some(model) => {
                    apply_cache_entry(db, model, file_path, size, sha256, now, expires_at).await
                }
                // Not there after all, so the collision was on some other
                // constraint. Report the original failure.
                None => Err(error).context("db: create CI cache entry"),
            }
        }
        Err(error) => Err(error).context("db: create CI cache entry"),
    }
}

/// Point an existing cache entry at this call's blob.
async fn apply_cache_entry(
    db: &DatabaseConnection,
    model: ci_cache_entry::Model,
    file_path: &str,
    size: i64,
    sha256: Option<&str>,
    now: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
) -> Result<ci_cache_entry::Model> {
    let mut active: ci_cache_entry::ActiveModel = model.into();
    active.file_path = Set(file_path.to_string());
    active.size = Set(size);
    // Only overwrite the stored digest when the caller supplies one, so a
    // digest-less re-registration never wipes an existing content hash.
    if let Some(digest) = sha256 {
        active.sha256 = Set(Some(digest.to_string()));
    }
    active.last_accessed_at = Set(now);
    active.expires_at = Set(expires_at);
    active.update(db).await.context("db: update CI cache entry")
}

pub async fn list_expired_cache(db: &DatabaseConnection) -> Result<Vec<ci_cache_entry::Model>> {
    ci_cache_entry::Entity::find()
        .filter(ci_cache_entry::Column::ExpiresAt.lte(Utc::now()))
        .all(db)
        .await
        .context("db: list expired CI caches")
}
pub async fn find_cache_entry(
    db: &DatabaseConnection,
    repo_id: i64,
    key_hash: &str,
) -> Result<Option<ci_cache_entry::Model>> {
    ci_cache_entry::Entity::find()
        .filter(ci_cache_entry::Column::RepoId.eq(repo_id))
        .filter(ci_cache_entry::Column::KeyHash.eq(key_hash))
        .one(db)
        .await
        .context("db: find CI cache entry")
}
pub async fn delete_cache_entry(db: &DatabaseConnection, id: i64) -> Result<()> {
    ci_cache_entry::Entity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete CI cache entry")?;
    Ok(())
}
