//! Database operations for requested pull-request reviewers.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::pr_reviewer_request::{
    self, ActiveModel, Entity as RequestEntity, Model as ReviewerRequest,
};

pub async fn list_by_pr(db: &DatabaseConnection, pr_id: i64) -> Result<Vec<ReviewerRequest>> {
    RequestEntity::find()
        .filter(pr_reviewer_request::Column::PrId.eq(pr_id))
        .order_by_asc(pr_reviewer_request::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list requested reviewers")
}

pub async fn find(
    db: &DatabaseConnection,
    pr_id: i64,
    reviewer_id: i64,
) -> Result<Option<ReviewerRequest>> {
    RequestEntity::find()
        .filter(pr_reviewer_request::Column::PrId.eq(pr_id))
        .filter(pr_reviewer_request::Column::ReviewerId.eq(reviewer_id))
        .one(db)
        .await
        .context("db: find requested reviewer")
}

pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<ReviewerRequest> {
    model.insert(db).await.context("db: request reviewer")
}

pub async fn delete(db: &DatabaseConnection, pr_id: i64, reviewer_id: i64) -> Result<u64> {
    let result = RequestEntity::delete_many()
        .filter(pr_reviewer_request::Column::PrId.eq(pr_id))
        .filter(pr_reviewer_request::Column::ReviewerId.eq(reviewer_id))
        .exec(db)
        .await
        .context("db: remove requested reviewer")?;
    Ok(result.rows_affected)
}
