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
        let mut active: ci_retention_policy::ActiveModel = model.into();
        active.artifact_retention_days = Set(artifact_days);
        active.cache_retention_days = Set(cache_days);
        active.updated_at = Set(Utc::now());
        return active
            .update(db)
            .await
            .context("db: update CI retention policy");
    }
    ci_retention_policy::ActiveModel {
        repo_id: Set(repo_id),
        artifact_retention_days: Set(artifact_days),
        cache_retention_days: Set(cache_days),
        updated_at: Set(Utc::now()),
    }
    .insert(db)
    .await
    .context("db: create CI retention policy")
}

pub fn expires_after(days: i32) -> chrono::DateTime<Utc> {
    Utc::now() + Duration::days(days as i64)
}

pub async fn upsert_cache_entry(
    db: &DatabaseConnection,
    repo_id: i64,
    key_hash: &str,
    file_path: &str,
    size: i64,
    retention_days: i32,
) -> Result<ci_cache_entry::Model> {
    let now = Utc::now();
    let expires_at = now + Duration::days(retention_days as i64);
    if let Some(model) = ci_cache_entry::Entity::find()
        .filter(ci_cache_entry::Column::RepoId.eq(repo_id))
        .filter(ci_cache_entry::Column::KeyHash.eq(key_hash))
        .one(db)
        .await?
    {
        let mut active: ci_cache_entry::ActiveModel = model.into();
        active.file_path = Set(file_path.to_string());
        active.size = Set(size);
        active.last_accessed_at = Set(now);
        active.expires_at = Set(expires_at);
        return active.update(db).await.context("db: update CI cache entry");
    }
    ci_cache_entry::ActiveModel {
        repo_id: Set(repo_id),
        key_hash: Set(key_hash.to_string()),
        file_path: Set(file_path.to_string()),
        size: Set(size),
        created_at: Set(now),
        last_accessed_at: Set(now),
        expires_at: Set(expires_at),
        ..Default::default()
    }
    .insert(db)
    .await
    .context("db: create CI cache entry")
}

pub async fn touch_cache_entry(
    db: &DatabaseConnection,
    repo_id: i64,
    key_hash: &str,
    retention_days: i32,
) -> Result<()> {
    if let Some(model) = ci_cache_entry::Entity::find()
        .filter(ci_cache_entry::Column::RepoId.eq(repo_id))
        .filter(ci_cache_entry::Column::KeyHash.eq(key_hash))
        .one(db)
        .await?
    {
        let mut active: ci_cache_entry::ActiveModel = model.into();
        let now = Utc::now();
        active.last_accessed_at = Set(now);
        active.expires_at = Set(now + Duration::days(retention_days as i64));
        active.update(db).await?;
    }
    Ok(())
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
