//! Database operations for commit statuses.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::commit_status::{
    self, ActiveModel, Entity as CommitStatusEntity, Model as CommitStatus,
};

fn required_value<T>(value: ActiveValue<T>, column: &'static str) -> Result<T>
where
    T: Into<Value>,
{
    match value {
        ActiveValue::Set(value) | ActiveValue::Unchanged(value) => Ok(value),
        ActiveValue::NotSet => anyhow::bail!("db: commit status {column} is not set"),
    }
}

/// Create or update a commit status (upsert by repo_id + sha + context).
///
/// `(repo_id, sha, context)` is UNIQUE
/// (`idx_commit_statuses_repo_sha_context_unique`), and the lookup below is a
/// separate statement from the insert that follows it. Two CI reports of the
/// same context on the same commit — the normal shape of a build matrix
/// finishing, or a runner retrying — both read `None` and both insert; one
/// meets the constraint. That loss says the row this call wanted now exists,
/// which is the outcome the caller asked for, so it is resolved by re-reading
/// the winner's row and writing this call's state onto it.
///
/// Last report wins, exactly as it would have if the two had arrived a
/// millisecond apart. Only a UNIQUE violation is treated this way: a foreign
/// key failure (the repo or creator does not exist) or a broken connection
/// stays an error, because the row genuinely was not written.
pub async fn create_or_update(
    db: &DatabaseConnection,
    repo_id: i64,
    sha: &str,
    context: &str,
    model: ActiveModel,
) -> Result<Option<CommitStatus>> {
    if let Some(existing) = find_by_key(db, repo_id, sha, context).await? {
        return update_existing(db, existing, model).await;
    }

    match model.clone().insert(db).await {
        Ok(inserted) => Ok(Some(inserted)),
        Err(error) if crate::is_unique_violation(&error) => {
            // Lost the race for the first row. Whoever won holds this exact
            // (repo, sha, context), so update it the way the existing-row
            // branch would.
            match find_by_key(db, repo_id, sha, context).await? {
                Some(existing) => update_existing(db, existing, model).await,
                // The row is not there after all, so the collision was on some
                // other constraint. Report the original failure rather than
                // inventing a reason for it.
                None => Err(error).context("db: create commit status"),
            }
        }
        Err(error) => Err(error).context("db: create commit status"),
    }
}

/// Read the one status row identified by the unique key.
async fn find_by_key(
    db: &DatabaseConnection,
    repo_id: i64,
    sha: &str,
    context: &str,
) -> Result<Option<CommitStatus>> {
    CommitStatusEntity::find()
        .filter(commit_status::Column::RepoId.eq(repo_id))
        .filter(commit_status::Column::Sha.eq(sha))
        .filter(commit_status::Column::Context.eq(context))
        .one(db)
        .await
        .context("db: find existing commit status")
}

/// Write this call's report onto an existing status row.
///
/// The read in [`create_or_update`] is a separate statement, so repository
/// deletion can cascade the row before this write. `None` is that authoritative
/// absence; this update-only primitive cannot answer the DELETE by recreating
/// the status.
pub async fn update_existing(
    db: &DatabaseConnection,
    existing: CommitStatus,
    model: ActiveModel,
) -> Result<Option<CommitStatus>> {
    let id = existing.id;
    let repo_id = existing.repo_id;
    let sha = existing.sha;
    let context = existing.context;
    let state = required_value(model.state, "state")?;
    let description = required_value(model.description, "description")?;
    let target_url = required_value(model.target_url, "target_url")?;
    let creator_id = required_value(model.creator_id, "creator_id")?;
    let updated_at = required_value(model.updated_at, "updated_at")?;
    let updated = CommitStatusEntity::update_many()
        .col_expr(commit_status::Column::State, Expr::value(state))
        .col_expr(commit_status::Column::Description, Expr::value(description))
        .col_expr(commit_status::Column::TargetUrl, Expr::value(target_url))
        .col_expr(commit_status::Column::CreatorId, Expr::value(creator_id))
        .col_expr(commit_status::Column::UpdatedAt, Expr::value(updated_at))
        .filter(commit_status::Column::Id.eq(id))
        .filter(commit_status::Column::RepoId.eq(repo_id))
        .filter(commit_status::Column::Sha.eq(sha.clone()))
        .filter(commit_status::Column::Context.eq(context.clone()))
        .exec(db)
        .await
        .context("db: update commit status")?;
    match updated.rows_affected {
        0 | 1 => {}
        rows => anyhow::bail!(
            "db: commit status update affected {rows} rows for id {id} in repo {repo_id}"
        ),
    }

    // MySQL may report zero affected rows for an unchanged report. Re-read the
    // exact identity on every backend: absence is therefore a winning DELETE,
    // never a backend-specific row-count guess or a replacement at the same
    // `(repo, sha, context)` key.
    CommitStatusEntity::find()
        .filter(commit_status::Column::Id.eq(id))
        .filter(commit_status::Column::RepoId.eq(repo_id))
        .filter(commit_status::Column::Sha.eq(sha))
        .filter(commit_status::Column::Context.eq(context))
        .one(db)
        .await
        .context("db: find updated commit status")
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
