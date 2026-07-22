//! Database operations for repository stars.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::repo_star::{self, ActiveModel, Entity as RepoStarEntity, Model};

/// Toggle a star: if already starred, unstar (delete) and return false.
/// If not starred, star (insert) and return true.
pub async fn toggle_star(db: &DatabaseConnection, user_id: i64, repo_id: i64) -> Result<bool> {
    // Check if already starred
    let existing = RepoStarEntity::find()
        .filter(repo_star::Column::UserId.eq(user_id))
        .filter(repo_star::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: check existing star")?;

    if let Some(existing) = existing {
        // Unstar: delete the record
        RepoStarEntity::delete_by_id(existing.id)
            .exec(db)
            .await
            .context("db: delete star")?;
        Ok(false)
    } else {
        // Star: insert new record
        let now = chrono::Utc::now();
        let model = ActiveModel {
            user_id: Set(user_id),
            repo_id: Set(repo_id),
            created_at: Set(now),
            ..Default::default()
        };
        model.insert(db).await.context("db: insert star")?;
        Ok(true)
    }
}

/// Check if a user has starred a repository.
pub async fn is_starred(db: &DatabaseConnection, user_id: i64, repo_id: i64) -> Result<bool> {
    let result = RepoStarEntity::find()
        .filter(repo_star::Column::UserId.eq(user_id))
        .filter(repo_star::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: check star")?;
    Ok(result.is_some())
}

/// List stargazers of a repo with pagination.
pub async fn list_stargazers(
    db: &DatabaseConnection,
    repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Model>, i64)> {
    let base = RepoStarEntity::find()
        .filter(repo_star::Column::RepoId.eq(repo_id))
        .order_by_desc(repo_star::Column::CreatedAt);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count stargazers")? as i64;

    let stargazers = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list stargazers")?;

    Ok((stargazers, total))
}

/// Count the number of stars for a repository.
pub async fn count_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<i64> {
    RepoStarEntity::find()
        .filter(repo_star::Column::RepoId.eq(repo_id))
        .count(db)
        .await
        .context("db: count stars")
        .map(|c| c as i64)
}
