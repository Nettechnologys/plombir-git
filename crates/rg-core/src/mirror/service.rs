//! Mirror service — business logic for repository mirroring.
//!
//! Supports creating a mirror of an external Git repository, periodic
//! sync via cron-like scheduling, and manual sync triggers.

use anyhow::{Context, Result};
use chrono::Utc;
use rg_db::entities::mirror::{ActiveModel, Model as Mirror};
use rg_db::entities::repository;
use rg_git::cli_gateway::global_gateway;
use sea_orm::ActiveValue::Set;
use sea_orm::{DatabaseConnection, EntityTrait};
use std::path::Path;

/// Create a new mirror for a repository.
pub async fn create_mirror(
    db: &DatabaseConnection,
    repo_id: i64,
    url: String,
    username: Option<String>,
    password: Option<String>,
    sync_interval_seconds: i64,
) -> Result<Mirror> {
    // Ensure the repository exists
    let repo = repository::Entity::find_by_id(repo_id)
        .one(db)
        .await
        .context("check repo exists")?;
    if repo.is_none() {
        anyhow::bail!("repository not found");
    }

    // Check for existing mirror
    if rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .is_some()
    {
        anyhow::bail!("mirror already exists for this repository");
    }

    let now = Utc::now();
    let next_sync = now + chrono::Duration::seconds(sync_interval_seconds);

    let model = ActiveModel {
        repo_id: Set(repo_id),
        url: Set(url),
        username: Set(username),
        password_encrypted: Set(password),
        sync_interval_seconds: Set(sync_interval_seconds),
        next_sync_at: Set(Some(next_sync)),
        last_sync_at: Set(None),
        last_sync_error: Set(None),
        status: Set("active".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let mirror = rg_db::ops::mirror_ops::create(db, model).await?;
    Ok(mirror)
}

/// Get mirror for a repository.
pub async fn get_mirror(db: &DatabaseConnection, repo_id: i64) -> Result<Option<Mirror>> {
    rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id).await
}

/// Update mirror settings.
pub async fn update_mirror(
    db: &DatabaseConnection,
    repo_id: i64,
    url: Option<String>,
    username: Option<String>,
    password: Option<String>,
    sync_interval_seconds: Option<i64>,
    status: Option<String>,
) -> Result<Mirror> {
    let existing = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("mirror not found"))?;

    let mut model: ActiveModel = existing.into();
    if let Some(v) = url {
        model.url = Set(v);
    }
    if let Some(v) = username {
        model.username = Set(Some(v));
    }
    if let Some(v) = password {
        model.password_encrypted = Set(Some(v));
    }
    if let Some(v) = sync_interval_seconds {
        model.sync_interval_seconds = Set(v);
    }
    if let Some(v) = status {
        model.status = Set(v);
    }
    model.updated_at = Set(Utc::now());

    rg_db::ops::mirror_ops::update(db, model).await
}

/// Delete a mirror.
pub async fn delete_mirror(db: &DatabaseConnection, repo_id: i64) -> Result<()> {
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("mirror not found"))?;
    rg_db::ops::mirror_ops::delete_by_id(db, mirror.id).await
}

/// Sync a single mirror: clone (first time) or fetch (subsequent).
///
/// Returns Ok(true) if synced successfully, Ok(false) if mirror is inactive.
pub async fn sync_mirror(
    db: &DatabaseConnection,
    mirror: &Mirror,
    repo_root: &Path,
) -> Result<bool> {
    if mirror.status != "active" {
        return Ok(false);
    }

    let repo_path = repo_root.join(format!("{}.mirror", mirror.repo_id));

    let result = if repo_path.join("HEAD").exists() {
        // Existing mirror: git remote update
        run_git_remote_update(&repo_path)
    } else {
        // First time: git clone --mirror
        run_git_clone_mirror(&mirror.url, &repo_path)
    };

    let now = Utc::now();
    let next_sync = now + chrono::Duration::seconds(mirror.sync_interval_seconds);

    let mut model: ActiveModel = mirror.clone().into();
    model.last_sync_at = Set(Some(now));
    model.next_sync_at = Set(Some(next_sync));
    model.updated_at = Set(now);

    match result {
        Ok(()) => {
            model.last_sync_error = Set(None);
            model.status = Set("active".to_string());
        }
        Err(e) => {
            model.last_sync_error = Set(Some(format!("{e}")));
            model.status = Set("error".to_string());
            tracing::error!("Mirror sync failed for repo {}: {e}", mirror.repo_id);
        }
    }

    rg_db::ops::mirror_ops::update(db, model).await?;
    Ok(true)
}

/// Sync all due mirrors (called by background task / cron).
pub async fn sync_due_mirrors(
    db: &DatabaseConnection,
    repo_root: &Path,
    limit: u64,
) -> Result<usize> {
    let mirrors = rg_db::ops::mirror_ops::list_due_sync(db, limit).await?;
    let mut count = 0;
    for mirror in &mirrors {
        match sync_mirror(db, mirror, repo_root).await {
            Ok(true) => count += 1,
            Ok(false) => { /* inactive, skip */ }
            Err(e) => tracing::error!("Mirror sync error: {e}"),
        }
    }
    Ok(count)
}

/// Manually trigger a sync for a mirror.
pub async fn trigger_sync(db: &DatabaseConnection, repo_id: i64, repo_root: &Path) -> Result<()> {
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("mirror not found"))?;
    sync_mirror(db, &mirror, repo_root).await?;
    Ok(())
}

// ── Git helpers ─────────────────────────────────────────────────────────

fn run_git_clone_mirror(url: &str, path: &Path) -> Result<()> {
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent).context("create mirror dir")?;

    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    git.run_or_bail(&["clone", "--mirror", url, &path.to_string_lossy()], None)
        .context("git clone --mirror")
}

fn run_git_remote_update(path: &Path) -> Result<()> {
    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    git.run_or_bail(&["remote", "update", "--prune"], Some(path))
        .context("git remote update")
}
