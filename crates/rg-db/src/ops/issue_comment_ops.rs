//! Database operations for issue comments.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::issue_comment::{
    self, ActiveModel, Entity as CommentEntity, Model as Comment,
};

/// Find a comment by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Comment>> {
    CommentEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find issue comment by id")
}

/// List comments for an issue, ordered by creation time.
pub async fn list_by_issue(db: &DatabaseConnection, issue_id: i64) -> Result<Vec<Comment>> {
    CommentEntity::find()
        .filter(issue_comment::Column::IssueId.eq(issue_id))
        .order_by_asc(issue_comment::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list issue comments")
}

/// Create a new comment.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Comment> {
    model.insert(db).await.context("db: create issue comment")
}

/// Replace a comment's body, stamping `updated_at` — the edit marker readers
/// show. `None` when the row is gone.
pub async fn update_body(db: &DatabaseConnection, id: i64, body: &str) -> Result<Option<Comment>> {
    let updated = CommentEntity::update_many()
        .col_expr(issue_comment::Column::Body, sea_query::Expr::value(body))
        .col_expr(
            issue_comment::Column::UpdatedAt,
            sea_query::Expr::value(chrono::Utc::now()),
        )
        .filter(issue_comment::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: edit issue comment")?;
    if updated.rows_affected == 0 {
        return Ok(None);
    }
    find_by_id(db, id).await
}

/// Delete a comment. `false` when it was already gone.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let deleted = CommentEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete issue comment")?;
    Ok(deleted.rows_affected == 1)
}
