//! Editing and deleting what was written into an issue or a pull request
//! (card_60961272e1ba).
//!
//! A comment is its author's to edit and to delete; a repository
//! administrator may do both too, which is how a pasted secret or abuse gets
//! taken down. Whether the caller administers the repository is the HTTP
//! gate's decision and arrives here as `administers` — this module decides
//! only the row-level half, "is it yours".
//!
//! A review comment's text is also copied into the pull request's timeline
//! event; editing it rewrites that copy and deleting it removes the event, or a
//! secret taken down from the comment would still be read from the timeline.
//!
//! Deleting a comment or an issue removes the attachments uploaded into it
//! first, through [`crate::attachment::delete_attachment`]: the attachment rows
//! would go with their parent by foreign key, and the stored files with them
//! would be left behind with nothing that can find them again.

use anyhow::Result;
use sea_orm::DatabaseConnection;

use rg_db::entities::issue::Model as Issue;
use rg_db::entities::issue_comment::Model as IssueComment;
use rg_db::entities::review_comment::Model as ReviewComment;

use crate::attachment::AttachmentTarget;
use crate::blob_storage::BlobStorage;

/// Who is acting on a comment.
#[derive(Clone, Copy, Debug)]
pub struct Moderator {
    pub actor_id: i64,
    /// The caller administers the repository the comment lives in.
    pub administers: bool,
}

impl Moderator {
    fn may_change(self, author_id: i64, what: &str) -> Result<()> {
        if self.administers || self.actor_id == author_id {
            return Ok(());
        }
        Err(crate::error::forbidden(format!(
            "only its author or a repository administrator may change this {what}"
        )))
    }
}

fn checked_body(body: &str) -> Result<()> {
    if body.trim().is_empty() {
        return Err(crate::error::invalid_request(
            "comment body cannot be empty",
        ));
    }
    Ok(())
}

/// The issue comment `comment_id`, if it lives in `repo_id`.
async fn issue_comment_in(
    db: &DatabaseConnection,
    repo_id: i64,
    comment_id: i64,
) -> Result<IssueComment> {
    let comment = rg_db::ops::issue_comment_ops::find_by_id(db, comment_id)
        .await?
        .ok_or_else(|| crate::error::not_found("comment"))?;
    rg_db::ops::issue_ops::find_by_id(db, comment.issue_id)
        .await?
        .filter(|issue| issue.repo_id == repo_id)
        .ok_or_else(|| crate::error::not_found("comment"))?;
    Ok(comment)
}

/// The review comment `comment_id`, if it is on pull request `number` of
/// `repo_id`.
async fn review_comment_in(
    db: &DatabaseConnection,
    repo_id: i64,
    number: i64,
    comment_id: i64,
) -> Result<ReviewComment> {
    let comment = rg_db::ops::review_comment_ops::find_by_id(db, comment_id)
        .await?
        .ok_or_else(|| crate::error::not_found("comment"))?;
    rg_db::ops::pull_request_ops::find_by_id(db, comment.pr_id)
        .await?
        .filter(|pr| pr.repo_id == repo_id && pr.number == number)
        .ok_or_else(|| crate::error::not_found("comment"))?;
    Ok(comment)
}

/// Replace the body of an issue comment.
pub async fn edit_issue_comment(
    db: &DatabaseConnection,
    repo_id: i64,
    comment_id: i64,
    moderator: Moderator,
    body: &str,
) -> Result<IssueComment> {
    checked_body(body)?;
    let comment = issue_comment_in(db, repo_id, comment_id).await?;
    moderator.may_change(comment.author_id, "comment")?;
    rg_db::ops::issue_comment_ops::update_body(db, comment.id, body)
        .await?
        .ok_or_else(|| crate::error::not_found("comment"))
}

/// Delete an issue comment and the attachments uploaded into it.
pub async fn delete_issue_comment(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_id: i64,
    comment_id: i64,
    moderator: Moderator,
) -> Result<IssueComment> {
    let comment = issue_comment_in(db, repo_id, comment_id).await?;
    moderator.may_change(comment.author_id, "comment")?;
    let target = AttachmentTarget::IssueComment(comment.id);
    for attachment in
        rg_db::ops::attachment_ops::list_by_issue_comment(db, repo_id, comment.id).await?
    {
        crate::attachment::delete_attachment(db, storage, repo_id, target, attachment.id).await?;
    }
    if !rg_db::ops::issue_comment_ops::delete_by_id(db, comment.id).await? {
        return Err(crate::error::not_found("comment"));
    }
    Ok(comment)
}

/// Replace the body of a review comment.
pub async fn edit_review_comment(
    db: &DatabaseConnection,
    repo_id: i64,
    number: i64,
    comment_id: i64,
    moderator: Moderator,
    body: &str,
) -> Result<ReviewComment> {
    checked_body(body)?;
    let comment = review_comment_in(db, repo_id, number, comment_id).await?;
    moderator.may_change(comment.author_id, "comment")?;
    let edited = rg_db::ops::review_comment_ops::update_body(db, comment.id, body)
        .await?
        .ok_or_else(|| crate::error::not_found("comment"))?;
    // The timeline event that recorded the comment kept a copy of its text.
    let events = rg_db::ops::pr_event_ops::ids_for_comment(db, comment.pr_id, comment.id).await?;
    rg_db::ops::pr_event_ops::replace_bodies(db, &events, body).await?;
    Ok(edited)
}

/// Delete a review comment and its attachments.
///
/// A comment others replied to opens their thread — its position, its
/// resolution — and taking it away would strand the replies. It is refused
/// until the replies are gone; editing it stays possible.
pub async fn delete_review_comment(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_id: i64,
    number: i64,
    comment_id: i64,
    moderator: Moderator,
) -> Result<ReviewComment> {
    let comment = review_comment_in(db, repo_id, number, comment_id).await?;
    moderator.may_change(comment.author_id, "comment")?;
    if rg_db::ops::review_comment_ops::has_replies(db, comment.id).await? {
        return Err(crate::error::conflict(
            "this comment opens a thread others replied to; delete the replies first",
        ));
    }
    let target = AttachmentTarget::ReviewComment(comment.id);
    for attachment in
        rg_db::ops::attachment_ops::list_by_review_comment(db, repo_id, comment.id).await?
    {
        crate::attachment::delete_attachment(db, storage, repo_id, target, attachment.id).await?;
    }
    if !rg_db::ops::review_comment_ops::delete_by_id(db, comment.id).await? {
        return Err(crate::error::not_found("comment"));
    }
    // And the timeline events that copied its text.
    let events = rg_db::ops::pr_event_ops::ids_for_comment(db, comment.pr_id, comment.id).await?;
    rg_db::ops::pr_event_ops::delete_by_ids(db, &events).await?;
    Ok(comment)
}

/// Delete issue `number` of `repo_id` with everything in it — its comments,
/// labels, attachments. Repository administrators only; the caller has decided
/// that. The search index follows by its own trigger.
pub async fn delete_issue(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_id: i64,
    number: i64,
) -> Result<Issue> {
    let issue = rg_db::ops::issue_ops::find_by_repo_and_number(db, repo_id, number)
        .await?
        .ok_or_else(|| crate::error::not_found("issue"))?;
    for comment in rg_db::ops::issue_comment_ops::list_by_issue(db, issue.id).await? {
        let target = AttachmentTarget::IssueComment(comment.id);
        for attachment in
            rg_db::ops::attachment_ops::list_by_issue_comment(db, repo_id, comment.id).await?
        {
            crate::attachment::delete_attachment(db, storage, repo_id, target, attachment.id)
                .await?;
        }
    }
    let target = AttachmentTarget::Issue(issue.id);
    for attachment in rg_db::ops::attachment_ops::list_by_issue(db, repo_id, issue.id).await? {
        crate::attachment::delete_attachment(db, storage, repo_id, target, attachment.id).await?;
    }
    if !rg_db::ops::issue_ops::delete_by_id(db, issue.id).await? {
        return Err(crate::error::not_found("issue"));
    }
    // Notifications and subscriptions point at the issue by a polymorphic
    // subject, which no foreign key can follow.
    crate::notification::thread::forget_subject(
        db,
        crate::notification::thread::SubjectKind::Issue,
        issue.id,
    )
    .await?;
    Ok(issue)
}
