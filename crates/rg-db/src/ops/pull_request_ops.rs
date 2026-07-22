//! Database operations for pull requests.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::pull_request::{self, ActiveModel, Entity as PrEntity, Model as PullRequest};

/// Find a PR by database ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<PullRequest>> {
    PrEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find PR by id")
}

/// Find a PR by (repo_id, number).
pub async fn find_by_repo_and_number(
    db: &DatabaseConnection,
    repo_id: i64,
    number: i64,
) -> Result<Option<PullRequest>> {
    PrEntity::find()
        .filter(pull_request::Column::RepoId.eq(repo_id))
        .filter(pull_request::Column::Number.eq(number))
        .one(db)
        .await
        .context("db: find PR by repo and number")
}

/// List PRs for a repo, optionally filtered by state.
pub async fn list_by_repo(
    db: &DatabaseConnection,
    repo_id: i64,
    state: Option<&str>,
) -> Result<Vec<PullRequest>> {
    let mut query = PrEntity::find().filter(pull_request::Column::RepoId.eq(repo_id));
    if let Some(s) = state {
        query = query.filter(pull_request::Column::State.eq(s));
    }
    query
        .order_by_desc(pull_request::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list PRs by repo")
}

/// Paginated list of PRs for a repo. Returns (data, total).
pub async fn list_by_repo_paginated(
    db: &DatabaseConnection,
    repo_id: i64,
    state: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<PullRequest>, i64)> {
    let mut base = PrEntity::find().filter(pull_request::Column::RepoId.eq(repo_id));
    if let Some(s) = state {
        base = base.filter(pull_request::Column::State.eq(s));
    }
    let query = base.order_by_desc(pull_request::Column::CreatedAt);

    let total = query
        .clone()
        .count(db)
        .await
        .context("db: count PRs by repo")? as i64;
    let prs = query
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list PRs by repo (paginated)")?;

    Ok((prs, total))
}

/// Get the next PR number for a repo (max + 1, or 1 if no PRs).
pub async fn next_number(db: &DatabaseConnection, repo_id: i64) -> Result<i64> {
    let max = PrEntity::find()
        .filter(pull_request::Column::RepoId.eq(repo_id))
        .order_by_desc(pull_request::Column::Number)
        .one(db)
        .await
        .context("db: get max PR number")?;
    Ok(max.map(|m| m.number + 1).unwrap_or(1))
}

/// Create a new PR.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<PullRequest> {
    model.insert(db).await.context("db: create PR")
}

/// Update a PR.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<PullRequest> {
    model.update(db).await.context("db: update PR")
}

/// Delete a PR by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    PrEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete PR")?;
    Ok(())
}

fn head_repository_condition(source_repo_id: i64) -> Condition {
    Condition::any()
        .add(
            Condition::all()
                .add(pull_request::Column::RepoId.eq(source_repo_id))
                .add(pull_request::Column::HeadRepoId.is_null()),
        )
        .add(pull_request::Column::HeadRepoId.eq(source_repo_id))
}

/// Refresh open PR head SHAs after a branch push, including fork PRs whose
/// source repository is the pushed repository.
pub async fn update_open_head_sha(
    db: &DatabaseConnection,
    source_repo_id: i64,
    head_branch: &str,
    head_sha: &str,
) -> Result<Vec<PullRequest>> {
    let prs = PrEntity::find()
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadBranch.eq(head_branch))
        .filter(pull_request::Column::State.eq("open"))
        .all(db)
        .await
        .context("db: find PRs for pushed branch")?;

    let mut updated = Vec::with_capacity(prs.len());
    for pr in prs {
        if pr.head_sha.as_deref() == Some(head_sha) {
            updated.push(pr);
            continue;
        }
        let mut active: ActiveModel = pr.into();
        active.head_sha = Set(Some(head_sha.to_string()));
        active.updated_at = Set(chrono::Utc::now());
        updated.push(active.update(db).await.context("db: refresh PR head SHA")?);
    }
    Ok(updated)
}

/// Advance PR heads only while they still point at the commit that was used
/// to prepare a server-side update. This prevents a later DB write from
/// overwriting a newer concurrent push notification.
pub async fn advance_open_head_sha(
    db: &DatabaseConnection,
    source_repo_id: i64,
    head_branch: &str,
    expected_head_sha: &str,
    new_head_sha: &str,
) -> Result<u64> {
    let result = PrEntity::update_many()
        .col_expr(
            pull_request::Column::HeadSha,
            Expr::value(Some(new_head_sha.to_string())),
        )
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadBranch.eq(head_branch))
        .filter(pull_request::Column::State.eq("open"))
        .filter(pull_request::Column::HeadSha.eq(expected_head_sha))
        .exec(db)
        .await
        .context("db: advance PR head SHA")?;
    Ok(result.rows_affected)
}

/// Find enabled auto-merge PRs for a source repository commit.
pub async fn list_auto_merge_for_head_commit(
    db: &DatabaseConnection,
    source_repo_id: i64,
    commit_sha: &str,
) -> Result<Vec<PullRequest>> {
    PrEntity::find()
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadSha.eq(commit_sha))
        .filter(pull_request::Column::State.eq("open"))
        .filter(pull_request::Column::AutoMergeEnabled.eq(true))
        .all(db)
        .await
        .context("db: list auto-merge PRs for head commit")
}

pub async fn list_open_for_head_commit(
    db: &DatabaseConnection,
    source_repo_id: i64,
    commit_sha: &str,
) -> Result<Vec<PullRequest>> {
    PrEntity::find()
        .filter(head_repository_condition(source_repo_id))
        .filter(pull_request::Column::HeadSha.eq(commit_sha))
        .filter(pull_request::Column::State.eq("open"))
        .all(db)
        .await
        .context("db: list open PRs for head commit")
}

/// Atomically claim an auto-merge so concurrent approval/CI/push events cannot
/// merge the same PR twice.
pub async fn claim_auto_merge(db: &DatabaseConnection, pr_id: i64) -> Result<bool> {
    let result = PrEntity::update_many()
        .col_expr(pull_request::Column::AutoMergeEnabled, Expr::value(false))
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("open"))
        .filter(pull_request::Column::AutoMergeEnabled.eq(true))
        .exec(db)
        .await
        .context("db: claim auto-merge")?;
    Ok(result.rows_affected == 1)
}

pub async fn restore_auto_merge(db: &DatabaseConnection, pr_id: i64) -> Result<()> {
    PrEntity::update_many()
        .col_expr(pull_request::Column::AutoMergeEnabled, Expr::value(true))
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("open"))
        .exec(db)
        .await
        .context("db: restore auto-merge")?;
    Ok(())
}

/// Atomically move an open PR into the short-lived `merging` state.
pub async fn claim_merge(db: &DatabaseConnection, pr_id: i64) -> Result<bool> {
    let result = PrEntity::update_many()
        .col_expr(pull_request::Column::State, Expr::value("merging"))
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("open"))
        .exec(db)
        .await
        .context("db: claim PR merge")?;
    Ok(result.rows_affected == 1)
}

pub async fn restore_merge_claim(db: &DatabaseConnection, pr_id: i64) -> Result<()> {
    PrEntity::update_many()
        .col_expr(pull_request::Column::State, Expr::value("open"))
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("merging"))
        .exec(db)
        .await
        .context("db: restore PR merge claim")?;
    Ok(())
}

/// Recover a merge claim left behind by a crashed process after its lease has
/// expired. Returns true only when this call restored the PR.
pub async fn recover_stale_merge_claim(
    db: &DatabaseConnection,
    pr_id: i64,
    cutoff: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let result = PrEntity::update_many()
        .col_expr(pull_request::Column::State, Expr::value("open"))
        .col_expr(
            pull_request::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(pull_request::Column::Id.eq(pr_id))
        .filter(pull_request::Column::State.eq("merging"))
        .filter(pull_request::Column::UpdatedAt.lt(cutoff))
        .exec(db)
        .await
        .context("db: recover stale PR merge claim")?;
    Ok(result.rows_affected == 1)
}
