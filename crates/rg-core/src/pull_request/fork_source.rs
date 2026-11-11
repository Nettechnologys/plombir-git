//! Current authority to publish a private fork through a pull request.
//!
//! Reading a private repository permits a personal snapshot, not an ongoing
//! subscription that publishes later commits into another repository.
use std::collections::HashMap;

use anyhow::Result;
use rg_db::entities::{pull_request, repository};
use rg_db::ops::{pull_request_ops, repo_ops, user_ops};
use sea_orm::DatabaseConnection;

pub(crate) const UNAVAILABLE: &str =
    "head repository is no longer available to the pull request author";

pub(crate) fn unavailable() -> anyhow::Error {
    anyhow::Error::new(crate::error::HeadSourceUnavailable)
}

pub(crate) enum Source {
    SameRepository,
    Available(Box<repository::Model>),
    Unavailable,
}

/// One rule controls opening the channel and continuing to publish into it.
/// Downgrading a private source writer to reader freezes existing PRs too.
pub(crate) async fn may_publish(
    db: &DatabaseConnection,
    repository: &repository::Model,
    author_id: i64,
) -> Result<bool> {
    if repository.deleted_at.is_some() {
        return Ok(false);
    }
    if !repository.is_private {
        return Ok(true);
    }
    if user_ops::find_by_id(db, author_id)
        .await?
        .is_none_or(|user| !user.is_usable())
    {
        return Ok(false);
    }
    crate::repo::service::can_write_repo(db, repository, Some(author_id)).await
}

pub(crate) async fn resolve(db: &DatabaseConnection, pr: &pull_request::Model) -> Result<Source> {
    let Some(id) = pr.head_repo_id else {
        return Ok(Source::SameRepository);
    };
    let Some(repository) = repo_ops::find_by_id(db, id).await? else {
        return Ok(Source::Unavailable);
    };
    if !may_publish(db, &repository, pr.author_id).await? {
        return Ok(Source::Unavailable);
    }
    Ok(Source::Available(Box::new(repository)))
}

pub(crate) async fn is_live(db: &DatabaseConnection, pr: &pull_request::Model) -> Result<bool> {
    Ok(!matches!(resolve(db, pr).await?, Source::Unavailable))
}

// Reuse each (source, author) decision only within this synchronization call.
// This is not an authorization lease: diff/CI/merge still resolve independently.
async fn cached_is_live(
    db: &DatabaseConnection,
    pr: &pull_request::Model,
    verdicts: &mut HashMap<(Option<i64>, i64), bool>,
) -> Result<bool> {
    let key = (pr.head_repo_id, pr.author_id);
    if let Some(verdict) = verdicts.get(&key) {
        return Ok(*verdict);
    }
    let verdict = is_live(db, pr).await?;
    verdicts.insert(key, verdict);
    Ok(verdict)
}

/// Frozen rows owe no synchronize event/CI and must not keep the caller's
/// convergence loop retrying. Return only currently authorized PRs.
pub async fn refresh_open_heads(
    db: &DatabaseConnection,
    source_repo_id: i64,
    head_branch: &str,
    head_sha: Option<&str>,
) -> Result<pull_request_ops::OpenHeadShaRefresh> {
    let prs = pull_request_ops::list_open_with_head(db, source_repo_id, head_branch).await?;
    let mut stale_rows = 0;
    let mut verdicts = HashMap::new();
    for pr in prs {
        if !cached_is_live(db, &pr, &mut verdicts).await? || pr.head_sha.as_deref() == head_sha {
            continue;
        }
        if !pull_request_ops::compare_and_swap_open_head_sha(
            db,
            pr.id,
            pr.head_sha.as_deref(),
            head_sha,
        )
        .await?
        {
            stale_rows += 1;
        }
    }
    let mut open_prs = Vec::new();
    for pr in pull_request_ops::list_open_with_head(db, source_repo_id, head_branch).await? {
        if cached_is_live(db, &pr, &mut verdicts).await? {
            open_prs.push(pr);
        }
    }
    Ok(pull_request_ops::OpenHeadShaRefresh {
        open_prs,
        stale_rows,
    })
}

/// A suggestion can move a branch shared by several PRs. The actor's access
/// says nothing about each other PR author's publication authority.
pub(crate) async fn advance_open_heads(
    db: &DatabaseConnection,
    source_repo_id: i64,
    head_branch: &str,
    expected_head: &str,
    new_head: &str,
) -> Result<u64> {
    let mut changed = 0;
    let mut verdicts = HashMap::new();
    for pr in pull_request_ops::list_open_with_head(db, source_repo_id, head_branch).await? {
        if pr.head_sha.as_deref() == Some(expected_head)
            && cached_is_live(db, &pr, &mut verdicts).await?
            && pull_request_ops::compare_and_swap_open_head_sha(
                db,
                pr.id,
                Some(expected_head),
                Some(new_head),
            )
            .await?
        {
            changed += 1;
        }
    }
    Ok(changed)
}
