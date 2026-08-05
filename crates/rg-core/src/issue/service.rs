//! Issue service — business logic for Issue CRUD, labels, milestones, comments.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set, TransactionTrait};

use rg_db::entities::issue::{self, Model as Issue};
use rg_db::entities::issue_comment::{self, Model as Comment};
use rg_db::ops::{issue_comment_ops, issue_label_ops, issue_ops};

// ── Issue CRUD ──────────────────────────────────────────────────────────

/// Public issue view: the row itself plus label names read from their one
/// normalized owner (`issue_labels` → `labels`).
///
/// `labels` intentionally keeps the historic JSON-string wire shape. The web
/// client already normalizes that string to an array, and removing the storage
/// duplicate is not permission to break API consumers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IssueWithLabels {
    #[serde(flatten)]
    pub issue: Issue,
    pub labels: Option<String>,
}

fn serialize_label_names(names: Vec<String>) -> Result<Option<String>> {
    if names.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(&names)
        .map(Some)
        .context("serialize canonical issue label names")
}

pub async fn issues_with_labels(
    db: &DatabaseConnection,
    issues: Vec<Issue>,
) -> Result<Vec<IssueWithLabels>> {
    let issue_ids = issues.iter().map(|issue| issue.id).collect::<Vec<_>>();
    let mut names_by_issue = issue_label_ops::get_label_names_by_issue_ids(db, &issue_ids).await?;
    issues
        .into_iter()
        .map(|issue| {
            let names = names_by_issue.remove(&issue.id).unwrap_or_default();
            Ok(IssueWithLabels {
                issue,
                labels: serialize_label_names(names)?,
            })
        })
        .collect()
}

pub async fn issue_with_labels(db: &DatabaseConnection, issue: Issue) -> Result<IssueWithLabels> {
    issues_with_labels(db, vec![issue])
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("label enrichment lost the issue row"))
}

/// Create a new issue in the given repo.
pub async fn create_issue(
    db: &DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    title: String,
    body: Option<String>,
    labels: Option<Vec<String>>,
    milestone_id: Option<i64>,
) -> Result<Issue> {
    // `InvalidRequest`, not a bare `bail!`: this is the one outcome here the
    // caller *did* cause, and it is the only one allowed to become a 400. Every
    // other failure below is a query of ours and stays a 5xx.
    if title.trim().is_empty() {
        return Err(crate::error::invalid_request("issue title cannot be empty"));
    }

    // Resolved before anything is written: an unknown name is the caller's
    // mistake and must not leave a half-labelled issue behind. The junction is
    // the only stored owner; response views read the names back through it.
    let label_ids = match labels.as_deref() {
        Some(names) => Some(crate::label::service::resolve_label_ids(db, repo_id, names).await?),
        None => None,
    };

    let number = issue_ops::next_number(db, repo_id).await?;
    let model = issue::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        number: Set(number),
        title: Set(title),
        body: Set(body),
        state: Set("open".to_string()),
        author_id: Set(author_id),
        assignee_id: Set(None),
        milestone_id: Set(milestone_id),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        closed_at: Set(None),
        deleted_at: Set(None),
    };

    // FTS sync is handled by database triggers created in the migration chain.
    let txn = db.begin().await.context("db: begin transaction")?;

    let issue = model.insert(&txn).await.context("db: create issue")?;

    // The issue and its canonical junction rows commit together.
    if let Some(ids) = label_ids {
        issue_label_ops::set_labels(&txn, issue.id, ids).await?;
    }

    txn.commit().await.context("db: commit transaction")?;

    // Trigger issue.opened webhook
    let payload = serde_json::json!({
        "id": issue.id,
        "repo_id": issue.repo_id,
        "number": issue.number,
        "title": issue.title,
        "state": issue.state,
        "author_id": issue.author_id,
    });
    if let Err(e) = crate::webhook::service::trigger_issue_opened(db, repo_id, &payload).await {
        tracing::warn!(error = %format!("{e:#}"), "failed to trigger issue.opened webhook");
    }

    Ok(issue)
}

/// List issues for a repo, optionally filtered by state.
pub async fn list_issues(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    state: Option<&str>,
) -> Result<Vec<Issue>> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    issue_ops::list_by_repo(db, repo.id, state).await
}

/// Paginated list of issues. Returns (issues, total).
pub async fn list_issues_paginated(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    state: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Issue>, i64)> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    issue_ops::list_by_repo_paginated(db, repo.id, state, offset, limit).await
}

/// Paginated list of issues filtered by labels. Returns issues that have ALL specified labels.
///
/// A label name the repo does not have is refused, not dropped. Silently
/// skipping it turned `?labels=bug,typo` into `?labels=bug`: the client asked
/// for an intersection of two labels, got the whole of one, and nothing in the
/// response said the condition had been weakened.
///
/// The repo and state predicates live in SQL now, so `total` and the page are
/// counted the same way — see `find_issues_with_all_labels`.
pub async fn list_issues_filtered_by_labels(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    state: Option<&str>,
    label_names: &[String],
    offset: u64,
    limit: u64,
) -> Result<(Vec<Issue>, i64)> {
    let repo = resolve_repo(db, owner, repo_name).await?;

    // Resolve label names to IDs. Repeats collapse — `?labels=bug,bug` is one
    // condition, not a `HAVING COUNT(DISTINCT label_id) = 2` that can never
    // match. Shared with the write paths so one name cannot be legal on the way
    // in and unknown on the way out.
    let required_label_ids =
        crate::label::service::resolve_label_ids(db, repo.id, label_names).await?;

    if required_label_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }

    // Find issue IDs that have ALL required labels
    let (matching_issue_ids, total) = issue_label_ops::find_issues_with_all_labels(
        db,
        repo.id,
        &required_label_ids,
        state,
        offset,
        limit,
    )
    .await?;

    if matching_issue_ids.is_empty() {
        return Ok((Vec::new(), total));
    }

    // Fetch the actual issue models (batch query — avoids N+1). `find_by_ids`
    // answers in whatever order the database likes, so restore the page order
    // the paginating query asked for.
    let mut by_id: std::collections::HashMap<i64, Issue> =
        issue_ops::find_by_ids(db, &matching_issue_ids)
            .await?
            .into_iter()
            .map(|issue| (issue.id, issue))
            .collect();

    let issues: Vec<Issue> = matching_issue_ids
        .iter()
        .filter_map(|id| by_id.remove(id))
        .collect();

    Ok((issues, total))
}

/// Get a single issue by repo owner/name and issue number.
pub async fn get_issue(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    number: i64,
) -> Result<Issue> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    issue_ops::find_by_repo_and_number(db, repo.id, number)
        .await?
        .ok_or_else(|| crate::error::not_found("issue"))
}

/// Update an issue's title, body, state, labels, assignee, or milestone.
///
/// `delivery_tracker` is where the milestone-completion watch fan-out is
/// detached to; `None` = the process-global delivery tracker. See
/// [`crate::notification::spawn_notify_watchers`].
#[allow(clippy::too_many_arguments)]
pub async fn update_issue(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    number: i64,
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    labels: Option<Vec<String>>,
    assignee_id: Option<Option<i64>>,
    milestone_id: Option<Option<i64>>,
    delivery_tracker: Option<&crate::task_tracker::TaskTracker>,
) -> Result<Issue> {
    let existing = get_issue(db, owner, repo_name, number).await?;
    let issue_id = existing.id;
    let issue_repo_id = existing.repo_id;
    let issue_milestone_id = existing.milestone_id;
    let was_open = existing.state == "open";

    // Convert to ActiveModel — all non-PK fields become Unchanged; we Set() only changed fields.
    let mut active: issue::ActiveModel = existing.into();

    if let Some(t) = title {
        if t.trim().is_empty() {
            return Err(crate::error::invalid_request("issue title cannot be empty"));
        }
        active.title = Set(t);
    }
    if let Some(b) = body {
        active.body = Set(Some(b));
    }
    if let Some(ref s) = state {
        if s != "open" && s != "closed" {
            return Err(crate::error::invalid_request(format!(
                "invalid issue state: {s}, must be open or closed"
            )));
        }
        active.state = Set(s.clone());
        if s == "closed" {
            active.closed_at = Set(Some(Utc::now()));
        } else {
            active.closed_at = Set(None);
        }
    }
    // Resolved before the update statement, and applied inside the same
    // transaction as it — see `create_issue`. An unknown name refuses the whole
    // edit instead of quietly narrowing it.
    let mut label_ids: Option<Vec<i64>> = None;
    if let Some(l) = labels {
        label_ids = Some(crate::label::service::resolve_label_ids(db, issue_repo_id, &l).await?);
    }
    if let Some(a) = assignee_id {
        active.assignee_id = Set(a);
    }
    if let Some(m) = milestone_id {
        active.milestone_id = Set(m);
    }

    active.updated_at = Set(Utc::now());

    let txn = db.begin().await.context("db: begin transaction")?;
    let updated = active.update(&txn).await.context("db: update issue")?;
    if let Some(ids) = label_ids {
        issue_label_ops::set_labels(&txn, issue_id, ids).await?;
    }
    txn.commit().await.context("db: commit transaction")?;

    // FTS sync is handled by database triggers created in the migration chain.

    // Post-update side effects (non-fatal)
    if let Some(ref s) = state {
        if was_open && s == "closed" {
            let close_payload = serde_json::json!({
                "id": updated.id,
                "repo_id": issue_repo_id,
                "number": updated.number,
                "title": updated.title,
                "state": s,
            });
            if let Err(e) =
                crate::webhook::service::trigger_issue_closed(db, issue_repo_id, &close_payload)
                    .await
            {
                tracing::warn!(error = %format!("{e:#}"), "failed to trigger issue.closed webhook");
            }

            if let Some(mid) = issue_milestone_id {
                if let Ok(remaining) =
                    rg_db::ops::milestone_ops::count_open_by_milestone(db, issue_repo_id, mid).await
                {
                    if remaining == 0 {
                        if let Err(e) =
                            notify_milestone_closed(db, issue_repo_id, mid, delivery_tracker).await
                        {
                            tracing::warn!(milestone_id = %mid, error = %format!("{e:#}"), "failed to notify milestone closed");
                        }
                    }
                }
            }
        }
    }

    Ok(updated)
}

// ── Issue Comments ──────────────────────────────────────────────────────

/// Add a comment to an issue.
pub async fn add_comment(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    issue_number: i64,
    author_id: i64,
    body: String,
) -> Result<Comment> {
    if body.trim().is_empty() {
        return Err(crate::error::invalid_request(
            "comment body cannot be empty",
        ));
    }

    let issue = get_issue(db, owner, repo_name, issue_number).await?;

    let model = issue_comment::ActiveModel {
        id: sea_orm::NotSet,
        issue_id: Set(issue.id),
        author_id: Set(author_id),
        body: Set(body),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
    };

    let comment = issue_comment_ops::create(db, model).await?;

    // Trigger issue.comment webhook
    let issue_payload = serde_json::json!({
        "id": issue.id,
        "repo_id": issue.repo_id,
        "number": issue.number,
        "title": issue.title,
    });
    let comment_payload = serde_json::json!({
        "id": comment.id,
        "body": comment.body,
        "author_id": comment.author_id,
    });
    if let Err(e) = crate::webhook::service::trigger_issue_comment(
        db,
        issue.repo_id,
        &issue_payload,
        &comment_payload,
    )
    .await
    {
        tracing::warn!(error = %format!("{e:#}"), "failed to trigger issue.comment webhook");
    }

    Ok(comment)
}

/// List comments for an issue.
pub async fn list_comments(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    issue_number: i64,
) -> Result<Vec<Comment>> {
    let issue = get_issue(db, owner, repo_name, issue_number).await?;
    issue_comment_ops::list_by_issue(db, issue.id).await
}

// `update_comment(db, comment_id, body)` and `delete_comment(db, comment_id)`
// used to live here. Both were dead — no route, no caller anywhere in the
// workspace — and both were shaped so that a scope could not be passed even if
// a caller wanted to: the comment id is global, and neither signature had room
// for the issue or the repository it belongs to.
//
// That is the same primitive `time_tracking::service::delete_time_entry(db, id)`
// was before it became `delete_time_entry(db, issue_id, id)`, and the reason it
// mattered there was not the primitive itself but the handler written to its
// shape. Editing a comment is a feature this forge will grow eventually; when
// it does, the signature has to carry `issue_id` and the body has to verify
// `comment.issue_id` before touching the row — see `delete_time_entry` for the
// shape, and `api::attachments` for the call-site anchoring that goes with it.

// ── Helpers ─────────────────────────────────────────────────────────────

/// Notify watchers and trigger webhook when all issues in a milestone are closed.
async fn notify_milestone_closed(
    db: &DatabaseConnection,
    repo_id: i64,
    milestone_id: i64,
    delivery_tracker: Option<&crate::task_tracker::TaskTracker>,
) -> Result<()> {
    // Trigger milestone.closed webhook
    let payload = serde_json::json!({
        "id": milestone_id,
        "repo_id": repo_id,
    });
    if let Err(e) = crate::webhook::service::trigger_milestone_closed(db, repo_id, &payload).await {
        tracing::warn!(error = %format!("{e:#}"), "failed to trigger milestone.closed webhook");
    }

    // Notify watchers about milestone completion. Missing rows mean that there
    // is no longer context to announce; a failed read is a different outcome
    // and must remain visible even though this best-effort side effect does not
    // unwind the issue update that was already committed.
    let milestone = match rg_db::ops::milestone_ops::find_by_id(db, milestone_id).await {
        Ok(Some(milestone)) => milestone,
        Ok(None) => return Ok(()),
        Err(error) => {
            tracing::warn!(
                repo_id,
                milestone_id,
                error = %format!("{error:#}"),
                "milestone lookup failed while preparing notification"
            );
            return Ok(());
        }
    };
    let repo = match rg_db::entities::repository::Entity::find_by_id(repo_id)
        .one(db)
        .await
    {
        Ok(Some(repo)) => repo,
        Ok(None) => return Ok(()),
        Err(error) => {
            tracing::warn!(
                repo_id,
                milestone_id,
                error = %format!("{error:#}"),
                "repository lookup failed while preparing milestone notification"
            );
            return Ok(());
        }
    };

    // Detached: closing the last issue of a milestone answers an HTTP request,
    // and the fan-out below is a read check plus an insert for every subscriber.
    crate::notification::spawn_notify_watchers(
        db,
        delivery_tracker.unwrap_or_else(|| crate::task_tracker::delivery_tracker()),
        crate::notification::WatchEvent {
            repo_id,
            author_name: String::new(),
            title: format!("Milestone {} in {}", "closed", repo.name),
            notification_type: "milestone".to_string(),
            body: Some(format!("Milestone '{}' {}", milestone.title, "closed")),
        },
    );

    Ok(())
}

async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<rg_db::entities::repository::Model> {
    crate::repo::service::find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))
}

#[cfg(test)]
mod notification_lookup_tests {
    use super::notify_milestone_closed;
    use crate::test_support::{migrated_memory_database, CapturedLogs};
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, ConnectionTrait, NotSet, Set};

    #[tokio::test(flavor = "current_thread")]
    async fn milestone_lookup_failure_is_logged_without_failing_issue_completion() {
        let db = migrated_memory_database().await;
        db.execute_unprepared("DROP TABLE milestones")
            .await
            .expect("drop milestones");
        let (logs, _guard) = CapturedLogs::capture();

        notify_milestone_closed(&db, 7, 11, None)
            .await
            .expect("notification lookup failure stays best-effort");

        let rendered = logs.rendered();
        for expected in [
            "milestone lookup failed while preparing notification",
            "repo_id=7",
            "milestone_id=11",
            "db: find milestone by id",
            "no such table: milestones",
        ] {
            assert!(
                rendered.contains(expected),
                "missing `{expected}` in {rendered}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn repository_lookup_failure_is_logged_without_failing_issue_completion() {
        let db = migrated_memory_database().await;
        let owner = rg_db::ops::user_ops::create_user(
            &db,
            "milestone-owner",
            "milestone-owner@example.invalid",
            "",
            "Milestone Owner",
        )
        .await
        .expect("create owner");
        let now = Utc::now();
        let repo = rg_db::entities::repository::ActiveModel {
            id: NotSet,
            owner_id: Set(owner.id),
            name: Set("milestone-repo".to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        }
        .insert(&db)
        .await
        .expect("create repository");
        let milestone = rg_db::entities::milestone::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            title: Set("v1".to_string()),
            description: Set(None),
            state: Set("closed".to_string()),
            due_date: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db)
        .await
        .expect("create milestone");
        db.execute_unprepared("PRAGMA foreign_keys = OFF; DROP TABLE repositories")
            .await
            .expect("break repository lookup without removing milestone");
        let (logs, _guard) = CapturedLogs::capture();

        notify_milestone_closed(&db, repo.id, milestone.id, None)
            .await
            .expect("notification lookup failure stays best-effort");

        let rendered = logs.rendered();
        for expected in [
            "repository lookup failed while preparing milestone notification",
            &format!("repo_id={}", repo.id),
            &format!("milestone_id={}", milestone.id),
            "no such table: repositories",
        ] {
            assert!(
                rendered.contains(expected),
                "missing `{expected}` in {rendered}"
            );
        }
    }
}
