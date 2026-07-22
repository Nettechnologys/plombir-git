//! Database operations for commit statuses.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::commit_status::{
    self, ActiveModel, Entity as CommitStatusEntity, Model as CommitStatus,
};

/// Create or update a commit status (upsert by repo_id + sha + context).
pub async fn create_or_update(
    db: &DatabaseConnection,
    repo_id: i64,
    sha: &str,
    context: &str,
    model: ActiveModel,
) -> Result<CommitStatus> {
    // Try to find existing by unique constraint (repo_id, sha, context)
    let existing = CommitStatusEntity::find()
        .filter(commit_status::Column::RepoId.eq(repo_id))
        .filter(commit_status::Column::Sha.eq(sha))
        .filter(commit_status::Column::Context.eq(context))
        .one(db)
        .await
        .context("db: find existing commit status")?;

    if let Some(existing) = existing {
        let mut active: ActiveModel = existing.into();
        active.state = model.state;
        active.description = model.description;
        active.target_url = model.target_url;
        active.creator_id = model.creator_id;
        active.updated_at = model.updated_at;
        active.update(db).await.context("db: update commit status")
    } else {
        model.insert(db).await.context("db: create commit status")
    }
}

/// List all statuses for a commit SHA in a repo.
pub async fn list_by_sha(
    db: &DatabaseConnection,
    repo_id: i64,
    sha: &str,
) -> Result<Vec<CommitStatus>> {
    CommitStatusEntity::find()
        .filter(commit_status::Column::RepoId.eq(repo_id))
        .filter(commit_status::Column::Sha.eq(sha))
        .order_by_desc(commit_status::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list commit statuses by sha")
}

/// Get combined status counts per state for a commit SHA.
pub async fn get_combined_status(
    db: &DatabaseConnection,
    repo_id: i64,
    sha: &str,
) -> Result<Vec<(String, i64)>> {
    let statuses = list_by_sha(db, repo_id, sha).await?;

    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for s in &statuses {
        *counts.entry(s.state.clone()).or_insert(0) += 1;
    }

    Ok(counts.into_iter().collect())
}
