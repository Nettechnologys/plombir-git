//! Database operations for PR reviews.

use anyhow::{Context, Result};
use sea_orm::*;

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

/// List reviews by PR and reviewer.
pub async fn list_by_pr_and_reviewer(
    db: &DatabaseConnection,
    pr_id: i64,
    reviewer_id: i64,
) -> Result<Vec<PrReview>> {
    ReviewEntity::find()
        .filter(pr_review::Column::PrId.eq(pr_id))
        .filter(pr_review::Column::ReviewerId.eq(reviewer_id))
        .order_by_asc(pr_review::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list reviews by PR and reviewer")
}

/// Count approvals for a PR.
pub async fn count_approvals(db: &DatabaseConnection, pr_id: i64) -> Result<i64> {
    count_current_approvals(db, pr_id, None).await
}

/// Count at most one latest approval per live reviewer for the current head commit.
/// A later `request_changes` from the same reviewer supersedes their approval.
/// Historical reviews outlive their authors, but a missing, deactivated, or
/// retiring account no longer contributes a current authorization verdict.
pub async fn count_current_approvals(
    db: &DatabaseConnection,
    pr_id: i64,
    head_sha: Option<&str>,
) -> Result<i64> {
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
                && match head_sha {
                    Some(sha) => review.commit_id.as_deref() == Some(sha),
                    None => review.commit_id.is_none(),
                }
        })
        .map(|review| review.reviewer_id)
        .collect::<Vec<_>>();
    if candidate_reviewers.is_empty() {
        return Ok(0);
    }

    let count = UserEntity::find()
        .filter(user::Column::Id.is_in(candidate_reviewers))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: count live PR reviewers")?;
    Ok(count as i64)
}

/// Create a new review.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<PrReview> {
    model.insert(db).await.context("db: create PR review")
}

/// Delete a review by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    ReviewEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete review")?;
    Ok(())
}
