//! Database operations for repository stars.

use anyhow::{Context, Result};
use sea_orm::sea_query::OnConflict;
use sea_orm::*;

use crate::entities::repo_star::{self, ActiveModel, Entity as RepoStarEntity, Model};

/// Toggle a star: if already starred, unstar (delete) and return false.
/// If not starred, star (insert) and return true.
///
/// `(user_id, repo_id)` is UNIQUE. Two simultaneous requests can both observe
/// no row and race to insert it. The conflict-targeted insert makes that branch
/// idempotent: the loser does not undo the winner's star and returns `true` too.
/// A foreign-key or database failure still remains an error.
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
        RepoStarEntity::insert(model)
            .on_conflict(
                OnConflict::columns([repo_star::Column::UserId, repo_star::Column::RepoId])
                    // MySQL needs a harmless assignment for its DO NOTHING
                    // polyfill; PostgreSQL and SQLite emit DO NOTHING.
                    .do_nothing_on([repo_star::Column::Id])
                    .to_owned(),
            )
            .do_nothing()
            .exec(db)
            .await
            .context("db: insert star")?;
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
///
/// The `id` tiebreaker is what makes the walk total: a repository that trends
/// collects stars faster than the timestamp's resolution, and paging over an
/// order the engine may resolve differently between requests lists one
/// stargazer twice while dropping the next.
pub async fn list_stargazers(
    db: &DatabaseConnection,
    repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Model>, i64)> {
    let base = RepoStarEntity::find()
        .filter(repo_star::Column::RepoId.eq(repo_id))
        .order_by_desc(repo_star::Column::CreatedAt)
        .order_by_desc(repo_star::Column::Id);

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
