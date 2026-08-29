use crate::entities::{ci_cache_entry, ci_retention_policy};
use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sea_orm::sea_query::{Expr, OnConflict};
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
///
/// `None` means an existing policy disappeared under a repository cascade
/// before this call's update. That DELETE is authoritative and this primitive
/// never recreates the policy from its stale snapshot.
pub async fn upsert_policy(
    db: &DatabaseConnection,
    repo_id: i64,
    artifact_days: i32,
    cache_days: i32,
) -> Result<Option<ci_retention_policy::Model>> {
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
        Ok(created) => Ok(Some(created)),
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
///
/// The observed model supplies only the stable primary key. A repository
/// cascade between the read and this write returns `None`, never SeaORM's
/// backend-shaped `RecordNotUpdated` and never a resurrected policy.
pub async fn apply_policy(
    db: &DatabaseConnection,
    model: ci_retention_policy::Model,
    artifact_days: i32,
    cache_days: i32,
) -> Result<Option<ci_retention_policy::Model>> {
    let repo_id = model.repo_id;
    let updated = ci_retention_policy::Entity::update_many()
        .col_expr(
            ci_retention_policy::Column::ArtifactRetentionDays,
            Expr::value(artifact_days),
        )
        .col_expr(
            ci_retention_policy::Column::CacheRetentionDays,
            Expr::value(cache_days),
        )
        .col_expr(
            ci_retention_policy::Column::UpdatedAt,
            Expr::value(Utc::now()),
        )
        .filter(ci_retention_policy::Column::RepoId.eq(repo_id))
        .exec(db)
        .await
        .context("db: update CI retention policy")?;
    match updated.rows_affected {
        0 | 1 => {}
        rows => {
            anyhow::bail!("db: CI retention policy update affected {rows} rows for repo {repo_id}")
        }
    }

    // MySQL reports zero changed rows for a no-op update. Re-read the stable
    // primary key on every backend so zero means absence only when the
    // repository cascade actually removed the policy.
    ci_retention_policy::Entity::find_by_id(repo_id)
        .one(db)
        .await
        .context("db: find updated CI retention policy")
}

pub fn expires_after(days: i32) -> chrono::DateTime<Utc> {
    Utc::now() + Duration::days(days as i64)
}

/// Register the cache blob stored under `key_hash`, creating the row on first
/// use.
///
/// `(repo_id, key_hash)` is UNIQUE (`uq_ci_cache_entries_repo_key`). The
/// conflict-targeted statement makes a first insert and a replacement one
/// database arbitration point: a retention eviction cannot land between a
/// separate read and a stale `ActiveModel::update` and turn an ordinary cache
/// publication into `RecordNotUpdated`.
///
/// Last writer wins, and it wins *whole*: `file_path`, `size` and `sha256`
/// describe one uploaded blob. Digest-less legacy callers keep the previously
/// stored digest for compatibility; both production publication paths always
/// supply the digest of the archive they are registering.
///
/// A bounded second statement covers the one unusual interleaving in which an
/// eviction removes the conflicting row from inside the database's UPDATE
/// path. A foreign key failure (no such repository) or a broken connection is
/// never retried into success: nothing was registered, and the caller must roll
/// back its request-private archive.
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
    for attempt in 0..2 {
        let mut conflict = OnConflict::columns([
            ci_cache_entry::Column::RepoId,
            ci_cache_entry::Column::KeyHash,
        ]);
        conflict.update_columns([
            ci_cache_entry::Column::FilePath,
            ci_cache_entry::Column::Size,
            ci_cache_entry::Column::ExpiresAt,
        ]);
        if sha256.is_some() {
            conflict.update_column(ci_cache_entry::Column::Sha256);
        }

        ci_cache_entry::Entity::insert(ci_cache_entry::ActiveModel {
            repo_id: Set(repo_id),
            key_hash: Set(key_hash.to_string()),
            file_path: Set(file_path.to_string()),
            size: Set(size),
            sha256: Set(sha256.map(str::to_string)),
            created_at: Set(now),
            expires_at: Set(expires_at),
            ..Default::default()
        })
        .on_conflict(conflict.to_owned())
        .exec_without_returning(db)
        .await
        .context("db: upsert CI cache entry")?;

        if let Some(entry) = find_cache_entry(db, repo_id, key_hash).await? {
            return Ok(entry);
        }
        if attempt == 0 {
            continue;
        }
    }

    anyhow::bail!(
        "db: CI cache entry remained absent after converging repo {repo_id}, key {key_hash}"
    )
}

/// Extend the lifetime of the exact publication a reader observed.
///
/// A download or restore must not call [`upsert_cache_entry`]: if a new upload
/// lands after the read, a whole-publication upsert would point the row back at
/// the old archive and could resurrect bytes the successful upload is retiring.
/// `false` means that exact publication was replaced or deleted meanwhile.
pub async fn refresh_cache_entry(
    db: &DatabaseConnection,
    observed: &ci_cache_entry::Model,
    retention_days: i32,
) -> Result<bool> {
    let result = ci_cache_entry::Entity::update_many()
        .col_expr(
            ci_cache_entry::Column::ExpiresAt,
            Expr::value(expires_after(retention_days)),
        )
        .filter(ci_cache_entry::Column::Id.eq(observed.id))
        .filter(ci_cache_entry::Column::RepoId.eq(observed.repo_id))
        .filter(ci_cache_entry::Column::KeyHash.eq(observed.key_hash.clone()))
        .filter(ci_cache_entry::Column::FilePath.eq(observed.file_path.clone()))
        .exec(db)
        .await
        .context("db: refresh CI cache entry")?;
    if result.rows_affected > 1 {
        anyhow::bail!(
            "db: CI cache refresh affected {} rows for id {}",
            result.rows_affected,
            observed.id
        );
    }

    // MySQL can report zero for a no-op UPDATE. Re-read the same publication on
    // every backend instead of treating the row count as absence.
    Ok(find_cache_entry(db, observed.repo_id, &observed.key_hash)
        .await?
        .is_some_and(|current| {
            current.id == observed.id && current.file_path == observed.file_path
        }))
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
/// Delete only the expired publication the caller actually observed.
///
/// Retention lists rows before it stages their archives. A download, restore or
/// upload can refresh or replace that row in between; filtering on the stable
/// identity, publication path and *current* expiry keeps the stale sweep from
/// deleting the now-live entry. `false` is normal convergence, not a failure.
pub async fn delete_cache_entry_if_expired(
    db: &DatabaseConnection,
    observed: &ci_cache_entry::Model,
) -> Result<bool> {
    let result = ci_cache_entry::Entity::delete_many()
        .filter(ci_cache_entry::Column::Id.eq(observed.id))
        .filter(ci_cache_entry::Column::RepoId.eq(observed.repo_id))
        .filter(ci_cache_entry::Column::KeyHash.eq(observed.key_hash.clone()))
        .filter(ci_cache_entry::Column::FilePath.eq(observed.file_path.clone()))
        .filter(ci_cache_entry::Column::ExpiresAt.lte(Utc::now()))
        .exec(db)
        .await
        .context("db: delete CI cache entry")?;
    match result.rows_affected {
        0 => Ok(false),
        1 => Ok(true),
        rows => anyhow::bail!(
            "db: expired CI cache delete affected {rows} rows for id {}",
            observed.id
        ),
    }
}
