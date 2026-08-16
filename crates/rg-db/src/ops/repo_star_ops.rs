//! Database operations for repository stars.

use anyhow::{Context, Result};
use sea_orm::sea_query::OnConflict;
use sea_orm::*;

use crate::entities::{
    repo_star::{self, ActiveModel, Entity as RepoStarEntity},
    user,
};

/// Public account fields attached to one repository star.
///
/// Returning the joined user here keeps the list usable without leaking the
/// full `users` row (password hash, MFA material, session generation) through a
/// higher layer that happens to serialize its result.
#[derive(Clone, Debug, PartialEq)]
pub struct Stargazer {
    pub user_id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub starred_at: chrono::DateTime<chrono::Utc>,
}

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
) -> Result<(Vec<Stargazer>, i64)> {
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
        .find_also_related(user::Entity)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list stargazers")?
        .into_iter()
        .map(|(star, user)| {
            let user = user.context("repo star points at a missing user")?;
            Ok(Stargazer {
                user_id: user.id,
                username: user.username,
                display_name: user.display_name,
                avatar_url: user.avatar_url,
                starred_at: star.created_at,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok((stargazers, total))
}

/// List the repositories this account has starred, one id per repository.
///
/// Takes any connection because the one caller that needs it —
/// [`crate::ops::user_ops::delete_by_id`] — has to read the list inside the
/// transaction that is about to remove the account, and `repo_stars.user_id` is
/// `ON DELETE CASCADE`: after the delete there is nothing left to inventory.
/// The repositories these stars sit on belong to other accounts and stay, so
/// their cached `stars_count` is what the caller then refreshes.
pub async fn list_starred_repo_ids(db: &impl ConnectionTrait, user_id: i64) -> Result<Vec<i64>> {
    RepoStarEntity::find()
        .select_only()
        .column(repo_star::Column::RepoId)
        .filter(repo_star::Column::UserId.eq(user_id))
        .into_tuple::<i64>()
        .all(db)
        .await
        .context("db: list the repositories an account has starred")
}
