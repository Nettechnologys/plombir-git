//! Database operations for PR reviews.

use anyhow::{Context, Result};
use sea_orm::{prelude::DateTimeUtc, *};

use crate::entities::{
    pr_review::{self, ActiveModel, Entity as ReviewEntity, Model as PrReview},
    user::{self, Entity as UserEntity},
};

/// Find a review by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<PrReview>> {
    ReviewEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find review by id")
}

/// List all reviews for a PR, ordered by creation time.
pub async fn list_by_pr(db: &DatabaseConnection, pr_id: i64) -> Result<Vec<PrReview>> {
    ReviewEntity::find()
        .filter(pr_review::Column::PrId.eq(pr_id))
        .order_by_asc(pr_review::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list reviews by PR")
}

/// The live accounts whose latest verdict on the current head commit is an
/// approval — at most one entry per reviewer, ordered by account id.
/// A later `request_changes` from the same reviewer supersedes their approval.
/// Historical reviews outlive their authors, but a missing, deactivated, or
/// retiring account no longer contributes a current authorization verdict.
///
/// A dismissed review is history too: it stays the reviewer's latest verdict —
/// dismissing an approval does not resurrect an older one from the same person
/// — but a withdrawn verdict authorizes nothing (card_dc0f5d58e5f4).
///
/// This answers "who approved this head", not "how many approvals count": the
/// accounts come back whole because branch protection still has to ask who each
/// of them is — a bot, the author's side, a non-writer — before counting them
/// (`rg_core::branch_protection::service`, card_25220b69c295).
pub async fn current_approvers(
    db: &DatabaseConnection,
    pr_id: i64,
    head_sha: Option<&str>,
) -> Result<Vec<user::Model>> {
    let reviews = list_by_pr(db, pr_id).await?;
    let mut latest = std::collections::HashMap::new();
    for review in reviews {
        if matches!(review.action.as_str(), "approve" | "request_changes") {
            latest.insert(review.reviewer_id, review);
        }
    }
    let candidate_reviewers = latest
        .values()
        .filter(|review| {
            review.action == "approve"
                && review.dismissed_at.is_none()
                && match head_sha {
                    Some(sha) => review.commit_id.as_deref() == Some(sha),
                    None => review.commit_id.is_none(),
                }
        })
        .map(|review| review.reviewer_id)
        .collect::<Vec<_>>();
    if candidate_reviewers.is_empty() {
        return Ok(Vec::new());
    }

    UserEntity::find()
        .filter(user::Column::Id.is_in(candidate_reviewers))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .order_by_asc(user::Column::Id)
        .all(db)
        .await
        .context("db: load live PR approvers")
}

/// Create a new review.
pub async fn create<C: ConnectionTrait>(db: &C, model: ActiveModel) -> Result<PrReview> {
    model.insert(db).await.context("db: create PR review")
}

/// Withdraw a review, recording when and by whom.
///
/// Idempotent on purpose: the first dismissal is the one that counts, so a
/// second call leaves the original `dismissed_at` / `dismissed_by` in place and
/// returns the row as it stands. Runs on any connection or transaction handle
/// so the caller can bracket it with the event it writes.
pub async fn mark_dismissed<C: ConnectionTrait>(
    db: &C,
    review_id: i64,
    dismissed_by: i64,
    dismissed_at: DateTimeUtc,
) -> Result<Option<PrReview>> {
    let Some(review) = ReviewEntity::find_by_id(review_id)
        .one(db)
        .await
        .context("db: find review to dismiss")?
    else {
        return Ok(None);
    };
    if review.dismissed_at.is_some() {
        return Ok(Some(review));
    }
    let mut active: ActiveModel = review.into();
    active.dismissed_at = Set(Some(dismissed_at));
    active.dismissed_by = Set(Some(dismissed_by));
    let updated = active.update(db).await.context("db: dismiss PR review")?;
    Ok(Some(updated))
}
