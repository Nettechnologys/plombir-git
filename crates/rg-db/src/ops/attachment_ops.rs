//! Database operations for Issue, pull-request and comment attachments.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::attachment::{
    self, ActiveModel as AttachmentActiveModel, Entity as AttachmentEntity,
    Model as AttachmentModel,
};

pub async fn create(
    db: &DatabaseConnection,
    model: AttachmentActiveModel,
) -> Result<AttachmentModel> {
    model.insert(db).await.context("db: create attachment")
}

pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<AttachmentModel>> {
    AttachmentEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find attachment by id")
}

pub async fn find_by_uuid(db: &DatabaseConnection, uuid: &str) -> Result<Option<AttachmentModel>> {
    AttachmentEntity::find()
        .filter(attachment::Column::Uuid.eq(uuid))
        .one(db)
        .await
        .context("db: find attachment by uuid")
}

pub async fn list_by_issue(
    db: &DatabaseConnection,
    repo_id: i64,
    issue_id: i64,
) -> Result<Vec<AttachmentModel>> {
    list_for_target(db, repo_id, attachment::Column::IssueId, issue_id).await
}

pub async fn list_by_pull_request(
    db: &DatabaseConnection,
    repo_id: i64,
    pull_request_id: i64,
) -> Result<Vec<AttachmentModel>> {
    list_for_target(
        db,
        repo_id,
        attachment::Column::PullRequestId,
        pull_request_id,
    )
    .await
}

pub async fn list_by_issue_comment(
    db: &DatabaseConnection,
    repo_id: i64,
    comment_id: i64,
) -> Result<Vec<AttachmentModel>> {
    list_for_target(db, repo_id, attachment::Column::IssueCommentId, comment_id).await
}

pub async fn list_by_review_comment(
    db: &DatabaseConnection,
    repo_id: i64,
    comment_id: i64,
) -> Result<Vec<AttachmentModel>> {
    list_for_target(db, repo_id, attachment::Column::ReviewCommentId, comment_id).await
}

async fn list_for_target(
    db: &DatabaseConnection,
    repo_id: i64,
    column: attachment::Column,
    target_id: i64,
) -> Result<Vec<AttachmentModel>> {
    AttachmentEntity::find()
        .filter(attachment::Column::RepoId.eq(repo_id))
        .filter(column.eq(target_id))
        .order_by_asc(attachment::Column::CreatedAt)
        .order_by_asc(attachment::Column::Id)
        .all(db)
        .await
        .context("db: list attachments for target")
}

pub async fn repo_size(db: &DatabaseConnection, repo_id: i64) -> Result<i64> {
    let sizes = AttachmentEntity::find()
        .select_only()
        .column(attachment::Column::Size)
        .filter(attachment::Column::RepoId.eq(repo_id))
        .into_tuple::<i64>()
        .all(db)
        .await
        .context("db: list repository attachment sizes")?;
    Ok(sizes
        .into_iter()
        .fold(0_i64, |total, size| total.saturating_add(size)))
}

pub async fn increment_download_count(db: &DatabaseConnection, id: i64) -> Result<()> {
    AttachmentEntity::update_many()
        .col_expr(
            attachment::Column::DownloadCount,
            Expr::col(attachment::Column::DownloadCount).add(1),
        )
        .filter(attachment::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: increment attachment download count")?;
    Ok(())
}

pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    AttachmentEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete attachment")?;
    Ok(())
}
