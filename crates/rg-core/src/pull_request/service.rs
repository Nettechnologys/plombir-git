//! Pull request service — PR creation, diff, merge strategies.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, Set, TransactionTrait,
};
use std::collections::HashMap;

use crate::error::NotFound;
use rg_git::protocol::receive_pack::RefUpdate;

use rg_db::entities::pull_request::{self, Model as PullRequest};
use rg_db::entities::repository as repo_entity;
use rg_db::ops::pull_request_ops;

// ── PR CRUD ─────────────────────────────────────────────────────────────

/// Create a new pull request.
///
/// If `head_repo_id` is provided, this is a fork PR (cross-repository).
/// The `head_branch` should contain just the branch name (not `owner:branch` format).
///
/// `delivery_tracker` carries the watch fan-out off this call's critical path —
/// see [`announce_pr_to_watchers`] for what `None` means.
#[allow(clippy::too_many_arguments)]
pub async fn create_pr(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    repo_id: i64,
    author_id: i64,
    title: String,
    body: Option<String>,
    head_branch: String,
    base_branch: String,
    head_repo_id: Option<i64>,
    is_draft: bool,
    delivery_tracker: Option<&crate::task_tracker::TaskTracker>,
) -> Result<PullRequest> {
    // The two things the caller can get wrong here carry `InvalidRequest`;
    // everything after them is a query or a git read of ours, and a failure
    // there must not be reported as a malformed request.
    if title.trim().is_empty() {
        return Err(crate::error::invalid_request("PR title cannot be empty"));
    }
    if head_branch == base_branch {
        return Err(crate::error::invalid_request(
            "head and base branches cannot be the same",
        ));
    }
    // Both branch names are caller input and both outlive this call: the head is
    // handed to `try_get_branch_sha`, where a spelling gix rejects used to
    // surface as an operational 500, and the base is only ever written to the
    // row — where it is not read again until a merge, a diff or a CI trigger
    // fails on it far from the request that stored it. Checked before the first
    // query so neither can be persisted.
    //
    // Distinct from the "not found" answers further down: this says the name
    // cannot exist, those say no such branch exists here.
    for (label, branch) in [("head", &head_branch), ("base", &base_branch)] {
        rg_git::refname::validate_branch_name(branch)
            .map_err(|_| crate::error::invalid_request(format!("invalid {label} branch name")))?;
    }

    let target_repo = repo_entity::Entity::find_by_id(repo_id)
        .one(db)
        .await?
        .context("target repository not found")?;
    let target_namespace = repository_namespace(db, &target_repo).await?;
    let target_path = repo_root.join(format!("{target_namespace}/{}.git", target_repo.name));

    // Resolve head SHA (for same-repo PRs, look up branch; for fork PRs, use the head repo).
    // A missing branch is a caller error, but an unreadable repository or ref store
    // is ours: `try_get_branch_sha` preserves that distinction instead of turning both
    // into a nullable `head_sha` on a newly-created PR.
    let head_sha = if let Some(head_repo_id) = head_repo_id {
        // For fork PRs, resolve from the fork repo's git data
        let head_repo = repo_entity::Entity::find_by_id(head_repo_id)
            .one(db)
            .await?
            .context("head repository not found")?;
        let head_namespace = repository_namespace(db, &head_repo).await?;
        let head_path = repo_root.join(format!("{head_namespace}/{}.git", head_repo.name));
        crate::repo::service::try_get_branch_sha(&head_path, &head_branch)?.ok_or_else(|| {
            crate::error::invalid_request(format!("head branch '{head_branch}' not found"))
        })?
    } else {
        crate::repo::service::try_get_branch_sha(&target_path, &head_branch)?.ok_or_else(|| {
            crate::error::invalid_request(format!("head branch '{head_branch}' not found"))
        })?
    };

    let model = pull_request::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        // Allocated against the UNIQUE key by the insert below, not here: the
        // number is only free until someone else's insert takes it.
        number: sea_orm::NotSet,
        title: Set(title),
        body: Set(body),
        state: Set("open".to_string()),
        is_draft: Set(is_draft),
        auto_merge_enabled: Set(false),
        auto_merge_strategy: Set(None),
        auto_merge_enabled_by_id: Set(None),
        auto_merge_enabled_at: Set(None),
        author_id: Set(author_id),
        reviewer_id: Set(None),
        head_branch: Set(head_branch),
        base_branch: Set(base_branch),
        head_sha: Set(Some(head_sha)),
        merge_strategy: Set(None),
        merge_commit_sha: Set(None),
        head_repo_id: Set(head_repo_id),
        ci_approved_sha: Set(None),
        ci_approved_by: Set(None),
        ci_approved_at: Set(None),
        milestone_id: Set(None),
        labels: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        closed_at: Set(None),
        merged_at: Set(None),
    };

    let pr = insert_with_repo_number(db, repo_id, model).await?;
    if let Err(error) = rg_db::ops::pr_event_ops::record(
        db,
        pr.repo_id,
        pr.id,
        Some(author_id),
        "pull_request_opened",
        pr.body.clone(),
        serde_json::json!({
            "title": pr.title,
            "head_sha": pr.head_sha,
            "head_branch": pr.head_branch,
            "base_branch": pr.base_branch,
            "draft": pr.is_draft
        }),
    )
    .await
    {
        tracing::error!(
            repo_id = pr.repo_id,
            pr_id = pr.id,
            actor_id = author_id,
            event_type = "pull_request_opened",
            error = %format!("{error:#}"),
            "pull request was created, but its timeline event could not be recorded"
        );
    }

    // Trigger pull_request.opened webhook
    let payload = serde_json::json!({
        "id": pr.id,
        "repo_id": pr.repo_id,
        "number": pr.number,
        "title": pr.title,
        "state": pr.state,
        "head_branch": pr.head_branch,
        "base_branch": pr.base_branch,
        "head_repo_id": pr.head_repo_id,
        "author_id": pr.author_id,
    });
    if let Err(e) = crate::webhook::service::trigger_pr_opened(db, repo_id, &payload).await {
        tracing::warn!(error = %format!("{e:#}"), "failed to trigger PR opened webhook");
    }

    announce_pr_to_watchers(
        db,
        delivery_tracker,
        repo_id,
        &target_repo.name,
        Some(author_id),
        pr.number,
        &pr.title,
        "opened",
    );

    Ok(pr)
}

/// Insert one pull request under a freshly allocated repository-local number.
///
/// [`pull_request_ops::next_number`] answers `MAX(number) + 1` in a statement of
/// its own, so two creates in the same repository can read the same answer and
/// the UNIQUE index on `(repo_id, number)` refuses the second insert. That
/// collision is a race this server opened between its own read and its own
/// write, not a malformed request: the loser re-reads the maximum and takes the
/// next free number instead of handing a correct caller a 500 to retry by hand.
///
/// Only a backend-confirmed UNIQUE violation (plus the backend's own
/// retryable-transaction outcomes) is retried. A foreign key, a check
/// constraint or a dead connection stays an error — otherwise the loop would
/// spin on a failure that re-reading cannot fix.
///
/// The two retryable outcomes are not retried the same way. A UNIQUE violation
/// means someone committed and `MAX(number) + 1` has moved, so the next attempt
/// starts immediately; a busy backend means nothing has moved yet, and
/// [`crate::db_retry`] makes that attempt wait — without which concurrent
/// creates on SQLite spend the whole attempt budget busy-spinning on a lock
/// that is still held and answer correct callers with a 5xx (card_f0fd0aaa87b5).
///
/// # Why the budget is a deadline and not an attempt count
///
/// Same reason as the issue allocator it mirrors, and the same measurement: on
/// SQLite a read-then-write transaction is refused instantly, so thirty-two
/// attempts were about a third of a second however long the writer ahead
/// actually needed — and the writer ahead is often the code-index refresh
/// holding the database-wide writer lock for a whole repository snapshot
/// (card_d5612b049af6). The deadline is
/// [`rg_db::contention::ContentionBudget::for_request_write`], sized against a
/// holder's runtime rather than against a number of tries, and short enough
/// that the request behind this create has not been abandoned.
///
/// `model.number` is set here; whatever the caller left in it is overwritten.
///
/// This is also where a pull request is *counted*, for the same reason the
/// issue allocator counts issues: `create_pr` behind `POST .../pulls` and the
/// import subsystem's `import_github_pr` / `import_gitlab_mr` both come through
/// here and nothing else is common to them.
///
/// A pull request that arrives already merged is counted as merged as well.
/// [`merge_pr`] records the merges *it* performs, and it is never reached by an
/// import: the imported row is written with `state = "merged"` in one insert,
/// so without this the merged half of somebody's migrated history is invisible
/// to `forgekeep_prs_merged_total` while the opened half is not. A PR created
/// through `create_pr` is always born open, so the two producers cannot both
/// count the same merge.
pub(crate) async fn insert_with_repo_number(
    db: &DatabaseConnection,
    repo_id: i64,
    model: pull_request::ActiveModel,
) -> Result<PullRequest> {
    let pr = insert_with_repo_number_gated(db, repo_id, model, |_| std::future::ready(())).await?;
    crate::metrics_hook::record_pr_opened();
    if pr.state == "merged" {
        crate::metrics_hook::record_pr_merged();
    }
    Ok(pr)
}

/// The bounded allocate-then-insert loop behind [`insert_with_repo_number`].
///
/// `after_allocate` is a private test seam, called with the attempt number once
/// the number has been read but before it is written: production supplies a
/// ready future, while the regression test can hold two creates on the same
/// read without depending on scheduler timing.
async fn insert_with_repo_number_gated<F, Fut>(
    db: &DatabaseConnection,
    repo_id: i64,
    model: pull_request::ActiveModel,
    after_allocate: F,
) -> Result<PullRequest>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut budget = rg_db::contention::ContentionBudget::for_request_write();

    loop {
        let attempt = budget.begin_attempt();

        // The read is classified with the write: a backend that refuses the
        // lookup because someone else is mid-write has said "ask again", not
        // "this create is impossible".
        let numbered: Result<PullRequest> = async {
            let number = pull_request_ops::next_number(db, repo_id).await?;
            after_allocate(attempt).await;

            let mut candidate = model.clone();
            candidate.number = Set(number);
            pull_request_ops::create(db, candidate).await
        }
        .await;

        match numbered {
            Ok(pr) => return Ok(pr),
            Err(error) => {
                let retry = crate::db_retry::classify_anyhow(&error);
                if retry.is_worthwhile() && budget.may_retry() {
                    retry.wait(attempt).await;
                    continue;
                }
                if retry.is_worthwhile() {
                    return Err(error).context(budget.exhausted("allocate a PR number"));
                }
                return Err(error);
            }
        }
    }
}

/// Resolve a head reference in `owner:branch` format to (head_branch, head_repo_id).
///
/// Returns `(branch_name, Some(head_repo_id))` when the prefix names a different
/// repository — a fork PR — and `(branch_name, None)` when it names the target
/// repository itself, which is a plain same-repo PR written the long way.
///
/// The prefix is a **namespace**, and the only thing that knows how to read one
/// is [`crate::repo::service::find_repo_by_owner_name`]. Resolving it through
/// `users` instead had both failure modes of that shortcut at once
/// (card_2e84eeed4ff4): an organization is not a row in `users`, so `acme:feature`
/// could never be a head ref at all; and an organization's repositories carry
/// `owner_id = org.owner_id`, so comparing that column against the target's
/// treated a user's personal repository and their organization's repository of
/// the same name as one namespace. Identity is compared between *repositories*
/// here, which is the thing the answer is about.
pub async fn resolve_head_ref(
    db: &DatabaseConnection,
    target_repo_id: i64,
    head_ref: &str,
) -> Result<(String, Option<i64>)> {
    let Some((head_owner, head_branch)) = head_ref.split_once(':') else {
        // Simple branch name — same-repo PR.
        return Ok((head_ref.to_string(), None));
    };
    let head_branch = head_branch.to_string();

    let target_repo = repo_entity::Entity::find_by_id(target_repo_id)
        .one(db)
        .await?
        .context("target repository not found")?;

    // The head ref is client input, so a namespace that holds no such repository
    // is the caller's mistake — `InvalidRequest` keeps it a 400, while a failed
    // lookup behind it stays a 5xx. The two cases the caller could tell apart —
    // no such namespace, or a namespace without this repository — read the same
    // on purpose: neither gives them a next step the other does not.
    let head_repo =
        crate::repo::service::find_repo_by_owner_name(db, head_owner, &target_repo.name)
            .await?
            .ok_or_else(|| {
                crate::error::invalid_request(format!(
                    "no repository '{}/{}' found for head owner",
                    head_owner, target_repo.name
                ))
            })?;

    // The target repository named through its own namespace prefix.
    if head_repo.id == target_repo_id {
        return Ok((head_branch, None));
    }

    if head_repo.origin_repo_id != Some(target_repo_id) {
        return Err(crate::error::invalid_request(format!(
            "'{}/{}' is not a fork of the target repository",
            head_owner, target_repo.name
        )));
    }

    Ok((head_branch, Some(head_repo.id)))
}

/// The namespace of a repository, taking the row instead of its two id columns.
///
/// One rule, one implementation: this used to be its own copy of the org-then-user
/// lookup, next to an identical private copy in `repo::service` that the create
/// and delete paths use to build the on-disk directory. Two copies of "what is
/// this repository called" is how the disk and the API start disagreeing.
pub(super) async fn repository_namespace(
    db: &DatabaseConnection,
    repository: &repo_entity::Model,
) -> Result<String> {
    crate::repo::service::repository_namespace_name(db, repository.owner_id, repository.org_id)
        .await
}

/// Notify watchers of a PR event (`opened` / `closed` / `reopened` / `merged`).
///
/// `actor_name` is the account that caused the transition, and is `None` when
/// there isn't one: an auto-merge or a merge-queue merge is performed by the
/// server, not by a user. The body then states the action without an actor
/// instead of rendering a leading blank, and no recipient is excluded from the
/// fan-out.
async fn notify_watchers_pr(
    db: &DatabaseConnection,
    repo_id: i64,
    repo_name: &str,
    actor_name: Option<&str>,
    pr_number: i64,
    pr_title: &str,
    action: &str,
) -> Result<()> {
    let body = match actor_name {
        Some(actor) => format!("{} {}: {}", actor, action, pr_title),
        None => format!("PR #{} {}: {}", pr_number, action, pr_title),
    };
    crate::notification::notify_watchers(
        db,
        &crate::notification::WatchEvent {
            repo_id,
            author_name: actor_name.unwrap_or_default().to_string(),
            title: format!("PR #{} {} in {}", pr_number, action, repo_name),
            notification_type: "pull_request".to_string(),
            body: Some(body),
        },
    )
    .await
}

/// Resolve a username for the watch fan-out, or `None` when the account is
/// gone. A failed lookup is logged by the shared notification policy before it
/// degrades to an actor-less event.
async fn watch_actor_name(db: &DatabaseConnection, repo_id: i64, actor_id: i64) -> Option<String> {
    crate::notification::best_effort_user_by_id(db, actor_id, repo_id, "pull_request", "actor")
        .await
        .map(|user| user.username)
}

/// Fan a PR transition out to the repository's watchers, logging rather than
/// propagating a failure: the transition itself has already been committed.
///
/// Detached, never awaited. The walk costs a read check and an insert per
/// subscriber, and all three transitions that reach here — open, close/reopen,
/// merge — are answered to a waiting HTTP client, so awaiting it priced opening
/// a pull request on a popular repository at `O(watchers)` round-trips of
/// latency (card_3b4275a366ab). The actor lookup goes inside the task for the
/// same reason.
///
/// `tracker` is the caller's, when it has one: an HTTP handler passes its
/// `AppState`'s, so a test can drain exactly the work its own request produced.
/// `None` falls back to the process-global delivery tracker — the same one
/// `rg_http::run` closes on shutdown — for callers that are already off a
/// request path (the merge queue, auto-merge).
#[allow(clippy::too_many_arguments)]
fn announce_pr_to_watchers(
    db: &DatabaseConnection,
    tracker: Option<&crate::task_tracker::TaskTracker>,
    repo_id: i64,
    repo_name: &str,
    actor_id: Option<i64>,
    pr_number: i64,
    pr_title: &str,
    action: &str,
) {
    let tracker = tracker.unwrap_or_else(|| crate::task_tracker::delivery_tracker());
    let db = db.clone();
    let repo_name = repo_name.to_string();
    let pr_title = pr_title.to_string();
    let action = action.to_string();
    tracker.spawn(async move {
        let actor_name = match actor_id {
            Some(actor_id) => watch_actor_name(&db, repo_id, actor_id).await,
            None => None,
        };
        if let Err(e) = notify_watchers_pr(
            &db,
            repo_id,
            &repo_name,
            actor_name.as_deref(),
            pr_number,
            &pr_title,
            &action,
        )
        .await
        {
            tracing::warn!(error = %format!("{e:#}"), action, "failed to notify watchers about PR");
        }
    });
}

/// Paginated list of PRs. Returns (prs, total).
///
/// The unbounded `list_prs` that used to sit here is gone for the reason given
/// on `issue::service::list_issues_paginated`: it selected every pull request
/// of the repository, and its only caller was a listing with a documented
/// maximum (card_c386beea2fe0).
pub async fn list_prs_paginated(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    state: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<PullRequest>, i64)> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    pull_request_ops::list_by_repo_paginated(db, repo.id, state, offset, limit).await
}

/// Get a single PR.
///
/// The three "genuinely absent" outcomes — unknown owner, unknown repository,
/// unknown PR number — are reported as [`NotFound`], so a caller can tell them
/// apart from a failed query. Every HTTP handler on this path used to answer
/// `404` to *any* error here, which made a database outage indistinguishable
/// from a deleted PR; see the type's docs.
pub async fn get_pr(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    number: i64,
) -> Result<PullRequest> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    pull_request_ops::find_by_repo_and_number(db, repo.id, number)
        .await?
        .ok_or_else(|| NotFound::new("pull request").into())
}

/// Update PR metadata (title, body, state).
///
/// `delivery_tracker` carries the watch fan-out off this call's critical path —
/// see [`announce_pr_to_watchers`] for what `None` means.
#[allow(clippy::too_many_arguments)]
pub async fn update_pr(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    number: i64,
    title: Option<String>,
    body: Option<String>,
    state: Option<String>,
    is_draft: Option<bool>,
    actor_id: i64,
    delivery_tracker: Option<&crate::task_tracker::TaskTracker>,
) -> Result<PullRequest> {
    let mut pr = get_pr(db, owner, repo_name, number).await?;
    let previous_state = pr.state.clone();
    let previous_draft = pr.is_draft;

    if let Some(t) = title {
        if t.trim().is_empty() {
            return Err(crate::error::invalid_request("PR title cannot be empty"));
        }
        pr.title = t;
    }
    if let Some(b) = body {
        pr.body = Some(b);
    }
    if let Some(draft) = is_draft {
        // The PR's state, not the request's shape: reopen it and this same
        // call goes through. `Conflict` (409) is what the merge, auto-merge,
        // merge-queue and review gates next door already answer to the very
        // same predicate; a 400 told the client to stop retrying a request
        // that was never wrong.
        if pr.state != "open" {
            return Err(crate::error::conflict(format!(
                "only an open pull request can change draft status (current: {})",
                pr.state
            )));
        }
        pr.is_draft = draft;
    }
    if let Some(s) = &state {
        match s.as_str() {
            "open" | "closed" | "merged" => {
                let was_open = pr.state == "open";
                pr.state = s.clone();
                if s != "open" {
                    pr.auto_merge_enabled = false;
                }
                if s == "closed" && pr.closed_at.is_none() {
                    pr.closed_at = Some(Utc::now());
                }

                // Trigger pull_request.closed webhook when transitioning to closed
                if was_open && s == "closed" {
                    let close_payload = serde_json::json!({
                        "id": pr.id,
                        "repo_id": pr.repo_id,
                        "number": pr.number,
                        "title": pr.title,
                        "state": s,
                    });
                    if let Err(e) =
                        crate::webhook::service::trigger_pr_closed(db, pr.repo_id, &close_payload)
                            .await
                    {
                        tracing::warn!(error = %format!("{e:#}"), "failed to trigger PR closed webhook");
                    }
                }
            }
            _ => {
                return Err(crate::error::invalid_request(format!(
                    "invalid PR state: {s}"
                )))
            }
        }
    }

    pr.updated_at = Utc::now();

    // `Model -> ActiveModel` marks fields as `Unchanged`; explicitly mark the
    // mutable PR metadata so SeaORM actually emits an UPDATE.
    let final_title = pr.title.clone();
    let final_body = pr.body.clone();
    let final_state = pr.state.clone();
    let final_is_draft = pr.is_draft;
    let final_auto_merge_enabled = pr.auto_merge_enabled;
    let final_closed_at = pr.closed_at;
    let final_updated_at = pr.updated_at;
    let mut active: pull_request::ActiveModel = pr.into();
    active.title = Set(final_title);
    active.body = Set(final_body);
    active.state = Set(final_state);
    active.is_draft = Set(final_is_draft);
    active.auto_merge_enabled = Set(final_auto_merge_enabled);
    active.closed_at = Set(final_closed_at);
    active.updated_at = Set(final_updated_at);
    let updated = pull_request_ops::update(db, active).await?;
    if previous_draft != updated.is_draft {
        let event_type = if updated.is_draft {
            "pull_request_converted_to_draft"
        } else {
            "pull_request_marked_ready"
        };
        if let Err(error) = rg_db::ops::pr_event_ops::record(
            db,
            updated.repo_id,
            updated.id,
            Some(actor_id),
            event_type,
            None,
            serde_json::json!({}),
        )
        .await
        {
            tracing::error!(
                repo_id = updated.repo_id,
                pr_id = updated.id,
                actor_id,
                event_type,
                error = %format!("{error:#}"),
                "pull request draft state changed, but its timeline event could not be recorded"
            );
        }
    }
    if previous_state != updated.state {
        let event_type = match updated.state.as_str() {
            "open" => "pull_request_reopened",
            "closed" => "pull_request_closed",
            _ => "pull_request_state_changed",
        };
        if let Err(error) = rg_db::ops::pr_event_ops::record(
            db,
            updated.repo_id,
            updated.id,
            Some(actor_id),
            event_type,
            None,
            serde_json::json!({"from": previous_state, "to": updated.state}),
        )
        .await
        {
            tracing::error!(
                repo_id = updated.repo_id,
                pr_id = updated.id,
                actor_id,
                event_type,
                error = %format!("{error:#}"),
                "pull request state changed, but its timeline event could not be recorded"
            );
        }
        // Announced from here, after the transition is persisted — not from the
        // `state` match above, which runs before the UPDATE and would tell
        // watchers about a close that a later failure rolled back.
        let action = match updated.state.as_str() {
            "open" => Some("reopened"),
            "closed" => Some("closed"),
            // A merge announces itself from `update_pr_merged`, which knows the
            // strategy and the merge commit; a bare state write to "merged"
            // through this path is not the merge event.
            _ => None,
        };
        if let Some(action) = action {
            announce_pr_to_watchers(
                db,
                delivery_tracker,
                updated.repo_id,
                repo_name,
                Some(actor_id),
                updated.number,
                &updated.title,
                action,
            );
        }
        // Same placement and the same reason as the announcement above: the
        // transition is persisted, so this cannot cancel the CI of a close a
        // later failure rolled back. See `cancel_pull_request_ci` for why a
        // transition to `merged` cancels too.
        if previous_state == "open" && updated.state != "open" {
            super::ci::cancel_pull_request_ci(db, &updated, "pull request left `open`").await;
        }
    }
    Ok(updated)
}

// ── Diff ────────────────────────────────────────────────────────────────

/// Diff result for a PR.
#[derive(Debug, serde::Serialize)]
pub struct PrDiff {
    pub base_branch: String,
    pub head_branch: String,
    pub files_changed: Vec<FileDiff>,
    pub stats: DiffStats,
}

#[derive(Debug, serde::Serialize)]
pub struct FileDiff {
    pub path: String,
    pub status: String, // added / modified / deleted / renamed
    pub additions: i64,
    pub deletions: i64,
    pub patch: Option<String>,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct DiffLine {
    /// meta / context / addition / deletion
    pub kind: String,
    pub content: String,
    pub old_line: Option<i64>,
    pub new_line: Option<i64>,
}

#[derive(Debug, serde::Serialize)]
pub struct DiffStats {
    pub total_additions: i64,
    pub total_deletions: i64,
    pub files_changed: i64,
}

/// Compute the diff between base and head branches using `git diff`.
/// Supports cross-repository (fork) PRs.
///
/// Gix tree-diff operations are offloaded to `spawn_blocking` to avoid
/// blocking the tokio async runtime.
pub async fn compute_diff(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    owner: &str,
    repo_name: &str,
    number: i64,
) -> Result<PrDiff> {
    let pr = get_pr(db, owner, repo_name, number).await?;
    let base_repo_path = repo_root.join(format!("{}/{}.git", owner, repo_name));

    if !base_repo_path.exists() {
        bail!("repository path does not exist: {:?}", base_repo_path);
    }
    require_pull_request_branch(&base_repo_path, "base", &pr.base_branch)?;

    // For fork PRs, fetch the head branch into the target repo first
    if let Some(head_repo_id) = pr.head_repo_id {
        let head_repo = repo_entity::Entity::find_by_id(head_repo_id)
            .one(db)
            .await?
            .context("head repository not found")?;
        // `repository_namespace`, not the owner's username: an organization's
        // repository lives on disk under the organization's name while its row
        // carries `owner_id = org.owner_id`, so building the path from that user
        // pointed at a directory that does not exist. `merge_claimed_pr` and
        // `create_pr` already resolved the namespace; this one did not, and it
        // became reachable the moment an organization could be a fork head
        // (card_2e84eeed4ff4) — the diff would have failed where the merge
        // succeeded.
        let head_namespace = repository_namespace(db, &head_repo).await?;
        let head_repo_path = repo_root.join(format!("{head_namespace}/{}.git", head_repo.name));

        // Do not let a failed fetch silently reuse an old `refs/forks/...` ref:
        // a deleted head branch is a stale PR state (409), whereas an unreadable
        // fork repository or a failed fetch is our retryable failure (5xx).
        require_pull_request_branch(&head_repo_path, "head", &pr.head_branch)?;
        let fetch_ref = format!("refs/heads/{}", pr.head_branch);
        let local_ref = format!("refs/forks/{}/{}", head_namespace, pr.head_branch);

        let git = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let fetch_output = git.run(
            &[
                "fetch",
                &head_repo_path.to_string_lossy(),
                &format!("{}:{}", fetch_ref, local_ref),
            ],
            Some(&base_repo_path),
        )?;
        fetch_output
            .ensure_success()
            .context("failed to fetch pull request head branch")?;

        // Compute diff inside spawn_blocking (CPU-intensive gix tree-diff)
        let base_path = base_repo_path.clone();
        let pr_clone = pr.clone();
        let local_ref = local_ref.clone();
        return tokio::task::spawn_blocking(move || {
            compute_cross_repo_diff(&base_path, &pr_clone.base_branch, &local_ref, &pr_clone)
        })
        .await?;
    }

    require_pull_request_branch(&base_repo_path, "head", &pr.head_branch)?;

    // Same-repo diff — offload to spawn_blocking
    let base_path = base_repo_path.clone();
    let pr_clone = pr.clone();
    tokio::task::spawn_blocking(move || compute_same_repo_diff(&base_path, &pr_clone)).await?
}

/// The head revision a pull request's diff is about.
///
/// Not `refs/heads/<head>`, and not the fork ref just fetched: both name the
/// branch tip *now*, while everything the reviewer's verdict attaches to names
/// `pr.head_sha` — approvals are counted for it
/// (`pr_review_ops::count_current_approvals`), branch protection judges it, and
/// the merge is pinned to it. The row is moved by the detached post-push hook,
/// so it lags the branch by design; a diff taken from the branch shows content
/// that the approval about to be recorded will not cover (card_9ff26bb95dc9).
///
/// Falls back to the ref when the row names no commit, or names one this
/// repository does not hold — a pull request created before the column existed,
/// or a fork head that was never fetched here. Showing the branch is what this
/// function did for every pull request until now, so the fallback is the old
/// behaviour rather than a new failure mode.
fn diff_head_rev(repo_path: &std::path::Path, pr: &PullRequest, head_ref: &str) -> String {
    let Some(head_sha) = pr.head_sha.as_deref().filter(|sha| !sha.is_empty()) else {
        return head_ref.to_string();
    };
    let Ok(git) = rg_git::cli_gateway::global_gateway().as_ref() else {
        return head_ref.to_string();
    };
    let resolved = git.run(
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{head_sha}^{{commit}}"),
        ],
        Some(repo_path),
    );
    match resolved {
        Ok(output) if output.success() => head_sha.to_string(),
        _ => {
            tracing::warn!(
                pr_id = pr.id,
                head_branch = %pr.head_branch,
                head_sha = %head_sha,
                "the pull request head commit is not in this repository; diffing the branch tip instead"
            );
            head_ref.to_string()
        }
    }
}

/// The two commits BOTH halves of a pull-request diff are read between,
/// resolved once so the halves cannot answer about different ranges.
///
/// A pull request is about what its head ADDS to the base, which is why the
/// patch half has always been read as `base...head`: git resolves three dots to
/// `merge-base(base, head)` against the head, and the merge itself
/// ([`gix_merge_commits_to_tree`]) works from that same merge base. The numstat
/// half, however, was handed `refs/heads/<base>` — and a tree-diff against the
/// base TIP is two-dot `git diff base head`.
///
/// The two agree only while the base has not moved since the branch point, and
/// a base that moves is the normal life of an open pull request. As soon as it
/// does, the numstat half reports every file the base gained in the meantime as
/// a change the head never made: measured on a live repository, a file
/// committed to `main` after branching is published in `files_changed` as
/// `deleted` with `patch: null`, and its lines are added to `stats`
/// (card_dd105a36fc64).
///
/// So the merge base is resolved here, once, and both halves are handed the
/// same pair of commits — the patch half as two explicit revisions rather than
/// a `...` range, so there is no second engine left to interpret it.
fn forgekeep_diff_revs(
    repo_path: &std::path::Path,
    base_ref: &str,
    head_rev: &str,
) -> Result<(String, String)> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let commit_id = |spec: &str| -> Result<gix::ObjectId> {
        Ok(repo
            .rev_parse_single(spec)
            .with_context(|| format!("ref not found: {}", spec))?
            .object()?
            .peel_to_commit()
            .with_context(|| format!("{} is not a commit", spec))?
            .id)
    };

    let base_id = commit_id(base_ref)?;
    let head_id = commit_id(head_rev)?;

    let merge_base = repo
        .merge_base(base_id, head_id)
        .with_context(|| format!("no merge base between {} and {}", base_ref, head_rev))?
        .detach();

    Ok((merge_base.to_string(), head_id.to_string()))
}

/// The argv the unified-diff half of a pull-request diff is read with, stated
/// in one place for both call sites.
///
/// `git diff` detects renames on its own — `diff.renames` has defaulted to true
/// since git 2.9, and the repository's own `.git/config` can turn it on at any
/// permission level — and it prints a renamed file as ONE entry, keyed by the
/// new path. [`gix_diff_numstat`], which is what builds `files_changed`, walks
/// the tree with `track_rewrites(None)` and reports the same rename as a
/// deletion plus an addition. [`attach_patches`] joins the two halves by path,
/// so a rename made the answer disagree with itself twice over: the old path
/// was listed with no patch at all, and the new path carried a whole-file
/// numstat above a patch in which one line had changed.
///
/// `--no-renames` states the model the numstat half already uses, so both
/// halves name the same entries and count the same lines — see
/// card_283b386e7091.
///
/// The two revisions arrive already resolved, from [`forgekeep_diff_revs`],
/// instead of being spelled here as a `base...head` range: the range is the
/// other thing the two halves used to choose independently, and a `...` range
/// is git's own reading of it rather than the one the numstat half walked —
/// see card_dd105a36fc64.
///
/// Everything else here states, at git's own default, a decision the
/// repository's own `.git/config` would otherwise make — and repository-local
/// configuration is the placement no amount of environment disarming reaches,
/// because git loads it at every permission level. Each entry is a knob
/// measured to change what this half says about a pull request (git 2.43.0):
///
/// * `diff.algorithm` decides *how many* lines changed, and the numstat half no
///   longer asks anybody: it states [`forgekeep_diff_algorithm`]. Left
///   unstated here, the same six-line fixture is `2 2` to the numstat half and
///   four added lines in the patch under it — one answer, counted twice, by two
///   different algorithms (card_4a21d30afb9a). `-c` beats a configuration file
///   and the later `-c` wins, so this one outranks the same setting
///   [`rg_git::invocation::local`] states for every local git command.
/// * `diff.noprefix`, `diff.mnemonicPrefix` and (git 2.45+) `diff.srcPrefix` /
///   `diff.dstPrefix` decide what [`split_unified_diff`] parses: it reads the
///   `a/` / `b/` pair, and a repository that dropped the prefixes hands a
///   top-level file named `b` a header the reader silently truncates.
/// * `diff.context`, `diff.interHunkContext` and `diff.indentHeuristic` decide
///   how much of the file a reviewer is shown around a change and where the
///   hunk boundaries fall.
/// * `diff.ignoreSubmodules` can drop a changed submodule out of this half
///   entirely, while the tree-walking half keeps reporting it as an entry.
/// * a `diff.<driver>.textconv` renders the patch through a program instead of
///   the bytes — the numstat half counts the bytes, so the two would describe
///   different content. `--no-ext-diff` denies `diff.external`, which is the
///   other half of that.
fn forgekeep_patch_argv<'a>(old_rev: &'a str, new_rev: &'a str) -> [&'a str; 16] {
    [
        "-c",
        "core.quotePath=false",
        "-c",
        forgekeep_diff_algorithm_setting(),
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--ignore-submodules=none",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--unified=3",
        "--inter-hunk-context=0",
        "--indent-heuristic",
        old_rev,
        new_rev,
    ]
}

/// The unified diff the patch half of a pull-request diff is built from, read
/// in one place for both call sites.
///
/// Run through [`rg_git::invocation::local`] rather than on a bare gateway: the
/// gateway disarms the host's environment for every child it spawns, which is
/// what puts `/etc/gitconfig` and `~/.gitconfig` out of reach, but it says
/// nothing about which values ForgeKeep wants when nobody configured any. That
/// list is `invocation::local`'s, and reading the patch under it is what makes
/// this half of the answer ForgeKeep's own decision rather than a property of
/// the git binary that happens to be installed.
fn forgekeep_patch_text(
    repo_path: &std::path::Path,
    old_rev: &str,
    new_rev: &str,
) -> Result<String> {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let git = rg_git::invocation::local(gateway);
    let patch_output = git.run(&forgekeep_patch_argv(old_rev, new_rev), Some(repo_path))?;
    patch_output.ensure_success()?;
    Ok(patch_output.stdout_str())
}

/// Compute diff for same-repo PR.
fn compute_same_repo_diff(repo_path: &std::path::Path, pr: &PullRequest) -> Result<PrDiff> {
    let head_rev = diff_head_rev(repo_path, pr, &format!("refs/heads/{}", pr.head_branch));

    // Both halves below read the same two commits — see `forgekeep_diff_revs`.
    let (old_rev, new_rev) = forgekeep_diff_revs(
        repo_path,
        &format!("refs/heads/{}", pr.base_branch),
        &head_rev,
    )?;

    // Use gix tree-diff for numstat (files_changed + per-file additions/deletions)
    let (files_changed, stats) = gix_diff_numstat(repo_path, old_rev.clone(), new_rev.clone())?;

    // Get unified diff patch via gateway (TODO(gix): replace with gix blob-diff
    // when byte-identical output is achievable — see plan.md Phase 3)
    let patch_text = forgekeep_patch_text(repo_path, &old_rev, &new_rev)?;

    let mut files = files_changed;
    attach_patches(&mut files, &patch_text);

    Ok(PrDiff {
        base_branch: pr.base_branch.clone(),
        head_branch: pr.head_branch.clone(),
        stats,
        files_changed: files,
    })
}

/// Compute diff for cross-repo (fork) PR using a fetched ref.
fn compute_cross_repo_diff(
    repo_path: &std::path::Path,
    base_branch: &str,
    fork_ref: &str,
    pr: &PullRequest,
) -> Result<PrDiff> {
    // The fetch above brought the fork's tip; the pull request's own head is
    // what the review and the merge are about — see `diff_head_rev`.
    let head_rev = diff_head_rev(repo_path, pr, fork_ref);

    // Both halves below read the same two commits — see `forgekeep_diff_revs`.
    let (old_rev, new_rev) =
        forgekeep_diff_revs(repo_path, &format!("refs/heads/{}", base_branch), &head_rev)?;

    // Use gix tree-diff for numstat (files_changed + per-file additions/deletions)
    let (files_changed, stats) = gix_diff_numstat(repo_path, old_rev.clone(), new_rev.clone())?;

    // Get unified diff patch via gateway (TODO(gix): replace with gix blob-diff when feasible)
    let patch_text = forgekeep_patch_text(repo_path, &old_rev, &new_rev)?;

    let mut files = files_changed;
    attach_patches(&mut files, &patch_text);

    Ok(PrDiff {
        base_branch: pr.base_branch.clone(),
        head_branch: pr.head_branch.clone(),
        stats,
        files_changed: files,
    })
}

/// Join the file list one half of the diff produced to the patches the other
/// half printed, and say out loud when the two do not meet.
///
/// The join is by path, and a key that does not match is indistinguishable
/// from a file that legitimately has nothing to show: both leave `patch:
/// None`, and the handle answers `200` with an answer that contradicts itself.
/// That same near-miss has already produced two different defects — the path
/// was spelled differently (`card_c9ecf8644e12`) and the rename model differed
/// (`card_283b386e7091`) — and both were found by hand on a live repository
/// because nothing in the code or the tests reacted to the join failing.
///
/// So the mismatch is counted in BOTH directions: a listed file no patch
/// claimed, and a patched entry that is on nobody's list. The second direction
/// is the one a caller cannot see at all — such an entry is dropped from the
/// answer entirely, without even a `patch: null` to hint at it.
///
/// This is a detector, not a gate: the answer still goes out. While
/// `files_changed` is built by one engine and the patch by another, there is
/// no cheap way to make the two incapable of disagreeing — but the third cause
/// should be found in the log rather than by probing a live git.
fn attach_patches(files: &mut [FileDiff], unified_diff: &str) {
    let patches = split_unified_diff(unified_diff);
    let mut listed_without_patch: Vec<String> = Vec::new();
    for file in files.iter_mut() {
        match patches.get(&file.path) {
            Some(patch) => {
                file.lines = parse_diff_lines(patch);
                file.patch = Some(patch.clone());
            }
            None => listed_without_patch.push(file.path.clone()),
        }
    }

    let listed: std::collections::HashSet<&str> =
        files.iter().map(|file| file.path.as_str()).collect();
    let mut patched_without_entry: Vec<&str> = patches
        .keys()
        .map(String::as_str)
        .filter(|path| !listed.contains(path))
        .collect();

    if listed_without_patch.is_empty() && patched_without_entry.is_empty() {
        return;
    }
    // Both halves are unordered, so the log line is sorted — an operator
    // comparing two occurrences reads a difference in the paths, not in a hash
    // order.
    listed_without_patch.sort_unstable();
    patched_without_entry.sort_unstable();
    tracing::warn!(
        listed_files = files.len(),
        patch_entries = patches.len(),
        listed_without_patch = ?listed_without_patch,
        patched_without_entry = ?patched_without_entry,
        "the two halves of the pull request diff do not name the same files; \
         the answer they build together contradicts itself"
    );
}

/// The line without its terminator.
///
/// Not `trim_end`: a committed path may end in a space, and only a `+++ `
/// label carries the TAB git appends to a name that contains one.
fn diff_line_body(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Drop the `a/` or `b/` git puts in front of a diff pathspec.
fn strip_pathspec_prefix(pathspec: &str) -> &str {
    pathspec
        .strip_prefix("a/")
        .or_else(|| pathspec.strip_prefix("b/"))
        .unwrap_or(pathspec)
}

/// Decode git's C-style quoting of a path (`"a/we\"ird.txt"` → `a/we"ird.txt`),
/// answering the decoded path together with the byte offset just past the
/// closing quote. `None` for input that does not open with a quote, or whose
/// quoting never closes.
///
/// The `core.quotePath=false` both diff call sites pass only stops git from
/// escaping bytes >= 0x80; `"`, `\` and control characters are escaped
/// unconditionally, so a reader that does not decode them keeps a key no file
/// is stored under.
fn unquote_c_style_path(quoted: &str) -> Option<(String, usize)> {
    let bytes = quoted.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 1;
    while at < bytes.len() {
        match bytes[at] {
            b'"' => return Some((String::from_utf8_lossy(&decoded).into_owned(), at + 1)),
            b'\\' => {
                at += 1;
                let escape = *bytes.get(at)?;
                at += 1;
                match escape {
                    b'a' => decoded.push(0x07),
                    b'b' => decoded.push(0x08),
                    b'f' => decoded.push(0x0c),
                    b'n' => decoded.push(b'\n'),
                    b'r' => decoded.push(b'\r'),
                    b't' => decoded.push(b'\t'),
                    b'v' => decoded.push(0x0b),
                    b'0'..=b'7' => {
                        // git spells a raw byte as three octal digits.
                        let mut value = u32::from(escape - b'0');
                        for _ in 0..2 {
                            match bytes.get(at).copied() {
                                Some(digit @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(digit - b'0');
                                    at += 1;
                                }
                                _ => break,
                            }
                        }
                        decoded.push(u8::try_from(value).ok()?);
                    }
                    other => decoded.push(other),
                }
            }
            byte => {
                decoded.push(byte);
                at += 1;
            }
        }
    }
    None
}

/// Split the pathspec pair of a `diff --git ` header into its two halves, each
/// still carrying its `a/` / `b/` prefix.
///
/// Git quotes each half on its own and only when that name needs escaping, and
/// it never escapes the space that separates the pair — so an unquoted pair
/// whose names contain spaces is genuinely ambiguous. Git's own patch reader
/// resolves that the only way it can be: by accepting a split whose two halves
/// name the same file. A rename that stays ambiguous is left to the `+++ ` and
/// `rename to ` lines below, which spell one path per line.
fn split_pathspec_pair(pair: &str) -> Option<(String, String)> {
    if let Some((first, end)) = unquote_c_style_path(pair) {
        let rest = pair.get(end..)?.strip_prefix(' ')?;
        let second = match unquote_c_style_path(rest) {
            Some((second, _)) => second,
            None => rest.to_string(),
        };
        return Some((first, second));
    }
    // An unquoted first half carries no `"` of its own, so the first quote on
    // the line is the one that opens the second half.
    if let Some(quote_at) = pair.find('"') {
        let first = pair.get(..quote_at)?.strip_suffix(' ')?;
        let (second, _) = unquote_c_style_path(pair.get(quote_at..)?)?;
        return Some((first.to_string(), second));
    }
    let separators: Vec<usize> = pair.match_indices(' ').map(|(at, _)| at).collect();
    if let [only] = separators[..] {
        return Some((pair[..only].to_string(), pair[only + 1..].to_string()));
    }
    separators.into_iter().find_map(|at| {
        let (first, second) = (&pair[..at], &pair[at + 1..]);
        (strip_pathspec_prefix(first) == strip_pathspec_prefix(second))
            .then(|| (first.to_string(), second.to_string()))
    })
}

/// The path a `diff --git ` header names on its `b/` side — the spelling
/// [`gix_diff_numstat`] keys its [`FileDiff`] under.
fn diff_git_header_path(header: &str) -> Option<String> {
    let (_, second) = split_pathspec_pair(header.strip_prefix("diff --git ")?)?;
    Some(strip_pathspec_prefix(&second).to_string())
}

/// The path a `+++ ` or `rename to ` line names, or `None` for the `/dev/null`
/// half of a deletion.
///
/// The caller strips the `a/` / `b/` prefix where there is one: `rename to`
/// prints the path bare, and a repository may well hold a directory named `b`.
fn diff_label_path(label: &str) -> Option<String> {
    // Git appends a TAB to a `+++ ` label that contains a space. Strip exactly
    // one, so a name that itself ends in a space keeps it.
    let label = diff_line_body(label);
    let label = label.strip_suffix('\t').unwrap_or(label);
    if label == "/dev/null" {
        return None;
    }
    Some(match unquote_c_style_path(label) {
        Some((path, _)) => path,
        None => label.to_string(),
    })
}

fn split_unified_diff(unified_diff: &str) -> HashMap<String, String> {
    let mut patches = HashMap::new();
    let mut current_path: Option<String> = None;
    let mut current_patch = String::new();
    // Only a file entry's header names paths. Inside a hunk a `+++ ` line is
    // content — adding the line `++ x` prints `+++ x`, and a committed patch
    // file adds exactly that — so a reader that keeps looking for headers there
    // re-keys the rest of the patch under whatever that content happens to say.
    let mut in_file_header = false;

    let flush =
        |path: &mut Option<String>, patch: &mut String, patches: &mut HashMap<String, String>| {
            let patch = std::mem::take(patch);
            if let Some(path) = path.take() {
                patches.insert(path, patch);
            }
        };

    for line in unified_diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            flush(&mut current_path, &mut current_patch, &mut patches);
            in_file_header = true;
            current_path = diff_git_header_path(diff_line_body(line));
        } else if line.starts_with("@@ ") {
            in_file_header = false;
        } else if in_file_header {
            // `+++` names the file numstat keys this entry under, one path to
            // the line, so it wins over the header wherever it is printed at
            // all. A pure rename prints no `---`/`+++` pair — and its header is
            // the ambiguous kind whenever a name carries a space — so `rename
            // to` is what answers for that one.
            if let Some(label) = line.strip_prefix("+++ ") {
                if let Some(path) = diff_label_path(label) {
                    current_path = Some(strip_pathspec_prefix(&path).to_string());
                }
            } else if let Some(label) = line.strip_prefix("rename to ") {
                current_path = diff_label_path(label).or(current_path);
            }
        }
        // A header whose pair could not be split leaves the path to a line
        // further down, so the entry is buffered on `in_file_header` too — and
        // dropped by `flush` if no line ever names it.
        if current_path.is_some() || in_file_header {
            current_patch.push_str(line);
        }
    }
    flush(&mut current_path, &mut current_patch, &mut patches);
    patches
}

/// Read one file's patch into numbered lines.
///
/// The only thing that says whether a line is content or structure is where it
/// sits: `old_line` is `None` until the first `@@`, and every `---` / `+++`
/// label a file entry carries is printed above that. So a `-` or `+` seen after
/// a hunk header is content, whatever it spells — and content that spells a
/// label is ordinary: `--` opens a comment in SQL, Lua and Haskell, `++`
/// increments in every C-shaped language, and a committed `.patch` file is made
/// of such lines. Excluding them here dropped the line out of its own hunk and
/// shifted every number after it by one, which is what a review comment is
/// anchored to — see card_aab572c59c7b.
fn parse_diff_lines(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut old_line = None;
    let mut new_line = None;

    for raw_line in patch.lines() {
        if raw_line.starts_with("@@ ") {
            if let Some((old, new)) = parse_hunk_header(raw_line) {
                old_line = Some(old);
                new_line = Some(new);
            }
            lines.push(DiffLine {
                kind: "meta".into(),
                content: raw_line.into(),
                old_line: None,
                new_line: None,
            });
        } else if old_line.is_some() && raw_line.starts_with('+') {
            let line_number = new_line;
            new_line = new_line.map(|line| line + 1);
            lines.push(DiffLine {
                kind: "addition".into(),
                content: raw_line[1..].into(),
                old_line: None,
                new_line: line_number,
            });
        } else if old_line.is_some() && raw_line.starts_with('-') {
            let line_number = old_line;
            old_line = old_line.map(|line| line + 1);
            lines.push(DiffLine {
                kind: "deletion".into(),
                content: raw_line[1..].into(),
                old_line: line_number,
                new_line: None,
            });
        } else if old_line.is_some() && raw_line.starts_with(' ') {
            let previous_old = old_line;
            let previous_new = new_line;
            old_line = old_line.map(|line| line + 1);
            new_line = new_line.map(|line| line + 1);
            lines.push(DiffLine {
                kind: "context".into(),
                content: raw_line[1..].into(),
                old_line: previous_old,
                new_line: previous_new,
            });
        } else {
            lines.push(DiffLine {
                kind: "meta".into(),
                content: raw_line.into(),
                old_line: None,
                new_line: None,
            });
        }
    }
    lines
}

fn parse_hunk_header(header: &str) -> Option<(i64, i64)> {
    let mut fields = header.split_whitespace();
    (fields.next()? == "@@").then_some(())?;
    let old = fields.next()?.strip_prefix('-')?;
    let new = fields.next()?.strip_prefix('+')?;
    Some((parse_range_start(old)?, parse_range_start(new)?))
}

fn parse_range_start(range: &str) -> Option<i64> {
    range.split(',').next()?.parse().ok()
}

/// The diff algorithm ForgeKeep counts pull-request lines with, stated instead
/// of looked up.
///
/// `Repository::diff_resource_cache` fills the blob platform's options from
/// `diff.algorithm` of the repository it was opened with, and `line_counts()`
/// then runs whatever came back. Opening through [`rg_git::repository::open`]
/// already puts the host's `/etc/gitconfig`, `~/.gitconfig` and `GIT_*` out of
/// reach, but the answer to "how many lines does this pull request add" would
/// still be a property of a config file — the repository's own `.git/config` is
/// loaded at every permission level — rather than of ForgeKeep. Two instances
/// must report the same numbers for the same pull request, and a diff algorithm
/// is not a rendering preference here: Myers and Histogram genuinely disagree
/// on how many lines changed.
///
/// `Myers` is what an unconfigured host produced before, because that is the
/// value `gix` falls back to when `diff.algorithm` is unset, so no existing
/// instance sees its numbers move.
///
/// The resource cache below also declines configuration-backed diff drivers.
/// An in-tree `.gitattributes` file is repository content and remains
/// authoritative, but a `diff=<name>` assignment cannot make a server execute
/// or trust a matching `diff.<name>` section from `.git/config`.
///
/// [`forgekeep_merge_options`] states the same algorithm for the *merge* text
/// driver and keeps its own copy on purpose: that function spells out every
/// field of a `gix` options struct so a knob added later breaks the build
/// instead of defaulting silently.
fn forgekeep_diff_algorithm() -> gix::diff::blob::Algorithm {
    gix::diff::blob::Algorithm::Myers
}

/// The same decision, spelled the way `git`'s own `diff.algorithm` spells it,
/// for the half of a pull-request diff that is read from the `git` binary.
///
/// Derived from [`forgekeep_diff_algorithm`] rather than written out a second
/// time: the two halves of one answer disagreeing about how many lines changed
/// is the whole defect this exists to deny (card_4a21d30afb9a), and a second
/// literal is how that comes back. The match is exhaustive on purpose — a
/// `gix` release that adds an algorithm breaks the build here instead of
/// quietly leaving the CLI half on the old one.
fn forgekeep_diff_algorithm_setting() -> &'static str {
    match forgekeep_diff_algorithm() {
        gix::diff::blob::Algorithm::Histogram => "diff.algorithm=histogram",
        gix::diff::blob::Algorithm::Myers => "diff.algorithm=myers",
        gix::diff::blob::Algorithm::MyersMinimal => "diff.algorithm=minimal",
    }
}

const FORGEKEEP_LARGE_FILE_THRESHOLD_BYTES: u64 = 512 * 1024 * 1024;

/// Build the attribute view shared by ForgeKeep's native diff and merge paths.
///
/// Only attributes committed in the repository participate. In particular,
/// `core.attributesFile` and `$GIT_DIR/info/attributes` are deployment state,
/// not repository content, so neither is admitted into a server-owned answer.
fn forgekeep_committed_attribute_stack(repo: &gix::Repository) -> Result<gix::worktree::Stack> {
    let index = repo.index_or_load_from_head_or_empty()?;
    let mut attribute_buffer = Vec::new();
    let mut attribute_collection = gix::attrs::search::MetadataCollection::default();
    let attribute_globals = gix::attrs::Search::new_globals(
        std::iter::empty::<std::path::PathBuf>(),
        &mut attribute_buffer,
        &mut attribute_collection,
    )?;
    let attributes = gix::worktree::stack::state::Attributes::new(
        attribute_globals,
        None,
        gix::worktree::stack::state::attributes::Source::IdMapping,
        attribute_collection,
    );
    Ok(gix::worktree::Stack::from_state_and_ignore_case(
        repo.workdir().unwrap_or_else(|| repo.git_dir()),
        false,
        gix::worktree::stack::State::AttributesStack(attributes),
        &index,
        index.path_backing(),
    ))
}

/// Build the blob-diff platform from the parts ForgeKeep owns.
///
/// `Repository::diff_resource_cache` is deliberately Git-compatible: besides
/// in-tree `.gitattributes`, it reads `core.attributesFile`, `$GIT_DIR/info`,
/// `diff.<name>` drivers and `core.bigFileThreshold` from configuration. That is
/// the wrong ownership boundary for a server answer. Repository-local config is
/// deployment state rather than committed repository content, and an isolated
/// open still loads it.
///
/// Keep the built-in `binary` macro and `.gitattributes` from the repository's
/// current index, matching gix's tree-diff convention, but admit no external
/// attribute files or configured drivers. The explicit 512 MiB threshold is
/// gix's unconfigured default, so ordinary instances retain their old boundary
/// without allowing `.git/config` to turn an arbitrary text blob into a binary
/// zero-count.
fn forgekeep_diff_resource_cache(repo: &gix::Repository) -> Result<gix::diff::blob::Platform> {
    let attribute_stack = forgekeep_committed_attribute_stack(repo)?;

    let mut worktree_filter = gix::filter::plumbing::Pipeline::default();
    worktree_filter.options_mut().object_hash = repo.object_hash();
    let filter = gix::diff::blob::Pipeline::new(
        gix::diff::blob::pipeline::WorktreeRoots::default(),
        worktree_filter,
        Vec::new(),
        gix::diff::blob::pipeline::Options {
            large_file_threshold_bytes: FORGEKEEP_LARGE_FILE_THRESHOLD_BYTES,
            fs: Default::default(),
        },
    );

    Ok(gix::diff::blob::Platform::new(
        gix::diff::blob::platform::Options {
            algorithm: Some(forgekeep_diff_algorithm()),
            skip_internal_diff_if_external_is_configured: false,
        },
        filter,
        gix::diff::blob::pipeline::Mode::ToGit,
        attribute_stack,
    ))
}

/// Compute per-file diff statistics using gix tree-to-tree diff.
///
/// Replaces `git diff --numstat` with native gix tree-diff + per-blob line counting.
/// Returns file-level additions/deletions/status + aggregated totals.
///
/// Every failure is propagated: a ref that does not resolve, a tree-diff that
/// blows up, and a blob we cannot read or line-count all become an `Err`. The
/// one case that legitimately has no line count — a binary blob — is reported
/// as a zero numstat entry, the way `git diff --numstat` prints `-` for it.
fn gix_diff_numstat(
    repo_path: &std::path::Path,
    old_ref: String,
    new_ref: String,
) -> Result<(Vec<FileDiff>, DiffStats)> {
    use gix::bstr::ByteSlice;

    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let old_id = repo
        .rev_parse_single(old_ref.as_str())
        .with_context(|| format!("ref not found: {}", old_ref))?;
    let new_id = repo
        .rev_parse_single(new_ref.as_str())
        .with_context(|| format!("ref not found: {}", new_ref))?;

    // If refs point to the same tree, there are no changes
    if old_id == new_id {
        return Ok((
            vec![],
            DiffStats {
                total_additions: 0,
                total_deletions: 0,
                files_changed: 0,
            },
        ));
    }

    let old_tree = old_id
        .object()?
        .peel_to_tree()
        .map_err(|_| anyhow::anyhow!("{} is not a tree-ish", old_ref))?;
    let new_tree = new_id
        .object()?
        .peel_to_tree()
        .map_err(|_| anyhow::anyhow!("{} is not a tree-ish", new_ref))?;

    let mut platform = old_tree.changes()?;
    platform.options(|opts| {
        opts.track_rewrites(None);
    });

    let mut files = Vec::new();
    let mut total_additions = 0i64;
    let mut total_deletions = 0i64;

    let mut resource_cache = forgekeep_diff_resource_cache(&repo)?;

    let file_count;
    {
        let files_ref = &mut files;
        let total_add_ref = &mut total_additions;
        let total_del_ref = &mut total_deletions;

        platform
            .for_each_to_obtain_tree(
                &new_tree,
                |change| -> Result<std::ops::ControlFlow<()>, anyhow::Error> {
                    // `Change::diff` handles blobs and symlinks only. Two other
                    // shapes reach this callback and gix rejects both with the
                    // same "Can only diff blobs and links" — but they need
                    // opposite handling.
                    //
                    // A directory is dropped: it has no blob representation, and
                    // the leaf children that follow it are the file-level
                    // changes we expose to callers.
                    //
                    // A submodule is recorded as a gitlink (mode `160000`) and
                    // has no children — it IS the leaf. git reports it as a
                    // changed path spelled `Subproject commit <sha>`, so it is
                    // kept and line-counted below without a blob diff.
                    //
                    // Both sides are inspected, so a path that changes shape —
                    // `file -> submodule` and back — is classified by the side
                    // that cannot be diffed rather than by the other one.
                    let (previous_mode, entry_mode) = match &change {
                        gix::object::tree::diff::Change::Addition { entry_mode, .. }
                        | gix::object::tree::diff::Change::Deletion { entry_mode, .. } => {
                            (None, *entry_mode)
                        }
                        gix::object::tree::diff::Change::Modification {
                            previous_entry_mode,
                            entry_mode,
                            ..
                        } => (Some(*previous_entry_mode), *entry_mode),
                        gix::object::tree::diff::Change::Rewrite {
                            source_entry_mode,
                            entry_mode,
                            ..
                        } => (Some(*source_entry_mode), *entry_mode),
                    };
                    let sides = || std::iter::once(entry_mode).chain(previous_mode);
                    // The gitlink is asked about first: a `directory -> submodule`
                    // change carries both shapes at once, and dropping it as a
                    // directory would lose the only event that path ever gets.
                    let is_submodule = sides().any(|mode| mode.is_commit());
                    if !is_submodule && sides().any(|mode| mode.is_tree()) {
                        return Ok(std::ops::ControlFlow::Continue(()));
                    }

                    let location = change.location().to_str_lossy().to_string();

                    let (additions, deletions) = if is_submodule {
                        // git renders a gitlink as a one-line text file holding
                        // `Subproject commit <sha>`, and `git diff --numstat`
                        // counts that line: `1 0` for an added submodule, `0 1`
                        // for a removed one, `1 1` for a moved pointer. This
                        // function stands in for that command, and the patch
                        // attached to this very entry carries exactly that one
                        // line — a zero numstat would contradict it.
                        //
                        // The approximation is a path that swaps shape. For a
                        // three-line file replacing a gitlink git prints `3 1`,
                        // while this reports `1 1`: the blob side cannot be
                        // line-counted without the very diff gix refuses to run
                        // for the gitlink side, so only the pointer line is
                        // counted. An entry with an approximate count still
                        // beats the whole diff failing, and the patch attached
                        // to it is git's own, so the reader sees every line.
                        match &change {
                            gix::object::tree::diff::Change::Addition { .. } => (1, 0),
                            gix::object::tree::diff::Change::Deletion { .. } => (0, 1),
                            _ => (1, 1),
                        }
                    } else {
                        // Only `Ok(None)` means "this file has no line count" —
                        // gix answers that for a binary blob, and a zero numstat
                        // is the right report for it. An `Err` from either step
                        // means we could not read or diff the blob at all;
                        // swallowing it here would publish an unreadable file as
                        // an unchanged one.
                        match change
                            .diff(&mut resource_cache)
                            .with_context(|| format!("failed to diff changed blob: {location}"))?
                            .line_counts()
                            .with_context(|| {
                                format!("failed to count changed lines of blob: {location}")
                            })? {
                            Some(counts) => (counts.insertions as i64, counts.removals as i64),
                            None => (0, 0),
                        }
                    };

                    let status = match &change {
                        gix::object::tree::diff::Change::Addition { .. } => "added",
                        gix::object::tree::diff::Change::Deletion { .. } => "deleted",
                        _ => "modified",
                    };

                    *total_add_ref += additions;
                    *total_del_ref += deletions;

                    files_ref.push(FileDiff {
                        path: location,
                        status: status.to_string(),
                        additions,
                        deletions,
                        patch: None,
                        lines: Vec::new(),
                    });

                    resource_cache.clear_resource_cache_keep_allocation();
                    Ok(std::ops::ControlFlow::Continue(()))
                },
            )
            // `Error::ForEach` renders as a bare "the user-provided callback
            // failed" — keep it as a `source` instead of interpolating it, so
            // the per-file context raised above survives into `{err:#}`.
            .map_err(anyhow::Error::from)
            .context("tree-diff failed")?;

        file_count = files.len() as i64;
    }

    Ok((
        files,
        DiffStats {
            total_additions,
            total_deletions,
            files_changed: file_count,
        },
    ))
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    /// `main` → `feature`, where the feature commit touches a nested text file
    /// and a nested binary file. Returns the work tree, which is also the repo
    /// path we hand to [`gix_diff_numstat`].
    fn repo_with_a_text_and_a_binary_change() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        git.run_or_bail(&["init", "-q", "-b", "main", work.to_str().unwrap()], None)
            .unwrap();
        for args in [
            ["config", "user.name", "PR diff test"],
            ["config", "user.email", "prdiff@example.com"],
            ["config", "commit.gpgsign", "false"],
        ] {
            git.run_or_bail(&args, Some(&work)).unwrap();
        }

        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::create_dir_all(work.join("assets")).unwrap();
        std::fs::write(work.join("src/lib.rs"), "one\ntwo\n").unwrap();
        std::fs::write(work.join("assets/blob.bin"), [0u8, 1, 2, 0, 3]).unwrap();
        git.run_or_bail(&["add", "."], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "base"], Some(&work))
            .unwrap();

        git.run_or_bail(&["checkout", "-q", "-b", "feature"], Some(&work))
            .unwrap();
        std::fs::write(work.join("src/lib.rs"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(work.join("assets/blob.bin"), [0u8, 9, 9, 0, 7, 7]).unwrap();
        git.run_or_bail(&["add", "."], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "change"], Some(&work))
            .unwrap();

        (dir, work)
    }

    fn numstat(work: &std::path::Path) -> Result<(Vec<FileDiff>, DiffStats)> {
        gix_diff_numstat(
            work,
            "refs/heads/main".to_string(),
            "refs/heads/feature".to_string(),
        )
    }

    /// The legitimate half of the old `.ok().flatten()`: gix answers `Ok(None)`
    /// for a binary blob, and a zero numstat is the correct report for it —
    /// exactly what `git diff --numstat` prints as `-`.
    #[test]
    fn a_binary_blob_stays_a_zero_numstat_entry() {
        let (_dir, work) = repo_with_a_text_and_a_binary_change();

        let (files, stats) = numstat(&work).expect("a readable repository must diff");

        assert_eq!(stats.files_changed, 2, "both files changed: {files:?}");
        let binary = files
            .iter()
            .find(|f| f.path == "assets/blob.bin")
            .expect("the binary file must still be listed as changed");
        assert_eq!(
            (binary.additions, binary.deletions),
            (0, 0),
            "a binary blob has no line count — that is a zero numstat, not an error"
        );
        let text = files.iter().find(|f| f.path == "src/lib.rs").unwrap();
        assert_eq!((text.additions, text.deletions), (1, 0));
        assert_eq!((stats.total_additions, stats.total_deletions), (1, 0));
    }

    /// The defect half: with `.diff(..).ok()` / `.line_counts().ok().flatten()`
    /// a blob we cannot read was indistinguishable from a binary one, so the PR
    /// diff answered `200` with a plausible zero numstat for a file that had in
    /// fact changed. Deleting the loose object of the new-side blob reproduces
    /// it; the whole call must now fail, naming the file.
    #[test]
    fn an_unreadable_blob_fails_the_whole_numstat() {
        let (_dir, work) = repo_with_a_text_and_a_binary_change();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        // Sanity: the same repository diffs cleanly while every object is readable.
        numstat(&work).expect("the fixture must diff before we break it");

        let oid = git
            .run(&["rev-parse", "feature:src/lib.rs"], Some(&work))
            .unwrap()
            .stdout_str()
            .trim()
            .to_string();
        let object = work
            .join(".git")
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        std::fs::remove_file(&object)
            .unwrap_or_else(|e| panic!("loose object {object:?} must exist: {e}"));

        let err =
            numstat(&work).expect_err("an unreadable blob must not be reported as zero changes");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("src/lib.rs"),
            "the error must name the file it failed on, got: {rendered}"
        );
    }

    /// `main` → `feature`, where the feature commit adds a submodule next to an
    /// ordinary source change. Returns the work tree, which is also the repo
    /// path we hand to [`gix_diff_numstat`].
    fn repo_with_a_submodule_added() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        let work = dir.path().join("work");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        for repo in [&inner, &work] {
            git.run_or_bail(&["init", "-q", "-b", "main", repo.to_str().unwrap()], None)
                .unwrap();
            for args in [
                ["config", "user.name", "PR diff test"],
                ["config", "user.email", "prdiff@example.com"],
                ["config", "commit.gpgsign", "false"],
            ] {
                git.run_or_bail(&args, Some(repo)).unwrap();
            }
        }

        std::fs::write(inner.join("inner.txt"), "vendored\n").unwrap();
        git.run_or_bail(&["add", "."], Some(&inner)).unwrap();
        git.run_or_bail(&["commit", "-qm", "vendored base"], Some(&inner))
            .unwrap();

        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join("src/lib.rs"), "one\ntwo\n").unwrap();
        git.run_or_bail(&["add", "."], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "base"], Some(&work))
            .unwrap();

        git.run_or_bail(&["checkout", "-q", "-b", "feature"], Some(&work))
            .unwrap();
        std::fs::write(work.join("src/lib.rs"), "one\ntwo\nthree\n").unwrap();
        // git refuses the file transport for submodules by default since 2.38,
        // and a fixture cloning a sibling directory is exactly the case that
        // switch guards — so it is granted for this one command.
        git.run_or_bail(
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                inner.to_str().unwrap(),
                "vendor",
            ],
            Some(&work),
        )
        .unwrap();
        git.run_or_bail(&["add", "."], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "add a submodule"], Some(&work))
            .unwrap();

        (dir, work)
    }

    /// `card_7360e65022ec` — a submodule is a gitlink (mode `160000`), and
    /// `Change::diff` rejects it the same way it rejects a directory: "Can only
    /// diff blobs and links, not Commit". Before the fix that error was not
    /// confined to the submodule — it failed the WHOLE diff, so a pull request
    /// touching a submodule showed no files at all and, because CODEOWNERS is
    /// advisory on the create path, silently got no reviewer.
    #[test]
    fn a_submodule_is_a_changed_entry_not_a_failed_diff() {
        let (_dir, work) = repo_with_a_submodule_added();

        let (files, stats) = numstat(&work).expect("a submodule must not fail the whole diff");

        let submodule = files
            .iter()
            .find(|file| file.path == "vendor")
            .unwrap_or_else(|| panic!("the submodule must be listed as changed: {files:?}"));
        assert_eq!(submodule.status, "added");
        // `git diff --numstat` prints `1 0` here: the gitlink renders as the one
        // line `Subproject commit <sha>`.
        assert_eq!((submodule.additions, submodule.deletions), (1, 0));

        let text = files
            .iter()
            .find(|file| file.path == "src/lib.rs")
            .unwrap_or_else(|| {
                panic!("the ordinary file of the same commit must survive the submodule: {files:?}")
            });
        assert_eq!((text.additions, text.deletions), (1, 0));

        // `.gitmodules` is written by `submodule add` and is an ordinary file.
        assert!(
            files.iter().any(|file| file.path == ".gitmodules"),
            "the submodule registration file is an ordinary changed file: {files:?}"
        );
        assert_eq!(stats.files_changed, 3, "{files:?}");
    }

    /// The two-sided half of the classification. A path that stops being a
    /// submodule keeps its gitlink on the OLD side only, so a check that looked
    /// at the new entry mode alone would hand it to the blob diff and fail the
    /// whole diff again — the same defect wearing the other shape.
    #[test]
    fn a_path_that_stops_being_a_submodule_is_still_a_changed_entry() {
        let (dir, work) = repo_with_a_submodule_added();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let _ = dir;

        // `main` gains the submodule, so the shape change is what `feature`
        // does to it: a gitlink on the old side, an ordinary file on the new.
        git.run_or_bail(&["checkout", "-q", "main"], Some(&work))
            .unwrap();
        git.run_or_bail(&["merge", "-q", "--ff-only", "feature"], Some(&work))
            .unwrap();
        git.run_or_bail(&["checkout", "-q", "-B", "feature"], Some(&work))
            .unwrap();
        git.run_or_bail(&["rm", "-q", "--cached", "vendor"], Some(&work))
            .unwrap();
        std::fs::remove_dir_all(work.join("vendor")).unwrap();
        std::fs::write(work.join("vendor"), "a\nb\nc\n").unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(
            &["commit", "-qm", "the submodule becomes a file"],
            Some(&work),
        )
        .unwrap();

        let (files, _stats) =
            numstat(&work).expect("a path that stops being a submodule must not fail the diff");

        let swapped = files
            .iter()
            .find(|file| file.path == "vendor")
            .unwrap_or_else(|| panic!("the changed path must be listed: {files:?}"));
        assert_eq!(swapped.status, "modified");
        // git prints `3 1` here; the blob side is not line-counted — see the
        // gitlink branch of `gix_diff_numstat`.
        assert_eq!((swapped.additions, swapped.deletions), (1, 1));
    }

    /// The same repository through the bare clone a server actually serves, and
    /// through the whole `compute_diff` path rather than the numstat helper —
    /// the patch text comes from git there, so the submodule entry must survive
    /// the join between the two.
    #[test]
    fn a_submodule_survives_the_full_diff_of_a_bare_repo() {
        let (dir, work) = repo_with_a_submodule_added();
        let bare = dir.path().join("repo.git");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        git.run_or_bail(
            &[
                "clone",
                "--bare",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            None,
        )
        .unwrap();

        let pr = pull_request_row("main", "feature");
        let diff = compute_same_repo_diff(&bare, &pr).expect("a bare repository must diff");

        let submodule = diff
            .files_changed
            .iter()
            .find(|file| file.path == "vendor")
            .unwrap_or_else(|| panic!("the submodule must reach the PR diff: {diff:?}"));
        assert_eq!((submodule.additions, submodule.deletions), (1, 0));
        assert!(
            submodule
                .patch
                .as_deref()
                .is_some_and(|patch| patch.contains("Subproject commit")),
            "the gitlink patch git printed must be attached to it: {submodule:?}"
        );
        assert!(
            diff.files_changed
                .iter()
                .any(|file| file.path == "src/lib.rs"),
            "the ordinary file of the same commit must be listed too: {diff:?}"
        );
    }

    #[test]
    fn a_nested_file_change_in_a_bare_repo_has_a_file_numstat() {
        let (dir, work) = repo_with_a_text_and_a_binary_change();
        let bare = dir.path().join("repo.git");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        git.run_or_bail(
            &[
                "clone",
                "--bare",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            None,
        )
        .unwrap();

        let (files, stats) = numstat(&bare).expect("a bare repository must diff nested files");
        assert_eq!(stats.files_changed, 2, "both leaf files changed: {files:?}");
        let text = files
            .iter()
            .find(|file| file.path == "src/lib.rs")
            .expect("the nested source file must be reported, not its src directory");
        assert_eq!((text.additions, text.deletions), (1, 0));
    }

    #[test]
    fn splits_patch_by_file_and_parses_line_numbers() {
        let diff = concat!(
            "diff --git a/src/a.rs b/src/a.rs\n",
            "index 111..222 100644\n",
            "--- a/src/a.rs\n",
            "+++ b/src/a.rs\n",
            "@@ -2,2 +2,3 @@\n",
            " same\n",
            "-old\n",
            "+new\n",
            "+extra\n",
            "diff --git a/README.md b/README.md\n",
            "--- a/README.md\n",
            "+++ b/README.md\n",
            "@@ -1 +1 @@\n",
            "-before\n",
            "+after\n",
        );
        let patches = split_unified_diff(diff);
        assert_eq!(patches.len(), 2);
        let lines = parse_diff_lines(&patches["src/a.rs"]);
        assert!(lines.iter().any(|line| {
            line.kind == "deletion" && line.old_line == Some(3) && line.content == "old"
        }));
        assert!(lines.iter().any(|line| {
            line.kind == "addition" && line.new_line == Some(4) && line.content == "extra"
        }));
    }

    /// `card_c9ecf8644e12` — the key a patch is filed under has to be the path
    /// numstat named, in every spelling git prints.
    ///
    /// The header line is the ambiguous one: `a/…` and `b/…` are separated by a
    /// space that a committed path is allowed to contain, and git escapes `"`
    /// and `\` there whatever `core.quotePath` says. The fixture below is
    /// `git diff` output captured verbatim from git 2.x, not a hand-written
    /// approximation.
    #[test]
    fn a_patch_is_keyed_by_the_path_git_named_however_git_spelled_it() {
        let diff = concat!(
            "diff --git \"a/back\\\\slash.txt\" \"b/back\\\\slash.txt\"\n",
            "index 1a9cc2b..e563bc2 100644\n",
            "--- \"a/back\\\\slash.txt\"\n",
            "+++ \"b/back\\\\slash.txt\"\n",
            "@@ -1 +1,2 @@\n",
            " p\n",
            "+q\n",
            "diff --git a/migr.sql b/migr.sql\n",
            "index 7d2e37d..2fa992c 100644\n",
            "--- a/migr.sql\n",
            "+++ b/migr.sql\n",
            "@@ -1,2 +1,2 @@\n",
            "--- sql comment\n",
            " keep\n",
            "+++ plus line\n",
            "diff --git a/my file.txt b/my file.txt\n",
            "deleted file mode 100644\n",
            "index 422c2b7..0000000\n",
            "--- a/my file.txt\t\n",
            "+++ /dev/null\n",
            "@@ -1,2 +0,0 @@\n",
            "-a\n",
            "-b\n",
            "diff --git \"a/we\\\"ird.txt\" \"b/we\\\"ird2.txt\"\n",
            "similarity index 50%\n",
            "rename from \"we\\\"ird.txt\"\n",
            "rename to \"we\\\"ird2.txt\"\n",
            "index 587be6b..b77b4eb 100644\n",
            "--- \"a/we\\\"ird.txt\"\n",
            "+++ \"b/we\\\"ird2.txt\"\n",
            "@@ -1 +1,2 @@\n",
            " x\n",
            "+y\n",
        );

        let patches = split_unified_diff(diff);

        let mut keys: Vec<&str> = patches.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["back\\slash.txt", "migr.sql", "my file.txt", "we\"ird2.txt",],
            "every entry must be filed under the raw path, not under a fragment \
             of the header or a still-escaped spelling"
        );

        // A deletion is the case the header line alone has to answer: its `+++`
        // half is `/dev/null`, so nothing below the header repeats the name.
        let deleted = &patches["my file.txt"];
        assert!(
            deleted.contains("-a\n") && deleted.contains("-b\n"),
            "the deleted file must carry its own hunk, got: {deleted}"
        );
        assert!(
            patches["we\"ird2.txt"].contains("+y\n"),
            "a quoted path keeps its hunk: {:?}",
            patches["we\"ird2.txt"]
        );
        // `--- sql comment` and `+++ plus line` are a deleted and an added
        // line, not the two halves of a header: `-- sql comment` and
        // `++ plus line` are what a reviewer wrote.
        assert!(
            patches["migr.sql"].contains("--- sql comment\n")
                && patches["migr.sql"].contains("+++ plus line\n"),
            "content that looks like a header must stay with its own file: {:?}",
            patches["migr.sql"]
        );
    }

    /// A rename that changes nothing prints no `---`/`+++` pair at all, and its
    /// header is the ambiguous kind when either name carries a space. The
    /// `rename to` line is the only unambiguous spelling left.
    #[test]
    fn a_pure_rename_is_keyed_by_its_rename_to_line() {
        let diff = concat!(
            "diff --git a/my file.txt b/my other file.txt\n",
            "similarity index 100%\n",
            "rename from my file.txt\n",
            "rename to my other file.txt\n",
        );

        let patches = split_unified_diff(diff);

        assert_eq!(
            patches.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["my other file.txt"],
            "got {patches:?}"
        );
    }

    /// One entry of the numstat half, as `gix_diff_numstat` leaves it before
    /// the patch half is joined onto it.
    fn listed_file(path: &str) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            status: "modified".to_string(),
            additions: 0,
            deletions: 0,
            patch: None,
            lines: Vec::new(),
        }
    }

    /// `card_3645e5c278da` — the join of the two halves has to have a voice.
    ///
    /// A key that does not match leaves `patch: null`, which reads exactly like
    /// a file that has nothing to show, so the two causes found so far were
    /// both caught by hand on a live repository. The fixture below disagrees in
    /// both directions at once: `ghost.txt` is listed and never patched, and
    /// `stray.txt` is patched and on nobody's list — the second one does not
    /// even reach the answer.
    #[test]
    fn halves_that_do_not_meet_are_named_in_one_warning() {
        let diff = concat!(
            "diff --git a/src/a.rs b/src/a.rs\n",
            "--- a/src/a.rs\n",
            "+++ b/src/a.rs\n",
            "@@ -1 +1,2 @@\n",
            " same\n",
            "+new\n",
            "diff --git a/stray.txt b/stray.txt\n",
            "--- a/stray.txt\n",
            "+++ b/stray.txt\n",
            "@@ -1 +1 @@\n",
            "-before\n",
            "+after\n",
        );
        let mut files = vec![listed_file("src/a.rs"), listed_file("ghost.txt")];

        let rendered = {
            let (logs, _guard) = crate::test_support::CapturedLogs::capture();
            attach_patches(&mut files, diff);
            logs.rendered()
        };

        assert!(
            files[0].patch.is_some(),
            "the file both halves named still gets its patch: {files:?}"
        );
        assert!(
            files[1].patch.is_none(),
            "nothing invents a patch for a file the diff never printed: {files:?}"
        );
        assert_eq!(
            rendered.matches("do not name the same files").count(),
            1,
            "one join, one warning: {rendered}"
        );
        assert!(
            rendered.contains("ghost.txt"),
            "a listed file no patch claimed must be named: {rendered}"
        );
        assert!(
            rendered.contains("stray.txt"),
            "a patched entry on nobody's list must be named — it is dropped \
             from the answer entirely: {rendered}"
        );
        assert!(
            rendered.contains("listed_files=2") && rendered.contains("patch_entries=2"),
            "both halves report how much they counted, so the log says who \
             undercounted: {rendered}"
        );
    }

    /// The other half of the detector: a join that meets is silent, so the
    /// warning above means something when it appears.
    #[test]
    fn halves_that_meet_say_nothing() {
        let diff = concat!(
            "diff --git a/src/a.rs b/src/a.rs\n",
            "--- a/src/a.rs\n",
            "+++ b/src/a.rs\n",
            "@@ -1 +1,2 @@\n",
            " same\n",
            "+new\n",
        );
        let mut files = vec![listed_file("src/a.rs")];

        let rendered = {
            let (logs, _guard) = crate::test_support::CapturedLogs::capture();
            attach_patches(&mut files, diff);
            logs.rendered()
        };

        assert!(files[0].patch.is_some(), "got {files:?}");
        assert!(
            rendered.is_empty(),
            "a join that meets must not warn: {rendered}"
        );
    }

    /// `main` → `feature`, where the feature commit deletes a file whose name
    /// contains a space, edits one whose name git quotes unconditionally, and
    /// touches a binary file whose name contains a space. Neither spelling is
    /// switched off by the `core.quotePath=false` the diff call sites pass.
    ///
    /// The binary file is the entry that pins the `diff --git` header itself:
    /// git prints `Binary files … differ` and no `---`/`+++` pair, so the
    /// header line is the only place that entry names its path.
    fn repo_with_awkwardly_named_changes() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        git.run_or_bail(&["init", "-q", "-b", "main", work.to_str().unwrap()], None)
            .unwrap();
        for args in [
            ["config", "user.name", "PR diff test"],
            ["config", "user.email", "prdiff@example.com"],
            ["config", "commit.gpgsign", "false"],
        ] {
            git.run_or_bail(&args, Some(&work)).unwrap();
        }

        std::fs::write(work.join("my file.txt"), "a\nb\n").unwrap();
        std::fs::write(work.join("we\"ird.txt"), "x\n").unwrap();
        std::fs::write(work.join("my blob.bin"), [0u8, 1, 2, 0]).unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "base"], Some(&work))
            .unwrap();

        git.run_or_bail(&["checkout", "-q", "-b", "feature"], Some(&work))
            .unwrap();
        std::fs::remove_file(work.join("my file.txt")).unwrap();
        std::fs::write(work.join("we\"ird.txt"), "x\ny\n").unwrap();
        std::fs::write(work.join("my blob.bin"), [0u8, 9, 9, 0, 7]).unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "change"], Some(&work))
            .unwrap();

        (dir, work)
    }

    /// The row `compute_same_repo_diff` is handed. Only the two branch names
    /// and the absent head SHA matter here — with no `head_sha` the diff is
    /// taken from `refs/heads/<head>`, which is what the fixture builds.
    fn pull_request_row(base: &str, head: &str) -> PullRequest {
        let now = chrono::Utc::now();
        PullRequest {
            id: 1,
            repo_id: 1,
            number: 1,
            title: "awkward names".to_string(),
            body: None,
            state: "open".to_string(),
            is_draft: false,
            auto_merge_enabled: false,
            auto_merge_strategy: None,
            auto_merge_enabled_by_id: None,
            auto_merge_enabled_at: None,
            author_id: 1,
            reviewer_id: None,
            head_branch: head.to_string(),
            base_branch: base.to_string(),
            head_sha: None,
            merge_strategy: None,
            merge_commit_sha: None,
            head_repo_id: None,
            ci_approved_sha: None,
            ci_approved_by: None,
            ci_approved_at: None,
            milestone_id: None,
            labels: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            merged_at: None,
        }
    }

    /// `card_c9ecf8644e12` — the defect as the reviewer meets it: the whole
    /// same-repo diff, numstat and patch text together, not the helper alone.
    ///
    /// Both files are listed either way; what the old reader lost was the
    /// patch, so the page showed a changed file with no line in it and nothing
    /// saying the diff had not been read to the end.
    #[test]
    fn every_changed_file_carries_its_patch_whatever_its_name() {
        let (_dir, work) = repo_with_awkwardly_named_changes();

        let diff = compute_same_repo_diff(&work, &pull_request_row("main", "feature"))
            .expect("the fixture repository must diff");

        let listed: Vec<&str> = diff
            .files_changed
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(diff.files_changed.len(), 3, "listed: {listed:?}");

        for file in &diff.files_changed {
            assert!(
                file.patch.is_some() && !file.lines.is_empty(),
                "'{}' is listed as changed but carries no patch: {file:?}",
                file.path
            );
        }

        let deleted = diff
            .files_changed
            .iter()
            .find(|file| file.path == "my file.txt")
            .unwrap_or_else(|| panic!("the deleted file must be listed, got {listed:?}"));
        assert!(
            deleted
                .lines
                .iter()
                .any(|line| line.kind == "deletion" && line.content == "a"),
            "the deleted file's own lines must be there: {:?}",
            deleted.lines
        );

        let quoted = diff
            .files_changed
            .iter()
            .find(|file| file.path == "we\"ird.txt")
            .unwrap_or_else(|| panic!("the quoted file must be listed, got {listed:?}"));
        assert!(
            quoted
                .lines
                .iter()
                .any(|line| line.kind == "addition" && line.content == "y"),
            "the quoted file's added line must be there: {:?}",
            quoted.lines
        );

        let binary = diff
            .files_changed
            .iter()
            .find(|file| file.path == "my blob.bin")
            .unwrap_or_else(|| panic!("the binary file must be listed, got {listed:?}"));
        assert!(
            binary
                .patch
                .as_deref()
                .is_some_and(|patch| patch.contains("Binary files")),
            "a binary entry names no path below its header, so the header is \
             the only thing that can key it: {binary:?}"
        );
    }

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// `main` → `feature`, where the feature commit renames a file and adds a
    /// line to it.
    ///
    /// This is the one change on which the two halves of the answer do not even
    /// agree about how many entries it has: the numstat walks the tree with
    /// `track_rewrites(None)` and sees a deletion plus an addition, while
    /// `git diff` used to be asked for rename detection and printed a single
    /// entry keyed by the new path.
    fn repo_with_a_renamed_and_edited_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        git.run_or_bail(&["init", "-q", "-b", "main", work.to_str().unwrap()], None)
            .unwrap();
        for args in [
            ["config", "user.name", "PR diff test"],
            ["config", "user.email", "prdiff@example.com"],
            ["config", "commit.gpgsign", "false"],
        ] {
            git.run_or_bail(&args, Some(&work)).unwrap();
        }

        std::fs::write(work.join("old.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "base"], Some(&work))
            .unwrap();

        git.run_or_bail(&["checkout", "-q", "-b", "feature"], Some(&work))
            .unwrap();
        std::fs::rename(work.join("old.txt"), work.join("new.txt")).unwrap();
        std::fs::write(work.join("new.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "rename and edit"], Some(&work))
            .unwrap();

        (dir, work)
    }

    /// `card_283b386e7091` — a renamed file made the answer disagree with
    /// itself, and neither half said so.
    ///
    /// The numstat listed both paths; the patch, read with rename detection on,
    /// carried a single entry under the new path. So the old path was published
    /// as a changed file with no patch and no lines, and the new path published
    /// a whole-file `+5` above a patch in which one line had been added.
    ///
    /// Both halves of the assertion matter and fail on different flags: the
    /// first catches the entry that lost its patch, the second the entry whose
    /// numbers stopped describing its own patch.
    #[test]
    fn a_renamed_file_is_counted_and_patched_by_one_model() {
        let (_dir, work) = repo_with_a_renamed_and_edited_file();

        let diff = compute_same_repo_diff(&work, &pull_request_row("main", "feature"))
            .expect("the fixture repository must diff");

        let listed: Vec<&str> = diff
            .files_changed
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            listed
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            ["new.txt", "old.txt"].into_iter().collect(),
            "a rename counted without rewrite tracking is a deletion plus an addition"
        );

        // Every entry is judged before anything is reported, so one run names
        // both halves of the contradiction: the entry that lost its patch, and
        // the entry whose numbers stopped describing its own patch. An
        // assertion per entry would stop at whichever came first.
        let mut disagreements = Vec::new();
        for file in &diff.files_changed {
            if file.patch.is_none() || file.lines.is_empty() {
                disagreements.push(format!(
                    "'{}' is listed as changed but carries no patch",
                    file.path
                ));
                continue;
            }
            let counted =
                |kind: &str| file.lines.iter().filter(|line| line.kind == kind).count() as i64;
            let shown = (counted("addition"), counted("deletion"));
            if shown != (file.additions, file.deletions) {
                disagreements.push(format!(
                    "'{}' reports +{} -{} over a patch showing +{} -{}",
                    file.path, file.additions, file.deletions, shown.0, shown.1
                ));
            }
        }
        assert!(
            disagreements.is_empty(),
            "the two halves of the diff describe different changes: {disagreements:?}"
        );
    }

    /// `main` → `feature`, where the feature commit deletes a line that opens
    /// with `--` and adds one that opens with `++`.
    ///
    /// A `.sql` migration is the least contrived carrier there is: `--` opens a
    /// comment in SQL, and this repository ships `.sql` files. Git prints the
    /// deletion as `--- sql comment` and the addition as `+++ counter`, which
    /// is what a reader looking for `---` / `+++` labels mistakes for
    /// structure.
    fn repo_with_a_dash_dash_line(root: &std::path::Path) -> std::path::PathBuf {
        let work = root.join("work");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        git.run_or_bail(&["init", "-q", "-b", "main", work.to_str().unwrap()], None)
            .unwrap();
        for args in [
            ["config", "user.name", "PR diff test"],
            ["config", "user.email", "prdiff@example.com"],
            ["config", "commit.gpgsign", "false"],
        ] {
            git.run_or_bail(&args, Some(&work)).unwrap();
        }

        std::fs::write(work.join("migr.sql"), "-- sql comment\nkeep\ntail\n").unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "base"], Some(&work))
            .unwrap();

        git.run_or_bail(&["checkout", "-q", "-b", "feature"], Some(&work))
            .unwrap();
        std::fs::write(work.join("migr.sql"), "keep\n++ counter\ntail\n").unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "drop the comment"], Some(&work))
            .unwrap();

        work
    }

    /// `card_aab572c59c7b` — a changed line that spells a diff label is still a
    /// changed line, and the numbers after it belong to the lines that carry
    /// them.
    ///
    /// The number is not cosmetic: a review comment is stored against
    /// `old_line` / `new_line`, so a hunk shifted by one files every comment
    /// below it against the wrong line.
    #[test]
    fn a_changed_line_that_spells_a_diff_label_is_still_a_changed_line() {
        let dir = tempfile::tempdir().unwrap();
        let work = repo_with_a_dash_dash_line(dir.path());

        let diff = compute_same_repo_diff(&work, &pull_request_row("main", "feature"))
            .expect("the fixture repository must diff");
        let file = diff
            .files_changed
            .iter()
            .find(|file| file.path == "migr.sql")
            .expect("the changed file must be listed");
        let at = |kind: &str, content: &str| {
            file.lines
                .iter()
                .find(|line| line.kind == kind && line.content == content)
                .unwrap_or_else(|| panic!("no {kind} '{content}' in {:?}", file.lines))
        };

        let deleted = at("deletion", "-- sql comment");
        assert_eq!(deleted.old_line, Some(1), "{deleted:?}");
        let added = at("addition", "++ counter");
        assert_eq!(added.new_line, Some(2), "{added:?}");

        // The line after both of them is where a lost `-`/`+` shows up as a
        // wrong number rather than as a missing line.
        let tail = at("context", "tail");
        assert_eq!(
            (tail.old_line, tail.new_line),
            (Some(3), Some(3)),
            "the numbers after a label-shaped change are off: {:?}",
            file.lines
        );

        let counted =
            |kind: &str| file.lines.iter().filter(|line| line.kind == kind).count() as i64;
        assert_eq!(
            (counted("addition"), counted("deletion")),
            (file.additions, file.deletions),
            "a line read as structure drops out of its own file's count: {:?}",
            file.lines
        );
    }

    /// `main` → `feature`, where the base moves on AFTER the branch point.
    ///
    /// That is the ordinary life of an open pull request: while the branch sits
    /// under review, someone else lands a commit on `main`. `base_only.txt` is
    /// that commit — a file this pull request never touched, and one the head
    /// does not have, so a diff taken against the base TIP reports it as a
    /// deletion the head is innocent of.
    fn repo_whose_base_moved_on(root: &std::path::Path) -> std::path::PathBuf {
        let work = root.join("work");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        git.run_or_bail(&["init", "-q", "-b", "main", work.to_str().unwrap()], None)
            .unwrap();
        for args in [
            ["config", "user.name", "PR diff test"],
            ["config", "user.email", "prdiff@example.com"],
            ["config", "commit.gpgsign", "false"],
        ] {
            git.run_or_bail(&args, Some(&work)).unwrap();
        }

        std::fs::write(work.join("shared.txt"), "one\ntwo\n").unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "base"], Some(&work))
            .unwrap();

        git.run_or_bail(&["checkout", "-q", "-b", "feature"], Some(&work))
            .unwrap();
        std::fs::write(work.join("shared.txt"), "one\ntwo\nthree\n").unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(
            &["commit", "-qm", "the change this pull request is"],
            Some(&work),
        )
        .unwrap();

        git.run_or_bail(&["checkout", "-q", "main"], Some(&work))
            .unwrap();
        std::fs::write(work.join("base_only.txt"), "landed on main meanwhile\n").unwrap();
        git.run_or_bail(&["add", "-A"], Some(&work)).unwrap();
        git.run_or_bail(&["commit", "-qm", "somebody else's work"], Some(&work))
            .unwrap();

        work
    }

    /// `card_dd105a36fc64` — a pull request answers for what it changes, not
    /// for what the base did after it branched off.
    ///
    /// The numstat half used to be walked from the base TIP (two-dot) while the
    /// patch half was read as `base...head` (three-dot, from the merge base),
    /// so `base_only.txt` was published as a `deleted` file with `patch: null`
    /// and its line counted into `stats` — a file the reviewer is being shown
    /// as part of a pull request that never touched it.
    #[test]
    fn a_base_that_moved_on_is_not_part_of_the_pull_request() {
        let dir = tempfile::tempdir().unwrap();
        let work = repo_whose_base_moved_on(dir.path());

        // The detector from `card_3645e5c278da` is the second half of this
        // test: on the defect the two halves name different files, so a run
        // that is right must also be a run that is silent.
        let (diff, rendered) = {
            let (logs, _guard) = crate::test_support::CapturedLogs::capture();
            let diff = compute_same_repo_diff(&work, &pull_request_row("main", "feature"))
                .expect("the fixture repository must diff");
            (diff, logs.rendered())
        };

        let listed: Vec<&str> = diff
            .files_changed
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            listed,
            vec!["shared.txt"],
            "only the file the pull request changed belongs in its diff"
        );
        assert_eq!(
            (
                diff.stats.total_additions,
                diff.stats.total_deletions,
                diff.stats.files_changed
            ),
            (1, 0, 1),
            "the totals count the pull request's own lines: {:?}",
            diff.stats
        );

        let file = &diff.files_changed[0];
        assert!(
            file.patch.is_some()
                && file
                    .lines
                    .iter()
                    .any(|line| line.kind == "addition" && line.content == "three"),
            "the one changed file still carries its own patch: {file:?}"
        );
        assert!(
            rendered.is_empty(),
            "the two halves must name the same files, so nothing warns: {rendered}"
        );
    }

    /// `card_dd105a36fc64` — the census beside the behavioural test above, and
    /// the fork path's only cover: both diff paths take the two revisions they
    /// read from ONE resolver, instead of each spelling a range of its own.
    #[test]
    fn both_diff_paths_take_their_two_revisions_from_one_place() {
        let source = include_str!("service.rs");

        for function in ["compute_same_repo_diff", "compute_cross_repo_diff"] {
            assert_eq!(
                rust_source::production_function_call_sites(
                    source,
                    function,
                    &["forgekeep_diff_revs"]
                )
                .len(),
                1,
                "`{function}` no longer resolves its revisions through \
                 `forgekeep_diff_revs`, so the range of its numstat half is chosen \
                 independently of its patch half's again — see card_dd105a36fc64"
            );
        }
    }

    /// The census the behavioural test above cannot carry: it exercises the
    /// same-repo path only, and a fork pull request takes the other one.
    ///
    /// What it pins is that there is ONE reader, and that the reader builds ONE
    /// argv. The flags inside it are the behavioural tests' job — and because
    /// both paths read through that same function, pinning it once covers the
    /// fork path too. Each half asserts a presence, not only an absence, so a
    /// census that stopped finding the functions would not report success about
    /// code it never read.
    #[test]
    fn both_diff_paths_read_their_patch_under_one_rename_model() {
        let source = include_str!("service.rs");

        for function in ["compute_same_repo_diff", "compute_cross_repo_diff"] {
            assert_eq!(
                rust_source::production_function_call_sites(
                    source,
                    function,
                    &["forgekeep_patch_text"]
                )
                .len(),
                1,
                "`{function}` no longer reads its patch through `forgekeep_patch_text`, \
                 so the rename model of its patch half is chosen independently of the \
                 numstat's again — see card_283b386e7091"
            );
        }

        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_patch_text",
                &["forgekeep_patch_argv"]
            )
            .len(),
            1,
            "`forgekeep_patch_text` no longer builds its argv through \
             `forgekeep_patch_argv` — either it was reverted, or this census has \
             stopped reading the function"
        );
    }

    /// The patch half of a pull-request diff must be a decision of ForgeKeep's,
    /// the same way the numstat half already is.
    ///
    /// The behavioural cover is
    /// `diff_configuration_ownership_tests::the_patch_half_is_counted_the_way_the_numstat_half_is`,
    /// which drives `forgekeep_patch_text` itself. What this adds is naming the
    /// two decisions behind it: running under ForgeKeep's git configuration
    /// instead of a bare gateway, and deriving the stated algorithm from
    /// `forgekeep_diff_algorithm` instead of spelling `myers` a second time.
    #[test]
    fn the_patch_half_states_the_configuration_it_is_read_under() {
        let source = include_str!("service.rs");

        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_patch_text",
                &["rg_git::invocation::local"]
            )
            .len(),
            1,
            "`forgekeep_patch_text` no longer states the configuration it reads the patch \
             under — either it was reverted, or this census has stopped reading the function"
        );
        assert!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_patch_text",
                &["gateway.run", "gateway.run_with_env", "gateway.run_bounded"]
            )
            .is_empty(),
            "`forgekeep_patch_text` runs git straight off the gateway again, so the patch \
             half is read under whatever the installed git defaults to — see card_4a21d30afb9a"
        );
        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_patch_argv",
                &["forgekeep_diff_algorithm_setting"]
            )
            .len(),
            1,
            "the patch argv no longer states its diff algorithm, so the patch under the \
             numbers is counted with a different algorithm than the numbers — see \
             card_4a21d30afb9a"
        );
        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_diff_algorithm_setting",
                &["forgekeep_diff_algorithm"]
            )
            .len(),
            1,
            "the CLI half's diff algorithm is no longer derived from \
             `forgekeep_diff_algorithm`, so the two halves of one answer can drift apart \
             again — see card_4a21d30afb9a"
        );
    }
}

/// `card_25afc5bcc044` — the number of lines a pull request adds and removes
/// must be a property of ForgeKeep, not of the machine the instance runs on.
///
/// A diff algorithm looks like a rendering preference and is not one here:
/// Myers and Histogram genuinely disagree about *how many* lines changed, and
/// that count is what a reviewer reads on the pull request page and what the
/// API answers. Two instances of ForgeKeep must not report different numbers
/// for the same pull request, and neither must say so.
///
/// Two placements reach the algorithm, and a different mechanism denies each,
/// so each gets its own test:
///
/// * the host's `/etc/gitconfig`, `~/.gitconfig` and `GIT_*` are denied by
///   opening through `rg_git::repository::open`;
/// * `diff.algorithm` written into the repository's own `.git/config` — which
///   is loaded at every permission level, so an isolated open cannot filter it
///   out — is denied by [`super::forgekeep_diff_algorithm`] stating the answer.
///
/// Each test counts its fixture twice: once the way ForgeKeep counts now,
/// through [`super::gix_diff_numstat`] itself, and once the way it counted
/// before the fix ([`numstat_before_the_fix`], which asks the opened repository
/// for its algorithm the way `diff_resource_cache` used to be left to). That
/// second half is what proves the planted configuration genuinely reaches a
/// line count — without it, "the numbers did not move" would be just as true
/// of a probe that missed its target.
#[cfg(test)]
mod diff_configuration_ownership_tests {
    use super::merge_configuration_ownership_tests::{git, init_fixture, plant_repository_config};
    use std::path::{Path, PathBuf};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// Marks the child process spawned by
    /// [`the_numstat_ignores_the_hosts_git_configuration`]; also its only input.
    const HOSTILE_HOST_CONFIG_CHILD: &str = "FORGEKEEP_TEST_HOSTILE_DIFF_CONFIG";

    /// The knob the card names, in whichever file it is planted. `histogram` is
    /// a real answer an operator might prefer for their own reading — it is
    /// git's own recommendation for readable hunks — which is exactly why it
    /// must not follow them into other people's pull requests.
    const HOSTILE_DIFF_CONFIG: &str = "[diff]\n\talgorithm = histogram\n";

    /// The driver name the host-configuration test binds `counts.txt` to
    /// through a global attributes file.
    const HOSTILE_DRIVER: &str = "forgekeep-host-probe";

    /// What the repository's own configuration decides about the *patch* half if
    /// nobody states otherwise: the algorithm its hunks are computed with, and
    /// whether the `a/` / `b/` pair the reader parses is printed at all.
    const HOSTILE_PATCH_CONFIG: &str = "[diff]\n\talgorithm = histogram\n\tnoprefix = true\n";

    /// The two branches every test here reads between.
    const BASE_REV: &str = "refs/heads/main";
    const HEAD_REV: &str = "refs/heads/feature";

    /// What the fixture below counts as under Myers, i.e. what ForgeKeep must
    /// answer whatever the host or the repository prefers: two lines added, two
    /// removed.
    const MYERS_NUMSTAT: (i64, i64) = (2, 2);

    /// And under Histogram, which is not a rounding difference: twice as many
    /// lines on both sides of the same six-line file.
    const HISTOGRAM_NUMSTAT: (i64, i64) = (4, 4);

    /// And what the host's `diff.<name>.binary` driver turns the same change
    /// into: a pull request that changed nothing. `gix` answers `None` for a
    /// blob a driver declared binary, and a zero numstat is the honest report
    /// for that — which is what makes this the quietest way for a host to
    /// rewrite what a reviewer sees.
    const SILENCED_NUMSTAT: (i64, i64) = (0, 0);

    /// A repository whose single changed file makes the two algorithms
    /// disagree.
    ///
    /// Six lines drawn from three repeated tokens. Myers minimises the edit
    /// script and finds the two-line change; Histogram optimises for
    /// human-readable hunks instead and reports four lines on each side.
    /// Measured on `gix-imara-diff 0.2.2`, and reproduced by `git 2.43.0`'s own
    /// `git diff --numstat --diff-algorithm=…`, which agrees with both.
    fn algorithm_sensitive_fixture(root: &Path) -> PathBuf {
        algorithm_sensitive_fixture_with_attributes(root, None)
    }

    fn algorithm_sensitive_fixture_with_attributes(
        root: &Path,
        attributes: Option<&str>,
    ) -> PathBuf {
        let worktree = init_fixture(root);
        let file = worktree.join("counts.txt");

        std::fs::write(&file, "alpha\nalpha\nbeta\nalpha\nbeta\ngamma\n").expect("base blob");
        if let Some(attributes) = attributes {
            std::fs::write(worktree.join(".gitattributes"), attributes)
                .expect("repository attributes");
        }
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-q", "-m", "base"]);

        git(&worktree, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(&file, "alpha\ngamma\nalpha\nalpha\ngamma\nbeta\n").expect("head blob");
        git(&worktree, &["commit", "-q", "-am", "rearrange the lines"]);
        git(&worktree, &["checkout", "-q", "main"]);

        worktree
    }

    /// The count as ForgeKeep produces it: the production function, opening the
    /// repository and stating its algorithm for itself.
    fn numstat_forgekeeps_way(worktree: &Path) -> (i64, i64) {
        let (_files, stats) = super::gix_diff_numstat(
            worktree,
            "refs/heads/main".to_string(),
            "refs/heads/feature".to_string(),
        )
        .expect("ForgeKeep counts this fixture");
        (stats.total_additions, stats.total_deletions)
    }

    /// The count as ForgeKeep produced it before `card_25afc5bcc044`: the same
    /// walk, with the blob platform left holding the algorithm
    /// `diff_resource_cache` read out of the repository's configuration.
    ///
    /// Which configuration that is depends on how the caller opened the
    /// repository, and that is the point — the two tests below hand this the
    /// same function opened two different ways.
    fn numstat_before_the_fix(repo: &gix::Repository) -> anyhow::Result<(i64, i64)> {
        let old_tree = repo
            .rev_parse_single("refs/heads/main")?
            .object()?
            .peel_to_tree()?;
        let new_tree = repo
            .rev_parse_single("refs/heads/feature")?
            .object()?
            .peel_to_tree()?;

        let mut resource_cache = repo.diff_resource_cache(
            gix::diff::blob::pipeline::Mode::ToGit,
            gix::diff::blob::pipeline::WorktreeRoots::default(),
        )?;
        let mut platform = old_tree.changes()?;
        platform.options(|options| {
            options.track_rewrites(None);
        });

        let mut counts = (0i64, 0i64);
        {
            let sink = &mut counts;
            platform
                .for_each_to_obtain_tree(
                    &new_tree,
                    |change| -> Result<std::ops::ControlFlow<()>, anyhow::Error> {
                        // The fixture keeps its one file at the repository root,
                        // so no directory entry ever reaches this closure.
                        if let Some(stats) = change.diff(&mut resource_cache)?.line_counts()? {
                            sink.0 += stats.insertions as i64;
                            sink.1 += stats.removals as i64;
                        }
                        Ok(std::ops::ControlFlow::Continue(()))
                    },
                )
                .map_err(anyhow::Error::from)?;
        }
        Ok(counts)
    }

    /// The patch as ForgeKeep reads it now, split back into the per-file
    /// entries `attach_patches` keys by path and counted line by line — the same
    /// two readers the API answer is built from, so what this measures is what a
    /// reviewer is shown above and below the numbers.
    fn patch_numstat(patch: &str) -> (i64, i64) {
        let entries = super::split_unified_diff(patch);
        let entry = entries.get("counts.txt").unwrap_or_else(|| {
            panic!("the patch names no `counts.txt` entry; it names {:?}", {
                let mut named: Vec<&String> = entries.keys().collect();
                named.sort();
                named
            })
        });
        let lines = super::parse_diff_lines(entry);
        let counted = |kind: &str| lines.iter().filter(|line| line.kind == kind).count() as i64;
        (counted("addition"), counted("deletion"))
    }

    /// Run one patch argv straight off the gateway, with nothing else stated.
    ///
    /// The gateway disarms the host's environment whatever it is handed, which
    /// is why every probe here plants its configuration in the repository
    /// instead: that is the placement neither half of the disarming reaches.
    fn patch_off_the_bare_gateway(worktree: &Path, argv: &[&str]) -> String {
        let git = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway must initialize");
        let output = git.run(argv, Some(worktree)).expect("git diff must run");
        assert!(
            output.success(),
            "git {argv:?} could not read the fixture: {}",
            output.stderr_str()
        );
        output.stdout_str()
    }

    /// The patch as ForgeKeep read it before the fix: the argv as it was, run
    /// straight off the gateway.
    fn patch_the_old_way(worktree: &Path) -> String {
        patch_off_the_bare_gateway(
            worktree,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--no-ext-diff",
                "--no-renames",
                BASE_REV,
                HEAD_REV,
            ],
        )
    }

    /// The patch as [`super::forgekeep_patch_argv`] states it and *nothing else*
    /// does: the same argv, run without the invocation policy wrapped around it
    /// in production.
    ///
    /// A fixture where the second layer is absent by construction. Production
    /// states its diff algorithm twice — once in the argv, once through
    /// `rg_git::invocation::local`'s owned settings — and two layers of one fix
    /// cover for each other: dropping either one on its own leaves the answer
    /// right, so a probe that only drives the production reader reports a
    /// working fix about a half that no longer works.
    fn patch_from_the_argv_alone(worktree: &Path) -> String {
        patch_off_the_bare_gateway(worktree, &super::forgekeep_patch_argv(BASE_REV, HEAD_REV))
    }

    /// `card_4a21d30afb9a` — the numbers and the patch under them are two halves
    /// of ONE answer, and they must be computed by the same algorithm.
    ///
    /// The numstat half states its own since `card_25afc5bcc044`; the patch half
    /// is read from the `git` binary, and `git diff` asks `diff.algorithm` too.
    /// On the fixture where Myers and Histogram disagree, that made a pull
    /// request report "+2 −2" above a patch holding four added lines — one
    /// answer contradicting itself, which is worse than the two halves having
    /// been wrong together.
    ///
    /// Both halves of the pair are asserted: the planted configuration has to
    /// reach the old reader, or "the numbers did not move" would be just as true
    /// of a probe that missed. The `a/` / `b/` pair is checked alongside the
    /// counts because `diff.noprefix` is planted with the algorithm: it is what
    /// [`super::split_unified_diff`] parses, and a top-level file named `b`
    /// would lose its patch entirely without it.
    #[test]
    fn the_patch_half_is_counted_the_way_the_numstat_half_is() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = algorithm_sensitive_fixture(dir.path());
        plant_repository_config(&worktree, HOSTILE_PATCH_CONFIG);

        let control = patch_the_old_way(&worktree);
        assert_eq!(
            patch_numstat(&control),
            HISTOGRAM_NUMSTAT,
            "the planted repository configuration never reached the patch, so this test \
             would stay green with the bug in place"
        );
        assert!(
            !control.contains("diff --git a/counts.txt b/counts.txt"),
            "the planted `diff.noprefix` never reached the patch header, so the prefix \
             half of this test proves nothing:\n{control}"
        );

        let stated = patch_from_the_argv_alone(&worktree);
        assert_eq!(
            patch_numstat(&stated),
            MYERS_NUMSTAT,
            "the patch argv does not state its diff algorithm on its own — it is riding on \
             `rg_git::invocation::local`'s copy, and the two halves of this answer are one \
             edit in another crate away from disagreeing again:\n{stated}"
        );
        assert!(
            stated.contains("diff --git a/counts.txt b/counts.txt"),
            "the patch argv does not state its path prefixes on its own:\n{stated}"
        );

        let patch = super::forgekeep_patch_text(&worktree, BASE_REV, HEAD_REV)
            .expect("ForgeKeep reads this fixture");
        assert_eq!(
            patch_numstat(&patch),
            MYERS_NUMSTAT,
            "`diff.algorithm` written into the repository configuration changed the patch a \
             pull request publishes:\n{patch}"
        );
        assert_eq!(
            patch_numstat(&patch),
            numstat_forgekeeps_way(&worktree),
            "the two halves of one pull-request diff disagree: the numbers say one thing and \
             the patch printed under them says another:\n{patch}"
        );
        assert!(
            patch.contains("diff --git a/counts.txt b/counts.txt"),
            "the patch header lost its `a/` / `b/` pair, which is what the reader that keys \
             a patch to its file parses:\n{patch}"
        );
    }

    /// The other half of the patch reader's policy, and the one the stated argv
    /// cannot carry: the settings [`rg_git::invocation::local`] owns.
    ///
    /// `core.attributesFile` names an attributes file outside the repository's
    /// content, and the repository's own `.git/config` can point it anywhere. An
    /// attributes line declaring the changed file `-diff` turns the whole patch
    /// into `Binary files … differ`: a pull request that changed four lines,
    /// published as a patch showing nothing. `invocation::local` denies it by
    /// stating `core.attributesFile=/dev/null`, and the in-tree
    /// `.gitattributes` — which is the user's own content, and which both halves
    /// read — is left alone.
    ///
    /// Planted in the repository rather than the environment on purpose — the
    /// gateway disarms the environment for every child it spawns, so the
    /// control half of this pair would not bite there.
    #[test]
    fn the_patch_half_is_read_under_forgekeeps_own_git_configuration() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = algorithm_sensitive_fixture(dir.path());
        let attributes = dir.path().join("repository-gitattributes");
        std::fs::write(&attributes, "counts.txt -diff\n").expect("attributes file");
        plant_repository_config(
            &worktree,
            &format!("[core]\n\tattributesFile = {}\n", attributes.display()),
        );

        let control = patch_the_old_way(&worktree);
        assert!(
            control.contains("Binary files"),
            "the attributes file the repository configuration names never reached the patch, \
             so this test would stay green with the bug in place:\n{control}"
        );

        let patch = super::forgekeep_patch_text(&worktree, BASE_REV, HEAD_REV)
            .expect("ForgeKeep reads this fixture");
        assert!(
            !patch.contains("Binary files"),
            "a `core.attributesFile` written into the repository configuration silenced the \
             patch a pull request publishes:\n{patch}"
        );
        assert_eq!(
            patch_numstat(&patch),
            MYERS_NUMSTAT,
            "the patch ForgeKeep publishes stopped counting the lines the change really \
             has:\n{patch}"
        );

        assert_eq!(
            numstat_forgekeeps_way(&worktree),
            MYERS_NUMSTAT,
            "a `core.attributesFile` named by repository configuration silenced the numstat \
             half of a pull request"
        );
    }

    #[test]
    fn in_tree_attributes_remain_part_of_the_numstat_contract() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree =
            algorithm_sensitive_fixture_with_attributes(dir.path(), Some("counts.txt -diff\n"));

        assert_eq!(
            numstat_forgekeeps_way(&worktree),
            SILENCED_NUMSTAT,
            "the owned resource cache stopped reading committed `.gitattributes`"
        );
    }

    #[test]
    fn repository_diff_drivers_cannot_silence_numstat() {
        const REPOSITORY_DRIVER: &str = "forgekeep-repository-probe";

        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = algorithm_sensitive_fixture_with_attributes(
            dir.path(),
            Some("counts.txt diff=forgekeep-repository-probe\n"),
        );
        plant_repository_config(
            &worktree,
            &format!("[diff \"{REPOSITORY_DRIVER}\"]\n\tbinary = true\n"),
        );

        let repo = rg_git::repository::open(&worktree).expect("open the fixture");
        assert_eq!(
            numstat_before_the_fix(&repo).expect("the old way reads the driver"),
            SILENCED_NUMSTAT,
            "the repository driver never reached the old resource cache, so this test has no \
             teeth"
        );
        assert_eq!(
            numstat_forgekeeps_way(&worktree),
            MYERS_NUMSTAT,
            "a `diff.<name>.binary` driver from repository configuration silenced numstat"
        );
    }

    #[test]
    fn repository_big_file_threshold_cannot_silence_numstat() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = algorithm_sensitive_fixture(dir.path());
        plant_repository_config(&worktree, "[core]\n\tbigFileThreshold = 1\n");

        let repo = rg_git::repository::open(&worktree).expect("open the fixture");
        assert_eq!(
            numstat_before_the_fix(&repo).expect("the old way reads the threshold"),
            SILENCED_NUMSTAT,
            "the repository threshold never reached the old resource cache, so this test has \
             no teeth"
        );
        assert_eq!(
            numstat_forgekeeps_way(&worktree),
            MYERS_NUMSTAT,
            "a `core.bigFileThreshold` from repository configuration silenced numstat"
        );
    }

    /// Configuration inside the repository ForgeKeep opened is the placement an
    /// isolated open does *not* cover — repository-local config is loaded at
    /// every permission level — so here the stated algorithm, and only the
    /// stated algorithm, has to hold.
    #[test]
    fn the_numstat_algorithm_comes_from_forgekeep_not_from_the_repository_configuration() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = algorithm_sensitive_fixture(dir.path());
        plant_repository_config(&worktree, HOSTILE_DIFF_CONFIG);

        let repo = rg_git::repository::open(&worktree).expect("open the fixture");
        assert_eq!(
            numstat_before_the_fix(&repo).expect("the old way still counts"),
            HISTOGRAM_NUMSTAT,
            "the planted `diff.algorithm = histogram` never reached the line count, so this \
             test would stay green with the bug in place"
        );
        assert_eq!(
            numstat_forgekeeps_way(&worktree),
            MYERS_NUMSTAT,
            "diff.algorithm written into the repository configuration changed the numbers a \
             pull request reports"
        );
    }

    /// The half the isolated open is responsible for, and it still has its own
    /// teeth after the pin: a `diff` driver reaches *past*
    /// [`super::forgekeep_diff_algorithm`], because `gix` lets a driver named by
    /// an attribute override the platform's algorithm — and `diff.<name>.binary`
    /// overrides the line count altogether. The host can name one through
    /// `core.attributesFile`, which is a global attributes file and therefore a
    /// source only an isolated open drops. Planted here, it turns a six-line
    /// file that gained and lost two lines into a pull request that reports no
    /// changed lines at all.
    ///
    /// The plain `diff.algorithm` the card names is planted alongside it, so
    /// this test answers the card's own question — a host preferring `histogram`
    /// must not move the numbers — in the placement the card names.
    ///
    /// `/etc/gitconfig` and `~/.gitconfig` reach the process only through
    /// environment variables, and a test may not mutate those in place —
    /// `rust_sources_do_not_mutate_process_environment` forbids it, and a shared
    /// thread pool is why. So the count runs in a child process that inherits
    /// the planted variables honestly.
    #[test]
    fn the_numstat_ignores_the_hosts_git_configuration() {
        let dir = tempfile::tempdir().expect("host config directory");
        let attributes = dir.path().join("host-gitattributes");
        std::fs::write(&attributes, format!("counts.txt diff={HOSTILE_DRIVER}\n"))
            .expect("host attributes file");

        let hostile = format!(
            "{HOSTILE_DIFF_CONFIG}[core]\n\tattributesFile = {}\n[diff \"{HOSTILE_DRIVER}\"]\n\tbinary = true\n",
            attributes.display()
        );
        let system = dir.path().join("system-gitconfig");
        let global = dir.path().join("global-gitconfig");
        std::fs::write(&system, &hostile).expect("system config");
        std::fs::write(&global, &hostile).expect("global config");

        let executable = std::env::current_exe().expect("current test executable");
        let output = std::process::Command::new(executable)
            .env(HOSTILE_HOST_CONFIG_CHILD, "1")
            // `GIT_CONFIG_SYSTEM` stands in for `/etc/gitconfig`, which a test
            // cannot write; `GIT_CONFIG_NOSYSTEM=0` keeps that level switched on.
            .env("GIT_CONFIG_SYSTEM", &system)
            .env("GIT_CONFIG_NOSYSTEM", "0")
            .env("GIT_CONFIG_GLOBAL", &global)
            .args([
                "--exact",
                "pull_request::service::diff_configuration_ownership_tests::\
                 numstat_under_a_hostile_host_config_child",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .expect("spawn the host-config child");

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "host-config child failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );

        let reported = |key: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or_else(|| {
                    panic!("child printed no `{key}` line:\nstdout:\n{stdout}\nstderr:\n{stderr}")
                })
                .trim()
                .to_owned()
        };
        let rendered = |counts: (i64, i64)| format!("{},{}", counts.0, counts.1);

        assert_eq!(
            reported("old-way="),
            rendered(SILENCED_NUMSTAT),
            "the planted host configuration never reached the line count, so this test would \
             stay green with the bug in place:\nstdout:\n{stdout}"
        );
        assert_eq!(
            reported("forgekeep="),
            rendered(MYERS_NUMSTAT),
            "the host's git configuration changed the numbers a pull request reports:\nstdout:\n\
             {stdout}"
        );
    }

    /// Driven only by [`the_numstat_ignores_the_hosts_git_configuration`], which
    /// is what supplies the planted environment. The early return keeps
    /// `--run-ignored all` honest instead of failing on a bare invocation.
    #[test]
    #[ignore = "spawned by the_numstat_ignores_the_hosts_git_configuration"]
    fn numstat_under_a_hostile_host_config_child() {
        if std::env::var_os(HOSTILE_HOST_CONFIG_CHILD).is_none() {
            return;
        }

        let dir = tempfile::tempdir().expect("child fixture directory");
        let worktree = algorithm_sensitive_fixture(dir.path());

        // A bare `gix::open` is what reaches the planted host configuration.
        match gix::open(&worktree)
            .map_err(anyhow::Error::from)
            .and_then(|repo| numstat_before_the_fix(&repo))
        {
            Ok((additions, deletions)) => println!("old-way={additions},{deletions}"),
            Err(error) => println!("old-way=failed: {error}"),
        }

        let (additions, deletions) = numstat_forgekeeps_way(&worktree);
        println!("forgekeep={additions},{deletions}");
    }

    /// The behavioural tests drive the production function and redden for each
    /// imported configuration source, so this census is not what carries them.
    /// What it adds is naming the ownership chain: the numstat function opens
    /// through the isolated wrapper and constructs exactly one owned resource
    /// cache, while the helper — not the caller — states the algorithm.
    ///
    /// Read from the production view, so the `gix::open` that
    /// [`numstat_under_a_hostile_host_config_child`] deliberately keeps alive a
    /// few lines up cannot satisfy the census. Each half asserts a presence as
    /// well as an absence: a census that has stopped finding the function at all
    /// would otherwise report "no bare open here" about a function it never
    /// read.
    #[test]
    fn the_pull_request_numstat_opens_and_builds_one_owned_resource_cache() {
        let source = include_str!("service.rs");

        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "gix_diff_numstat",
                &["rg_git::repository::open"]
            )
            .len(),
            1,
            "`gix_diff_numstat` no longer opens the repository through \
             `rg_git::repository::open` — either it was reverted, or this census has stopped \
             reading the function"
        );
        assert!(
            rust_source::production_function_call_sites(source, "gix_diff_numstat", &["gix::open"])
                .is_empty(),
            "`gix_diff_numstat` opens the repository with a bare `gix::open`, which reads the \
             host's /etc/gitconfig, ~/.gitconfig and GIT_* — see card_25afc5bcc044"
        );
        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "gix_diff_numstat",
                &["forgekeep_diff_resource_cache"]
            )
            .len(),
            1,
            "`gix_diff_numstat` no longer builds exactly one resource cache from ForgeKeep's \
             own policy"
        );
        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_diff_resource_cache",
                &["forgekeep_diff_algorithm"]
            )
            .len(),
            1,
            "the owned resource cache no longer states the algorithm its line counts are \
             computed with — see card_25afc5bcc044"
        );
        assert!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_diff_resource_cache",
                &["diff_resource_cache"]
            )
            .is_empty(),
            "the owned resource-cache helper delegates back to Repository::diff_resource_cache, \
             which imports repository configuration — see card_e9cd8e932a91"
        );
    }
}

// ── Merge ───────────────────────────────────────────────────────────────

/// Merge strategy for a PR.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}

impl MergeStrategy {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "merge" => Ok(Self::Merge),
            "squash" => Ok(Self::Squash),
            "rebase" => Ok(Self::Rebase),
            _ => bail!("invalid merge strategy, use: merge, squash, rebase"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct AutoMergeOutcome {
    /// disabled / pending / merged
    pub status: String,
    pub reason: Option<String>,
    pub merge: Option<MergeResult>,
}

pub async fn enable_auto_merge(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    number: i64,
    strategy: MergeStrategy,
    actor_id: i64,
) -> Result<PullRequest> {
    let pr = get_pr(db, owner, repo_name, number).await?;
    let updated = rg_db::contention::retry_transaction(
        "handoff PR from merge queue to auto-merge",
        || async {
            let transaction = db
                .begin()
                .await
                .context("db: begin merge-queue to auto-merge handoff")?;
            let sqlite = transaction.get_database_backend() == DatabaseBackend::Sqlite;

            // On SQLite this conditional UPDATE is intentionally first: even a
            // zero-row cancellation takes the writer slot before we inspect the
            // PR. PostgreSQL/MySQL instead lock the PR row first, which orders
            // this handoff against the enqueue direction without a global lock.
            let canceled_on_sqlite = if sqlite {
                rg_db::ops::merge_queue_ops::cancel_in_transaction(&transaction, pr.id).await?
            } else {
                None
            };
            let current = pull_request_ops::lock_by_id_for_update(&transaction, pr.id)
                .await?
                .ok_or_else(|| crate::error::not_found("pull request"))?;

            if current.state != "open" {
                return Err(crate::error::conflict(
                    "auto-merge can only be enabled for an open pull request",
                ));
            }
            if current.is_draft {
                return Err(crate::error::conflict(
                    "auto-merge cannot be enabled for a draft pull request",
                ));
            }

            let canceled = if sqlite {
                canceled_on_sqlite
            } else {
                rg_db::ops::merge_queue_ops::cancel_in_transaction(&transaction, current.id).await?
            };
            if canceled.is_none() {
                if let Some(entry) =
                    rg_db::ops::merge_queue_ops::find_by_pr_in_transaction(&transaction, current.id)
                        .await?
                {
                    if entry.status == "running" {
                        return Err(crate::error::conflict(
                            "cannot enable auto-merge while the merge queue is processing this PR",
                        ));
                    }
                    if entry.status == "queued" {
                        anyhow::bail!(
                            "db: queued merge-queue entry {} resisted ownership handoff for PR {}",
                            entry.id,
                            current.id
                        );
                    }
                }
            }

            let mut active: pull_request::ActiveModel = current.into();
            active.auto_merge_enabled = Set(true);
            active.auto_merge_strategy = Set(Some(strategy.as_str().to_string()));
            active.auto_merge_enabled_by_id = Set(Some(actor_id));
            active.auto_merge_enabled_at = Set(Some(Utc::now()));
            active.updated_at = Set(Utc::now());
            let updated = pull_request_ops::update_in_transaction(&transaction, active).await?;

            transaction
                .commit()
                .await
                .context("db: commit merge-queue to auto-merge handoff")?;
            Ok(updated)
        },
    )
    .await?;
    if let Err(error) = rg_db::ops::pr_event_ops::record(
        db,
        updated.repo_id,
        updated.id,
        Some(actor_id),
        "auto_merge_enabled",
        None,
        serde_json::json!({"strategy": strategy.as_str()}),
    )
    .await
    {
        tracing::error!(
            repo_id = updated.repo_id,
            pr_id = updated.id,
            actor_id,
            event_type = "auto_merge_enabled",
            error = %format!("{error:#}"),
            "auto-merge was enabled, but its timeline event could not be recorded"
        );
    }
    Ok(updated)
}

pub async fn disable_auto_merge(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    number: i64,
    actor_id: i64,
) -> Result<PullRequest> {
    let pr = get_pr(db, owner, repo_name, number).await?;
    let was_enabled = pr.auto_merge_enabled;
    let mut active: pull_request::ActiveModel = pr.into();
    active.auto_merge_enabled = Set(false);
    active.auto_merge_strategy = Set(None);
    active.auto_merge_enabled_by_id = Set(None);
    active.auto_merge_enabled_at = Set(None);
    active.updated_at = Set(Utc::now());
    let updated = pull_request_ops::update(db, active).await?;
    if was_enabled {
        if let Err(error) = rg_db::ops::pr_event_ops::record(
            db,
            updated.repo_id,
            updated.id,
            Some(actor_id),
            "auto_merge_disabled",
            None,
            serde_json::json!({}),
        )
        .await
        {
            tracing::error!(
                repo_id = updated.repo_id,
                pr_id = updated.id,
                actor_id,
                event_type = "auto_merge_disabled",
                error = %format!("{error:#}"),
                "auto-merge was disabled, but its timeline event could not be recorded"
            );
        }
    }
    Ok(updated)
}

/// Attempt an enabled auto-merge. A protection rule that refused is returned as
/// a pending outcome, while a rule that could not be *checked* — and every other
/// Git/DB failure — remains an error: "not yet" and "unknown" are different
/// answers, and only the first is something the caller can wait out.
pub async fn try_auto_merge(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    owner: &str,
    repo_name: &str,
    number: i64,
) -> Result<AutoMergeOutcome> {
    let pr = get_pr(db, owner, repo_name, number).await?;
    if !pr.auto_merge_enabled {
        return Ok(AutoMergeOutcome {
            status: "disabled".into(),
            reason: None,
            merge: None,
        });
    }
    if pr.state != "open" || pr.is_draft {
        return Ok(AutoMergeOutcome {
            status: "pending".into(),
            reason: Some("pull request is not open and ready for review".into()),
            merge: None,
        });
    }
    if let Err(error) = crate::branch_protection::service::check_merge_allowed(
        db,
        pr.repo_id,
        &pr.base_branch,
        pr.id,
    )
    .await
    {
        // A rule that refused is a condition the caller can wait out, and its
        // message was written for them (card_a997f30c142c). A rule the server
        // could not read refused nothing: `pending` would tell the caller — and
        // `PUT .../auto-merge`, which serialises this outcome into its `200` —
        // that the merge is waiting on a condition nobody evaluated, with the
        // failed read's chain as the explanation (card_af2abe7904bd, H-05).
        let Some(reason) = crate::error::client_facing_message(&error) else {
            return Err(error.context(format!(
                "auto-merge: branch protection of '{}' could not be checked",
                pr.base_branch
            )));
        };
        return Ok(AutoMergeOutcome {
            status: "pending".into(),
            reason: Some(reason),
            merge: None,
        });
    }

    let strategy = MergeStrategy::parse(
        pr.auto_merge_strategy
            .as_deref()
            .context("auto-merge strategy is missing")?,
    )?;
    let actor_id = pr.auto_merge_enabled_by_id.ok_or_else(|| {
        crate::error::forbidden("auto-merge has no authorizing actor with write access")
    })?;
    if !pull_request_ops::claim_auto_merge(db, pr.id).await? {
        return Ok(AutoMergeOutcome {
            status: "pending".into(),
            reason: Some("another automatic merge attempt is already running".into()),
            merge: None,
        });
    }
    // No tracker to hand down: auto-merge runs from the post-push hooks and the
    // CI-completion paths, which are already detached, so the merge announcement
    // takes the process-global delivery tracker.
    // The head is pinned to `pr.head_sha`: this path is woken by a green
    // pipeline for one specific commit (`try_auto_merges_for_head_commit`
    // selects pull requests *by* it), and the protection rules just checked
    // above counted their approvals and status checks for that same commit. The
    // branch may already point somewhere else — `pr.head_sha` is moved by the
    // detached post-push hook, so the row lags the ref by design — and merging
    // that tip would merge a commit nothing above ever looked at
    // (card_9ff26bb95dc9).
    let merge = match merge_pr(
        db,
        repo_root,
        owner,
        repo_name,
        number,
        actor_id,
        strategy,
        pr.head_sha.as_deref(),
        None,
    )
    .await
    {
        Ok(merge) => merge,
        Err(error) => {
            if let Err(restore_error) = pull_request_ops::restore_auto_merge(db, pr.id).await {
                tracing::error!(pr_id = pr.id, %restore_error, "failed to restore auto-merge after merge error");
            }
            return Err(error);
        }
    };
    Ok(AutoMergeOutcome {
        status: "merged".into(),
        reason: None,
        merge: Some(merge),
    })
}

/// Attempt every enabled PR whose source now points at this commit. Used by
/// push and CI-completion hooks for same-repository and fork pull requests.
pub async fn try_auto_merges_for_head_commit(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    source_repo_id: i64,
    commit_sha: &str,
) -> Result<Vec<AutoMergeOutcome>> {
    let prs =
        pull_request_ops::list_auto_merge_for_head_commit(db, source_repo_id, commit_sha).await?;
    let mut outcomes = Vec::with_capacity(prs.len());
    for pr in prs {
        // Each pull request is attempted on its own. A `?` here would abandon
        // the outcomes already collected, and those carry the base-branch moves
        // of merges that have *happened* — the caller runs their post-push
        // hooks from exactly this vector, so throwing it away leaves a merge
        // commit on the branch that no pipeline, webhook or watcher ever hears
        // about (card_94dbd5fd4bce). One unreadable row costs its own pull
        // request an attempt, nothing more.
        let repository = match repo_entity::Entity::find_by_id(pr.repo_id).one(db).await {
            Ok(Some(repository)) => repository,
            Ok(None) => {
                tracing::warn!(
                    pr_id = pr.id,
                    repo_id = pr.repo_id,
                    "automatic merge attempt skipped: target repository not found"
                );
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    pr_id = pr.id,
                    repo_id = pr.repo_id,
                    error = %format!("{error:#}"),
                    "automatic merge attempt skipped: target repository could not be read"
                );
                continue;
            }
        };
        let namespace = match repository_namespace(db, &repository).await {
            Ok(namespace) => namespace,
            Err(error) => {
                tracing::warn!(
                    pr_id = pr.id,
                    repo_id = pr.repo_id,
                    error = %format!("{error:#}"),
                    "automatic merge attempt skipped: repository namespace could not be resolved"
                );
                continue;
            }
        };
        match try_auto_merge(db, repo_root, &namespace, &repository.name, pr.number).await {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => tracing::warn!(
                pr_id = pr.id,
                error = %format!("{error:#}"),
                "automatic merge attempt failed"
            ),
        }
    }
    Ok(outcomes)
}

/// A base-branch move a merge made, named together with the repository whose
/// branch actually moved.
///
/// The repository is not decoration. Merges are evaluated *by head commit*
/// ([`try_auto_merges_for_head_commit`],
/// [`super::merge_queue::process_for_head_commit_with_ci`]),
/// and a fork PR's head lives in one repository while its base lives in
/// another — so the caller that asked "what does this commit merge?" cannot
/// assume the answer moved a branch of the repository it named. Running the
/// hooks against the wrong one would post the merge to another repository's
/// webhooks and hunt for the merge commit in a git dir that never had it.
#[derive(Debug, Clone)]
pub struct MergedRef {
    /// The repository whose base branch moved (the PR's *base* repository).
    pub repo_id: i64,
    /// Namespace of that repository — the user or organization name, i.e. the
    /// `<owner>` of `<owner>/<repo>.git` under the repo root.
    pub owner: String,
    pub repo_name: String,
    pub update: RefUpdate,
}

/// Result of a merge operation.
#[derive(Debug, serde::Serialize)]
pub struct MergeResult {
    pub merge_commit_sha: String,
    pub strategy: String,
    /// How this merge moved `refs/heads/<base>`, for the caller's post-push
    /// hooks. `None` = nothing observably moved (see [`base_ref_update`]).
    ///
    /// A merge advances the base branch exactly like a `git push` does, so it
    /// owes the same automation: a CI pipeline on the merge commit, the `push`
    /// webhook, the watch fan-out. Until card_87c4912c51ed a merge fired only
    /// `pull_request.merged`, so "run CI on every push to main" silently did
    /// not hold for the way most merges happen — through the UI.
    ///
    /// `rg-core` cannot run the hooks itself: they need the process's CI engine
    /// and notification hub, which live in the transport layer. So the merge
    /// reports the ref move and every caller holding that wiring feeds it into
    /// [`crate::push_hooks::post_push_hooks`].
    ///
    /// `#[serde(skip)]`: `MergeResult` is a REST response body and this is
    /// internal plumbing, not part of the API contract.
    #[serde(skip)]
    pub base_ref_update: Option<MergedRef>,
}

/// The ref move a merge made to its base branch, or `None` when the hooks must
/// not run for it.
///
/// Both guards are correctness, not defensive noise: an empty/zero `after` reads
/// to [`crate::push_hooks::trigger_push_webhooks`] as a *deleted* branch and
/// would fire `branch.deleted` for a branch that is alive, and an empty/zero
/// `before` reads as a *created* one. `before` is only ever unknown when the
/// pre-merge read of the base tip failed, and inventing zeros there would turn a
/// missing pipeline into a wrong webhook.
fn base_ref_update(base_branch: &str, before: &str, after: &str) -> Option<RefUpdate> {
    const ZERO_SHA: &str = "0000000000000000000000000000000000000000";
    if before.is_empty() || before == ZERO_SHA || after.is_empty() || after == ZERO_SHA {
        return None;
    }
    if before == after {
        return None;
    }
    Some(RefUpdate {
        old_sha: before.to_string(),
        new_sha: after.to_string(),
        refname: format!("refs/heads/{base_branch}"),
        status: "ok".to_string(),
        message: String::new(),
    })
}

/// Merge a pull request using the specified strategy.
/// Supports cross-repository (fork) PRs by fetching the head branch first.
/// `actor_id` is revalidated here for account standing and current repository
/// write access; every caller, including delayed auto-merge and queue workers,
/// must also pass through the target branch's protection rules below.
///
/// `expected_head_sha` pins *which commit* may be merged. A caller that has
/// already verified something about the head — the merge queue, whose CI run is
/// about a group commit built from one specific `pr.head_sha`; auto-merge, woken
/// by a green pipeline for one commit — passes it, and a head that has moved
/// since then answers `Conflict` instead of merging whatever the branch points
/// at now.
///
/// `None` does not mean "unpinned". The branch-protection check below returns
/// the head it judged, and when a rule counted approvals or status checks for a
/// commit, that commit pins the merge on its own: permission was granted to it
/// and to no other. Only when nobody judged a head — an unprotected base branch
/// — is the current tip merged, which is what the person pressing "merge" asked
/// for and all a caller holding no verified commit can honestly ask for.
///
/// Gix merge operations (tree merge, commit creation) are offloaded to
/// `spawn_blocking` to avoid blocking the tokio async runtime.
#[allow(clippy::too_many_arguments)]
pub async fn merge_pr(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    owner: &str,
    repo_name: &str,
    number: i64,
    actor_id: i64,
    strategy: MergeStrategy,
    expected_head_sha: Option<&str>,
    delivery_tracker: Option<&crate::task_tracker::TaskTracker>,
) -> Result<MergeResult> {
    let mut pr = get_pr(db, owner, repo_name, number).await?;

    let actor = rg_db::ops::user_ops::find_by_id(db, actor_id).await?;
    if actor.is_none_or(|actor| !actor.is_usable()) {
        return Err(crate::error::forbidden(
            "pull request merge requires an active account",
        ));
    }
    if !crate::repo::service::can_write(db, owner, repo_name, Some(actor_id)).await? {
        return Err(crate::error::forbidden(
            "write access denied for pull request merge",
        ));
    }

    if pr.state == "merging"
        && pull_request_ops::recover_stale_merge_claim(
            db,
            pr.id,
            Utc::now() - chrono::Duration::minutes(30),
        )
        .await?
    {
        pr = get_pr(db, owner, repo_name, number).await?;
    }

    // These three are states, not bad requests: the caller asked for a merge
    // that is correct in form and may well succeed once the PR reopens, leaves
    // draft, or the other attempt finishes. `Conflict` carries that distinction
    // to the HTTP layer, which would otherwise have to guess it from the
    // message — and guessed "400" for the storage failures alongside them.
    if pr.state != "open" {
        return Err(crate::error::conflict(format!(
            "cannot merge a PR that is not in 'open' state (current: {})",
            pr.state
        )));
    }
    if pr.is_draft {
        return Err(crate::error::conflict(
            "draft pull requests cannot be merged",
        ));
    }

    let verdict = crate::branch_protection::service::check_merge_allowed(
        db,
        pr.repo_id,
        &pr.base_branch,
        pr.id,
    )
    .await?;
    let pinned_head =
        reconcile_pinned_head(&pr, expected_head_sha, verdict.judged_head_sha.as_deref())?;

    if !pull_request_ops::claim_merge(db, pr.id).await? {
        return Err(crate::error::conflict(
            "another merge attempt is already in progress",
        ));
    }

    let result = merge_claimed_pr(
        db,
        repo_root,
        owner,
        repo_name,
        pr.clone(),
        strategy,
        pinned_head.as_deref(),
        delivery_tracker,
    )
    .await;
    if result.is_err() {
        if let Err(error) = pull_request_ops::restore_merge_claim(db, pr.id).await {
            tracing::error!(pr_id = pr.id, error = %format!("{error:#}"), "failed to restore PR merge state");
        }
    } else {
        // Count the merge here (not in the HTTP handler) so the REST path,
        // auto-merge, and the merge queue all funnel through one recording site.
        crate::metrics_hook::record_pr_merged();
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn merge_claimed_pr(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    owner: &str,
    repo_name: &str,
    pr: PullRequest,
    strategy: MergeStrategy,
    expected_head_sha: Option<&str>,
    delivery_tracker: Option<&crate::task_tracker::TaskTracker>,
) -> Result<MergeResult> {
    let repo_path = repo_root.join(format!("{}/{}.git", owner, repo_name));
    if !repo_path.exists() {
        bail!("repository path does not exist: {:?}", repo_path);
    }
    require_pull_request_branch(&repo_path, "base", &pr.base_branch)?;

    // Read the base tip *before* the merge: afterwards the old commit is only
    // reachable through the reflog, and the post-push hooks need the `before`
    // half of the ref move to tell "branch advanced" from "branch created".
    // The required-ref check above turns a deleted base into a conflict; a
    // later failed read only costs the hooks for this merge, never the merge.
    let base_sha_before = match get_ref_sha(&repo_path, &pr.base_branch) {
        Ok(sha) => Some(sha),
        Err(error) => {
            tracing::warn!(
                pr_id = pr.id,
                base_branch = %pr.base_branch,
                error = %format!("{error:#}"),
                "could not read the base branch tip before merging — post-push hooks will be skipped for this merge"
            );
            None
        }
    };

    // For fork PRs, fetch head branch into target repo
    if let Some(head_repo_id) = pr.head_repo_id {
        let head_repo = repo_entity::Entity::find_by_id(head_repo_id)
            .one(db)
            .await?
            .context("head repository not found")?;
        let head_namespace = repository_namespace(db, &head_repo).await?;
        let head_repo_path = repo_root.join(format!("{}/{}.git", head_namespace, head_repo.name));

        // A deleted head is stale PR state, while an unreadable fork repository
        // is a server failure. Checking before fetch also prevents an old local
        // `refs/forks/...` ref from being reused after the source branch vanished.
        require_pull_request_branch(&head_repo_path, "head", &pr.head_branch)?;
        let fetch_ref = format!("refs/heads/{}", pr.head_branch);
        let local_ref = format!("refs/forks/{}/{}", head_namespace, pr.head_branch);

        let git = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let fetch_output = git.run(
            &[
                "fetch",
                &head_repo_path.to_string_lossy(),
                &format!("{}:{}", fetch_ref, local_ref),
            ],
            Some(&repo_path),
        )?;

        if !fetch_output.success() {
            bail!(
                "failed to fetch fork branch: {}",
                String::from_utf8_lossy(&fetch_output.stderr)
            );
        }

        // Merge and cleanup in spawn_blocking (CPU-intensive gix merge)
        let merge_ref = format!("refs/forks/{}/{}", head_namespace, pr.head_branch);
        // The fetch above brought whatever the fork's branch points at *now*.
        // That is the tip this merge is about, so it — not the PR row, not the
        // sha some earlier pass read — is what the caller's pin is checked
        // against, and what the merge is then performed on by object id.
        let head_sha = resolve_ref_sha(&repo_path, &merge_ref)?;
        require_pinned_head(&pr, expected_head_sha, &head_sha)?;
        let merge_commit_sha = {
            let repo_path = repo_path.clone();
            let pr = pr.clone();
            let merge_ref = merge_ref.clone();
            tokio::task::spawn_blocking(move || -> Result<String> {
                let sha = merge_head_rev(&repo_path, &pr, &head_sha, strategy)?;
                // Clean up fetched ref
                if let Err(e) = gix_delete_ref(&repo_path, &merge_ref) {
                    tracing::warn!("failed to clean up fork ref '{}': {}", merge_ref, e);
                }
                Ok(sha)
            })
            .await??
        };

        return update_pr_merged(
            db,
            owner,
            repo_name,
            pr,
            merge_commit_sha,
            strategy,
            base_sha_before,
            delivery_tracker,
        )
        .await;
    }

    require_pull_request_branch(&repo_path, "head", &pr.head_branch)?;
    let head_sha = get_ref_sha(&repo_path, &pr.head_branch)?;
    require_pinned_head(&pr, expected_head_sha, &head_sha)?;

    // Same-repo merge — offload gix merge operations to spawn_blocking
    let merge_commit_sha = {
        let repo_path = repo_path.clone();
        let pr = pr.clone();
        tokio::task::spawn_blocking(move || -> Result<String> {
            merge_head_rev(&repo_path, &pr, &head_sha, strategy)
        })
        .await??
    };

    update_pr_merged(
        db,
        owner,
        repo_name,
        pr,
        merge_commit_sha,
        strategy,
        base_sha_before,
        delivery_tracker,
    )
    .await
}

/// Merge one named revision of the head into the base branch.
///
/// `head_rev` is a resolved commit id, not a branch name, and that is the point:
/// the tip is read once in [`merge_claimed_pr`], checked against the caller's
/// pin there, and the merge below then operates on that exact object. Resolving
/// the branch again here would reopen the window the pin exists to close — a
/// push landing between the check and the merge would be merged unverified.
///
/// Uses gix merge APIs for Merge and Squash strategies; Rebase still uses git CLI.
fn merge_head_rev(
    repo_path: &std::path::Path,
    pr: &PullRequest,
    head_rev: &str,
    strategy: MergeStrategy,
) -> Result<String> {
    match strategy {
        MergeStrategy::Merge => {
            let merge_msg = format!("Merge pull request #{} from {}", pr.number, pr.head_branch);
            gix_merge_no_ff(repo_path, head_rev, &merge_msg)
        }
        MergeStrategy::Squash => {
            let squash_msg = format!(
                "Squash merge pull request #{} from {}",
                pr.number, pr.head_branch
            );
            gix_squash_merge(repo_path, head_rev, &squash_msg)
        }
        MergeStrategy::Rebase => git_rebase_merge(repo_path, &pr.base_branch, head_rev),
    }
}

/// Update PR state after successful merge.
///
/// `base_sha_before` is the base branch tip read before the merge — see
/// [`MergeResult::base_ref_update`], which is built from it.
#[allow(clippy::too_many_arguments)]
async fn update_pr_merged(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    mut pr: PullRequest,
    merge_commit_sha: String,
    strategy: MergeStrategy,
    base_sha_before: Option<String>,
    delivery_tracker: Option<&crate::task_tracker::TaskTracker>,
) -> Result<MergeResult> {
    pr.state = "merged".to_string();
    pr.merge_strategy = Some(format!("{:?}", strategy).to_lowercase());
    pr.merge_commit_sha = Some(merge_commit_sha.clone());
    pr.merged_at = Some(Utc::now());
    pr.closed_at = Some(Utc::now());
    pr.updated_at = Utc::now();
    pr.auto_merge_enabled = false;

    // The merged row as it ought to be, taken before the conversion consumes
    // `pr`: it is both the source of the `Set` values below and what the rest of
    // this function reads when the write that should have persisted it fails.
    let merged = pr.clone();
    let mut active: pull_request::ActiveModel = pr.into();
    active.state = Set(merged.state.clone());
    active.merge_strategy = Set(merged.merge_strategy.clone());
    active.merge_commit_sha = Set(merged.merge_commit_sha.clone());
    active.auto_merge_enabled = Set(false);
    active.merged_at = Set(merged.merged_at);
    active.closed_at = Set(merged.closed_at);
    active.updated_at = Set(merged.updated_at);

    // Everything from here on is bookkeeping *about* a merge that has already
    // happened: `merge_claimed_pr` wrote the merge commit and moved
    // `refs/heads/<base>` before calling this function, and no `?` can take that
    // back. So a failure below costs its own row and a log line — never the
    // `MergeResult`. That value carries `base_ref_update`, and every caller
    // feeds it to `crate::push_hooks::post_push_hooks`; losing it leaves a merge
    // commit on the branch that no pipeline, webhook or watcher ever hears
    // about, while the caller is told the merge did not happen and, over REST,
    // answers 5xx for a branch that did move (card_1cf8a3004b6d).
    let merged_pr = match pull_request_ops::update(db, active).await {
        Ok(merged_pr) => merged_pr,
        Err(error) => {
            tracing::error!(
                pr_id = merged.id,
                repo_id = merged.repo_id,
                merge_commit_sha = %merge_commit_sha,
                error = %format!("{error:#}"),
                "the merge commit is already on the base branch, but the pull request row could not be marked merged — it stays 'merging' until its claim lease expires"
            );
            merged
        }
    };
    if let Err(error) = rg_db::ops::pr_event_ops::record(
        db,
        merged_pr.repo_id,
        merged_pr.id,
        None,
        "pull_request_merged",
        None,
        serde_json::json!({
            "strategy": merged_pr.merge_strategy,
            "commit_sha": merge_commit_sha
        }),
    )
    .await
    {
        tracing::error!(
            pr_id = merged_pr.id,
            repo_id = merged_pr.repo_id,
            merge_commit_sha = %merge_commit_sha,
            error = %format!("{error:#}"),
            "failed to record the pull_request_merged timeline event for a merge that happened"
        );
    }

    // Trigger pull_request.merged webhook
    let merge_payload = serde_json::json!({
        "id": merged_pr.id,
        "repo_id": merged_pr.repo_id,
        "number": merged_pr.number,
        "title": merged_pr.title,
        "merge_commit_sha": merge_commit_sha,
        "strategy": format!("{:?}", strategy).to_lowercase(),
    });
    if let Err(e) =
        crate::webhook::service::trigger_pr_merged(db, merged_pr.repo_id, &merge_payload).await
    {
        tracing::warn!(error = %format!("{e:#}"), "failed to trigger PR merged webhook");
    }

    // No actor: this path serves the REST merge, auto-merge and the merge queue
    // alike, and the last two have no user behind them. `merge_pr` does not
    // carry the caller's id, so naming one here would mean guessing.
    announce_pr_to_watchers(
        db,
        delivery_tracker,
        merged_pr.repo_id,
        repo_name,
        None,
        merged_pr.number,
        &merged_pr.title,
        "merged",
    );

    // A merge does not have to wait for the PR's own pipeline — see
    // `cancel_pull_request_ci` for why `merged` cancels just like `closed`.
    super::ci::cancel_pull_request_ci(db, &merged_pr, "pull request merged").await;

    Ok(MergeResult {
        base_ref_update: base_sha_before
            .as_deref()
            .and_then(|before| base_ref_update(&merged_pr.base_branch, before, &merge_commit_sha))
            .map(|update| MergedRef {
                repo_id: merged_pr.repo_id,
                owner: owner.to_string(),
                repo_name: repo_name.to_string(),
                update,
            }),
        merge_commit_sha,
        strategy: format!("{:?}", strategy).to_lowercase(),
    })
}

/// Settle which commit this merge is allowed to be about.
///
/// Two independent parties can name one: the caller, which verified something
/// itself (the merge queue's CI ran on a group commit built from one head;
/// auto-merge was woken by a green pipeline for one commit), and branch
/// protection, whose approvals and status checks were counted for one head.
/// Either alone is enough to pin, and the merge is then performed on that
/// object rather than on whatever `refs/heads/<head>` names by the time it runs.
///
/// When both name a commit they must name the *same* one. A caller verifying
/// commit A while the protection rules were satisfied by commit B means neither
/// commit has both halves of the permission to merge, so there is nothing to
/// merge — a `Conflict`, like every other "the state moved under you" answer
/// here, rather than a silent choice between the two.
///
/// `None` from both is the honest unpinned case: nobody judged a commit — an
/// unprotected branch merged by a person pressing the button — and the branch
/// tip is exactly what they asked for.
fn reconcile_pinned_head(
    pr: &PullRequest,
    expected: Option<&str>,
    judged: Option<&str>,
) -> Result<Option<String>> {
    match (expected, judged) {
        (Some(expected), Some(judged)) if expected != judged => {
            // Neither sha reaches the client: which commits a repository holds
            // is not something an error string owes them (H-05), same as
            // `require_pinned_head`.
            tracing::warn!(
                pr_id = pr.id,
                head_branch = %pr.head_branch,
                verified_head_sha = %expected,
                judged_head_sha = %judged,
                "refusing to merge: the verified head and the head branch protection judged are different commits"
            );
            Err(crate::error::conflict(
                "the pull request head moved after it was verified; retry the merge",
            ))
        }
        (Some(expected), _) => Ok(Some(expected.to_string())),
        (None, judged) => Ok(judged.map(str::to_string)),
    }
}

/// Refuse a merge whose head is no longer the commit the caller verified.
///
/// The queue's guarantee is "CI was green on exactly this content, then it was
/// merged". Between the pass that built the merge group from `pr.head_sha` and
/// this merge, the author can push: the branch — and, for a fork PR, the fetch
/// that re-reads the fork's tip — then names a commit no pipeline ever saw. That
/// is a `Conflict`, the same state answer a moved base branch gets: nothing is
/// wrong with the request, the queue simply rebuilds its group on the new head
/// next pass and asks CI again.
///
/// The message stays free of both shas. It renders verbatim to whoever asked for
/// the merge, and which commits a repository holds is not theirs to learn from
/// an error string (H-05); the pair goes to the log instead.
fn require_pinned_head(pr: &PullRequest, expected: Option<&str>, actual: &str) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if expected == actual {
        return Ok(());
    }
    tracing::warn!(
        pr_id = pr.id,
        head_branch = %pr.head_branch,
        expected_head_sha = %expected,
        actual_head_sha = %actual,
        "refusing to merge: the pull request head moved after it was verified"
    );
    Err(crate::error::conflict(
        "the pull request head moved after it was verified; retry the merge",
    ))
}

/// Resolve a full ref name — `refs/forks/<ns>/<branch>` and friends — to its
/// commit id. [`get_ref_sha`] is the `refs/heads/` half of the same job.
fn resolve_ref_sha(repo_path: &std::path::Path, ref_name: &str) -> Result<String> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let mut reference = repo
        .try_find_reference(ref_name)
        .with_context(|| format!("failed to look up {ref_name} in repository: {repo_path:?}"))?
        .with_context(|| format!("{ref_name} does not exist in repository: {repo_path:?}"))?;
    let id = reference
        .peel_to_id()
        .with_context(|| format!("failed to resolve {ref_name} in repository: {repo_path:?}"))?;
    Ok(id.to_string())
}

/// Rebase a PR head in an isolated worktree and fast-forward the bare repository's base ref.
///
/// The final push is a normal fast-forward, so a concurrently advanced base branch is rejected
/// instead of overwritten. Everything before it is [`replay_rebase`], which the merge queue also
/// runs to build the artifact it puts under CI — the rehearsal and the merge are the same replay
/// or the queue's verdict is about a different operation (card_1a416b30dc15).
fn git_rebase_merge(
    repo_path: &std::path::Path,
    base_branch: &str,
    head_ref: &str,
) -> Result<String> {
    let canonical_repo = std::fs::canonicalize(repo_path)
        .with_context(|| format!("failed to canonicalize repository: {:?}", repo_path))?;
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let git = rg_git::invocation::local(gateway);
    let head_sha = resolve_commit(&git, repo_path, head_ref)
        .with_context(|| format!("failed to resolve rebase head: {head_ref}"))?;

    let worktree = RebaseWorktree::new(repo_path, crate::staging::WorktreePurpose::Rebase)?;
    let upstream = format!("origin/{base_branch}");
    // `upstream_sha` is the base the replay was built on, read before the rebase
    // ran: a rejected push below is only the caller's conflict if that moved
    // underneath us.
    let upstream_before =
        match replay_rebase(&git, &canonical_repo, worktree.path(), &upstream, &head_sha)? {
            RebaseReplay::Replayed { upstream_sha } => upstream_sha,
            // A rebase git stopped on is the same *state* outcome the merge and
            // squash strategies already report as a `409`: the request was
            // correct, the server understood it, and the author has something to
            // do about it. Only the strategy differed, so the status must not.
            // The stderr stays in the log — a `Conflict` renders verbatim to the
            // client and must not carry a git command line (H-05).
            RebaseReplay::Conflicted { git_output } => {
                tracing::warn!(
                    base_branch,
                    head_ref,
                    stderr = %git_output,
                    "rebase merge stopped on a conflict"
                );
                return Err(crate::error::conflict(
                    "rebase conflict: the pull request no longer applies onto the base branch",
                ));
            }
        };

    let target_ref = format!("HEAD:refs/heads/{base_branch}");
    let push = git.run(&["push", "origin", &target_ref], Some(worktree.path()))?;
    if !push.success() {
        // Split the lost race from our own failures the way
        // `repo::service::push_branch_with_lease` does: ask the repository
        // what the base branch says now, rather than reading the rejection
        // text. A base that moved (or was deleted) while the replay ran is a
        // retriable `409`; a push that failed with the base still where we
        // found it is ours.
        let upstream_now = remote_branch_sha(&git, worktree.path(), base_branch)
            .context("failed to re-read the base branch after a rejected push")?;
        if upstream_now.as_deref() != Some(upstream_before.as_str()) {
            tracing::warn!(
                base_branch,
                head_ref,
                stderr = %push.stderr_str(),
                "base branch advanced while rebasing"
            );
            return Err(crate::error::conflict(
                "base branch advanced while rebasing; retry the merge",
            ));
        }
        bail!("rebase merge push failed: {}", push.stderr_str());
    }

    let head = git.run(&["rev-parse", "HEAD"], Some(worktree.path()))?;
    head.ensure_success()
        .context("failed to resolve rebased HEAD")?;
    Ok(head.stdout_str().trim().to_string())
}

/// What the merge queue's rehearsal of the `rebase` strategy produced.
pub(super) enum MergeGroupRebase {
    /// The replay finished: this is the tree it left behind, and the group
    /// commit the queue puts under CI carries it.
    Tree(String),
    /// The replay stopped on something a human has to resolve. The text is
    /// git's own account of it, for the operator log only — it names
    /// server-side paths and must not reach a pull request (H-05).
    Conflict(String),
}

/// Replay `head_sha` onto `base_sha` the way the `rebase` strategy will, and
/// answer with the tree that replay produced.
///
/// This is the queue's rehearsal, so nothing is pushed and no ref moves: the
/// merge group is built out of the returned tree, CI runs on it, and the merge
/// that follows runs [`git_rebase_merge`] over the same [`replay_rebase`].
///
/// `base_sha` is passed as the upstream rather than `origin/<branch>` because
/// the queue has already pinned the base it built the group from; resolving the
/// branch again inside the clone would let the two disagree.
pub(super) fn rebase_group_tree(
    repo_path: &std::path::Path,
    base_sha: &str,
    head_sha: &str,
) -> Result<MergeGroupRebase> {
    let canonical_repo = std::fs::canonicalize(repo_path)
        .with_context(|| format!("failed to canonicalize repository: {:?}", repo_path))?;
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let git = rg_git::invocation::local(gateway);

    let worktree = RebaseWorktree::new(repo_path, crate::staging::WorktreePurpose::MergeGroup)?;
    match replay_rebase(&git, &canonical_repo, worktree.path(), base_sha, head_sha)? {
        RebaseReplay::Conflicted { git_output } => Ok(MergeGroupRebase::Conflict(git_output)),
        RebaseReplay::Replayed { .. } => {
            // The replay wrote its result into the clone, and the clone is about
            // to be removed. The caller builds the group commit inside the
            // served repository, so the objects have to travel there first —
            // they are unreferenced until that commit's ref is published, which
            // is the same window the group commit itself already lives in.
            let fetched = git.run(
                &[
                    "fetch",
                    "--no-tags",
                    &worktree.path().to_string_lossy(),
                    "HEAD",
                ],
                Some(&canonical_repo),
            )?;
            fetched
                .ensure_success()
                .context("failed to bring the replayed objects into the repository")?;

            let tree = git.run(&["rev-parse", "HEAD^{tree}"], Some(worktree.path()))?;
            tree.ensure_success()
                .context("failed to resolve the replayed tree")?;
            Ok(MergeGroupRebase::Tree(tree.stdout_str().trim().to_string()))
        }
    }
}

/// How a replay ended.
enum RebaseReplay {
    /// git replayed every commit. The worktree's detached `HEAD` is the result,
    /// and `upstream_sha` is where the replay was built from.
    Replayed { upstream_sha: String },
    /// git stopped on something only a human can resolve; the string is its own
    /// stderr, for an operator log.
    Conflicted { git_output: String },
}

/// Clone the served repository into `worktree` and replay `head_sha` onto
/// `upstream` there.
///
/// `git rebase` cannot run directly inside a bare repository. Cloning into a
/// unique temporary worktree also keeps an interrupted/conflicting rebase from
/// leaving mutable index state in the served repository.
///
/// The head is addressed by object id rather than by ref because a clone from a
/// local path brings the whole object store with it: the merge queue rehearses a
/// fork head it fetched by object id, which no ref in this repository names.
///
/// Every step runs through [`rg_git::invocation::local`], the subprocess twin of the isolated
/// open the `gix` strategies use: what a replay produces has to be a property of ForgeKeep and
/// not of `/etc/gitconfig`, of the `~/.gitconfig` of the account the server runs under, or of
/// the server process's own `GIT_*` (card_dfee2b7016b9). The identity below already said *who*
/// signed the replay; the policy says what the replay is.
fn replay_rebase(
    git: &rg_git::invocation::LocalGitInvocation<'_>,
    canonical_repo: &std::path::Path,
    worktree: &std::path::Path,
    upstream: &str,
    head_sha: &str,
) -> Result<RebaseReplay> {
    let repo_arg = canonical_repo.to_string_lossy();
    let worktree_arg = worktree.to_string_lossy();
    git.run(&["clone", "--no-checkout", &repo_arg, &worktree_arg], None)?
        .ensure_success()
        .context("failed to create temporary rebase worktree")?;

    git.run(&["checkout", "--detach", head_sha], Some(worktree))?
        .ensure_success()
        .context("failed to check out rebase head")?;

    let upstream_sha =
        resolve_commit(git, worktree, upstream).context("failed to resolve the rebase upstream")?;

    let rebase = git.run_with_env(
        &["rebase", upstream],
        Some(worktree),
        &[
            ("GIT_AUTHOR_NAME", MERGE_SIGNATURE_NAME),
            ("GIT_AUTHOR_EMAIL", MERGE_SIGNATURE_EMAIL),
            ("GIT_COMMITTER_NAME", MERGE_SIGNATURE_NAME),
            ("GIT_COMMITTER_EMAIL", MERGE_SIGNATURE_EMAIL),
        ],
    )?;
    if rebase.success() {
        return Ok(RebaseReplay::Replayed { upstream_sha });
    }
    if rebase_stopped_on_conflict(git, worktree) {
        // The merge backend puts its conflict listing on stdout and its "could
        // not apply" line on stderr, so both are carried: an operator reading
        // the log gets what git said, whichever stream it chose.
        return Ok(RebaseReplay::Conflicted {
            git_output: format!(
                "{}\n{}",
                rebase.stdout_str().trim(),
                rebase.stderr_str().trim()
            )
            .trim()
            .to_string(),
        });
    }
    bail!("rebase merge failed: {}", rebase.stderr_str());
}

/// The commit `rev` names inside `repo`, as a full object id.
fn resolve_commit(
    git: &rg_git::invocation::LocalGitInvocation<'_>,
    repo: &std::path::Path,
    rev: &str,
) -> Result<String> {
    let output = git.run(
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
        Some(repo),
    )?;
    output.ensure_success()?;
    Ok(output.stdout_str().trim().to_string())
}

/// A throwaway clone that is removed however the replay ends — including the
/// early returns a conflict takes.
///
/// Staged beside the bare repository it replays against rather than in the
/// system temp directory, which is where it used to go. `Drop` covers every
/// outcome this process survives and none of the ones it does not, and a
/// `TMPDIR` nobody sweeps turned a killed rebase into a permanent full clone of
/// the repository. Beside the repository it is inside the tree
/// [`rg_core::staging::sweep_stale_sibling_spools`](crate::staging::sweep_stale_sibling_spools)
/// walks, under a name [`SiblingSpoolTree::Worktree`](crate::staging::SiblingSpoolTree)
/// recognises.
struct RebaseWorktree(std::path::PathBuf);

impl RebaseWorktree {
    fn new(bare_repo: &std::path::Path, purpose: crate::staging::WorktreePurpose) -> Result<Self> {
        let path = crate::staging::worktree_staging_path(bare_repo, purpose, uuid::Uuid::new_v4())
            .with_context(|| {
                format!(
                    "cannot stage a rebase worktree beside {}: the repository path is not \
                     `<owner>/<name>.git`",
                    bare_repo.display()
                )
            })?;
        Ok(Self(path))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for RebaseWorktree {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = ?self.0, %error, "failed to remove temporary rebase worktree");
            }
        }
    }
}

/// Did `git rebase` stop because a human has to resolve something?
///
/// Asked of git's own state rather than of its message: a rebase that started
/// and could not finish leaves its state directory in place and unmerged stages
/// in the index, while a rebase that never started (an unreachable upstream, a
/// broken object store) leaves neither. Matching on stderr would make the status
/// a property of git's wording — the antipattern this whole error family exists
/// to remove — and the two backends word it differently.
///
/// A failure to ask counts as "not a conflict": an unclassified rebase failure
/// stays a `500`, which is the honest answer when we could not find out.
fn rebase_stopped_on_conflict(
    git: &rg_git::invocation::LocalGitInvocation<'_>,
    worktree: &std::path::Path,
) -> bool {
    let git_dir = worktree.join(".git");
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        return true;
    }

    git.run(&["ls-files", "--unmerged"], Some(worktree))
        .map(|output| output.success() && !output.stdout_str().trim().is_empty())
        .unwrap_or(false)
}

/// The commit `origin` currently has for `branch`, or `None` if it has no such
/// branch any more. An unreadable remote is an error, not an absent branch.
fn remote_branch_sha(
    git: &rg_git::invocation::LocalGitInvocation<'_>,
    worktree: &std::path::Path,
    branch: &str,
) -> Result<Option<String>> {
    let refname = format!("refs/heads/{branch}");
    let output = git.run(&["ls-remote", "origin", &refname], Some(worktree))?;
    output
        .ensure_success()
        .context("failed to list the base branch on the served repository")?;
    Ok(output
        .stdout_str()
        .split_whitespace()
        .next()
        .map(str::to_string))
}

/// Set HEAD to point to a branch (equivalent to `git checkout <branch>` in a bare repo).
/// Uses gix to update the HEAD symbolic reference.
#[allow(dead_code)]
fn gix_set_head_to_branch(repo_path: &std::path::Path, branch: &str) -> Result<()> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    gix_set_head_to_branch_with_repo(&repo, branch)
}

/// Same as `gix_set_head_to_branch` but takes an already-open `Repository`.
fn gix_set_head_to_branch_with_repo(repo: &gix::Repository, branch: &str) -> Result<()> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::{FullName, Target};

    let branch_ref: FullName = format!("refs/heads/{}", branch)
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid branch reference: {}", e))?;
    let head_name: FullName = "HEAD"
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid HEAD reference: {}", e))?;

    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "checkout".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch_ref),
        },
        name: head_name,
        deref: false,
    })
    .map_err(|e| anyhow::anyhow!("failed to set HEAD to refs/heads/{}: {}", branch, e))?;

    Ok(())
}

/// Fast-forward a branch to point to another branch's commit (equivalent to `git merge --ff-only`).
/// Uses gix to update the base branch reference.
#[allow(dead_code)]
fn gix_fast_forward(
    repo_path: &std::path::Path,
    base_branch: &str,
    head_branch: &str,
) -> Result<()> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    gix_fast_forward_with_repo(&repo, base_branch, head_branch)
}

/// Same as `gix_fast_forward` but takes an already-open `Repository`.
fn gix_fast_forward_with_repo(
    repo: &gix::Repository,
    base_branch: &str,
    head_branch: &str,
) -> Result<()> {
    let head_ref_str = format!("refs/heads/{}", head_branch);
    let base_ref_str = format!("refs/heads/{}", base_branch);

    // Resolve head branch commit
    let head_id = repo
        .rev_parse_single(head_ref_str.as_str())
        .map_err(|e| anyhow::anyhow!("failed to resolve {}: {}", head_ref_str, e))?;

    // Update base branch to point to head's commit
    repo.reference(
        base_ref_str.as_str(),
        head_id.detach(),
        gix::refs::transaction::PreviousValue::Any,
        "fast-forward merge",
    )
    .map_err(|e| anyhow::anyhow!("fast-forward failed: {}", e))?;

    Ok(())
}

#[allow(dead_code)]
fn get_head_sha(repo_path: &std::path::Path) -> Result<String> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    get_head_sha_with_repo(&repo)
}

/// Same as `get_head_sha` but takes an already-open `Repository`.
fn get_head_sha_with_repo(repo: &gix::Repository) -> Result<String> {
    let head_id = repo
        .rev_parse_single("HEAD")
        .map_err(|e| anyhow::anyhow!("failed to parse HEAD: {}", e))?;
    Ok(head_id.to_string())
}

/// Require the branch recorded on a pull request to still exist.
///
/// The branch is not request input at this point: it was accepted when the PR
/// was created and may legitimately have been deleted afterwards. Keep that
/// stale resource state separate from a failed repository read so API callers
/// can refresh on 409 and retry on 5xx.
fn require_pull_request_branch(
    repo_path: &std::path::Path,
    kind: &str,
    branch: &str,
) -> Result<()> {
    crate::repo::service::try_get_branch_sha(repo_path, branch)?.ok_or_else(|| {
        crate::error::conflict(format!(
            "pull request {kind} branch '{branch}' no longer exists"
        ))
    })?;
    Ok(())
}

/// Resolve a branch reference to its SHA using gix.
fn get_ref_sha(repo_path: &std::path::Path, branch: &str) -> Result<String> {
    crate::repo::service::try_get_branch_sha(repo_path, branch)?.ok_or_else(|| {
        anyhow::anyhow!(
            "failed to resolve refs/heads/{}: reference does not exist",
            branch
        )
    })
}

// ── Gix merge helpers ───────────────────────────────────────────────────

/// The identity ForgeKeep signs merge commits with.
///
/// Load-bearing, not cosmetic: `gix`'s plain `Repository::commit` resolves the
/// author and committer from the host's git configuration, and refuses with
/// "Author identity is not configured" when the machine has none. A server in a
/// container has none, so every `merge` and `squash` merge answered
/// `500 INTERNAL_ERROR` there while passing on a developer's box — and where the
/// host *did* have one, the merge commit was signed with whoever happened to
/// have run `git config --global` on that machine.
///
/// Both halves are the same bug: the commit's identity must come from
/// ForgeKeep, not from the host it runs on. The rebase strategy already passed
/// this identity to `git rebase` explicitly; these constants are now the single
/// place all three strategies read it from.
const MERGE_SIGNATURE_NAME: &str = "ForgeKeep";
const MERGE_SIGNATURE_EMAIL: &str = "noreply@forgekeep.local";

/// [`MERGE_SIGNATURE_NAME`] / [`MERGE_SIGNATURE_EMAIL`] as a `gix` signature
/// stamped at the current time.
///
/// `SignatureRef::time` is the raw git wire form — `<seconds> <offset>` — so it
/// is rendered here rather than borrowed from a config file. UTC, because the
/// server's local zone is an operator's deployment choice and must not end up
/// inside the object a merge produces.
fn merge_signature(time: &str) -> gix::actor::SignatureRef<'_> {
    gix::actor::SignatureRef {
        name: MERGE_SIGNATURE_NAME.into(),
        email: MERGE_SIGNATURE_EMAIL.into(),
        time,
    }
}

/// The `<seconds> +0000` stamp [`merge_signature`] borrows.
///
/// Kept separate so the caller owns the string: `SignatureRef` borrows its time,
/// and a temporary built inside `merge_signature` would not outlive the call.
fn merge_signature_time() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format!("{seconds} +0000")
}

/// Delete a reference using gix (replaces `git update-ref -d <ref>`).
fn gix_delete_ref(repo_path: &std::path::Path, ref_name: &str) -> Result<()> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
    use gix::refs::FullName;

    let full_name: FullName = ref_name
        .try_into()
        .map_err(|e| anyhow::anyhow!("invalid ref name '{}': {}", ref_name, e))?;

    repo.edit_reference(RefEdit {
        change: Change::Delete {
            expected: PreviousValue::Any,
            log: RefLog::AndReference,
        },
        name: full_name,
        deref: false,
    })
    .map_err(|e| anyhow::anyhow!("failed to delete ref '{}': {}", ref_name, e))?;

    Ok(())
}

/// Perform a `--no-ff` merge using gix merge_commits API.
/// Creates a merge commit with two parents (current HEAD + `head_ref`).
fn gix_merge_no_ff(repo_path: &std::path::Path, head_ref: &str, message: &str) -> Result<String> {
    // `rg_git::repository::open`, not `gix::open`: the bytes of a merge must not
    // depend on the machine — see [`forgekeep_merge_options`].
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let our_commit = repo
        .rev_parse_single("HEAD")
        .map_err(|e| anyhow::anyhow!("failed to resolve HEAD: {}", e))?;
    let their_commit = repo
        .rev_parse_single(head_ref)
        .with_context(|| format!("failed to resolve merge ref '{}'", head_ref))?;

    let merged_tree_id = gix_merge_commits_to_tree(&repo, our_commit, their_commit, head_ref)?;

    // Create merge commit (two parents).
    //
    // `commit_as`, not `commit`: the latter reads the identity out of the host's
    // git configuration — see [`MERGE_SIGNATURE_NAME`].
    let stamp = merge_signature_time();
    let signature = merge_signature(&stamp);
    let commit_id = repo
        .commit_as(
            signature,
            signature,
            "HEAD",
            message,
            merged_tree_id.detach(),
            [our_commit.detach(), their_commit.detach()],
        )
        .map_err(|e| anyhow::anyhow!("failed to create merge commit: {}", e))?;

    Ok(commit_id.detach().to_string())
}

/// Perform a squash merge: merge commits, then create a single-parent commit.
fn gix_squash_merge(repo_path: &std::path::Path, head_ref: &str, message: &str) -> Result<String> {
    // Opened the same way `gix_merge_no_ff` is, and for the same reason.
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let our_commit = repo
        .rev_parse_single("HEAD")
        .map_err(|e| anyhow::anyhow!("failed to resolve HEAD: {}", e))?;
    let their_commit = repo
        .rev_parse_single(head_ref)
        .with_context(|| format!("failed to resolve merge ref '{}'", head_ref))?;

    let merged_tree_id = gix_merge_commits_to_tree(&repo, our_commit, their_commit, head_ref)?;

    // Squash merge: single-parent commit, signed the same way the merge commit
    // above is — see [`MERGE_SIGNATURE_NAME`] for why not `repo.commit`.
    let stamp = merge_signature_time();
    let signature = merge_signature(&stamp);
    let commit_id = repo
        .commit_as(
            signature,
            signature,
            "HEAD",
            message,
            merged_tree_id.detach(),
            [our_commit.detach()],
        )
        .map_err(|e| anyhow::anyhow!("failed to create squash commit: {}", e))?;

    Ok(commit_id.detach().to_string())
}

/// The merge semantics ForgeKeep guarantees, written down instead of looked up.
///
/// `repo.tree_merge_options()` is the natural-looking call and the wrong one:
/// it *reads* `merge.renames`, `merge.renameLimit`, `diff.renames`,
/// `merge.conflictStyle` and `diff.algorithm` out of whatever configuration the
/// repository was opened with. Opening through [`rg_git::repository::open`]
/// already puts the host's `/etc/gitconfig`, `~/.gitconfig` and `GIT_*` out of
/// reach, but a lookup would still leave the answer to "what tree does this
/// pull request merge to" as a property of a config file rather than of
/// ForgeKeep — and that answer has to be identical on every instance.
///
/// The values below are git's own defaults, i.e. exactly what an unconfigured
/// host produced before: rename tracking on at 50% similarity with a
/// 1000-entry limit, Myers diff, conflicts kept in `merge` style with
/// 7-character markers. The struct is spelled out field by field on purpose —
/// a knob gix adds later then breaks the build here instead of quietly
/// defaulting to whatever the upstream default happens to be.
///
/// This owns the tree options; [`forgekeep_merge_resource_cache`] separately
/// owns the blob platform that applies them. Both halves are required because
/// gix's convenience `Repository::merge_commits` constructs that platform from
/// repository configuration internally.
fn forgekeep_merge_options() -> gix::merge::commit::Options {
    use gix::merge::blob::builtin_driver::text;

    gix::merge::plumbing::tree::Options {
        rewrites: Some(gix::diff::Rewrites::default()),
        blob_merge: gix::merge::blob::platform::merge::Options {
            is_virtual_ancestor: false,
            resolve_binary_with: None,
            text: text::Options {
                diff_algorithm: gix::diff::blob::Algorithm::Myers,
                conflict: text::Conflict::Keep {
                    style: text::ConflictStyle::Merge,
                    marker_size: text::Conflict::DEFAULT_MARKER_SIZE
                        .try_into()
                        .expect("git's default conflict marker size is not zero"),
                },
            },
        },
        blob_merge_command_ctx: Default::default(),
        fail_on_conflict: None,
        marker_size_multiplier: 0,
        symlink_conflicts: None,
        tree_conflicts: None,
    }
    .into()
}

/// Build the blob-merge platform from ForgeKeep-owned policy.
///
/// `Repository::merge_resource_cache` deliberately follows Git configuration:
/// repository-local `merge.default`, `merge.renormalize`, custom
/// `merge.<name>` drivers and `core.bigFileThreshold` all affect it even when
/// the repository was opened with isolated host permissions. A server merge
/// must instead depend only on committed attributes and the explicit options
/// below. Named attributes still select gix's built-in `text`, `binary` and
/// `union` drivers; no command from `.git/config` is admitted.
fn forgekeep_merge_resource_cache(repo: &gix::Repository) -> Result<gix::merge::blob::Platform> {
    let mut worktree_filter = gix::filter::plumbing::Pipeline::default();
    worktree_filter.options_mut().object_hash = repo.object_hash();
    let filter = gix::merge::blob::Pipeline::new(
        Default::default(),
        worktree_filter,
        gix::merge::blob::pipeline::Options {
            large_file_threshold_bytes: FORGEKEEP_LARGE_FILE_THRESHOLD_BYTES,
        },
    );

    Ok(gix::merge::blob::Platform::new(
        filter,
        gix::merge::blob::pipeline::Mode::ToGit,
        forgekeep_committed_attribute_stack(repo)?,
        Vec::new(),
        Default::default(),
    ))
}

/// Merge two commits through gix plumbing while owning both resource caches.
fn forgekeep_merge_commits<'repo>(
    repo: &'repo gix::Repository,
    our_commit: gix::Id<'repo>,
    their_commit: gix::Id<'repo>,
    labels: gix::merge::blob::builtin_driver::text::Labels<'_>,
) -> Result<gix::merge::plumbing::commit::Outcome<'repo>> {
    use gix::prelude::ObjectIdExt as _;

    let mut diff_cache = forgekeep_diff_resource_cache(repo)?;
    let mut blob_merge = forgekeep_merge_resource_cache(repo)?;
    let commit_graph = repo.commit_graph_if_enabled()?;
    let mut graph = repo.revision_graph(commit_graph.as_ref());

    Ok(gix::merge::plumbing::commit(
        our_commit.detach(),
        their_commit.detach(),
        labels,
        &mut graph,
        &mut diff_cache,
        &mut blob_merge,
        repo,
        &mut |id| id.to_owned().attach(repo).shorten_or_id().to_string(),
        forgekeep_merge_options().into(),
    )?)
}

/// Core merge logic: merge two commits and write the merged tree.
///
/// The conflict gate here is `has_unresolved_conflicts`, not the length of
/// `outcome.tree_merge.conflicts`, and the difference is the whole behaviour of
/// the merge endpoint. `gix` documents that list as "conflicts might have been
/// auto-resolved, but they are listed here for completeness": two branches that
/// edited *different lines of the same file* produce an entry there whose
/// resolution is `Ok(…)` and whose merged blob is already computed. Rejecting on
/// a non-empty list therefore rejected every pull request whose two sides
/// touched one file — most of them — with a `409` telling the author to resolve
/// a conflict that does not exist, and accepted only the merges where no content
/// was merged at all. `TreatAsUnresolved::git()` is git's own definition of
/// "still needs a human", which is the question this gate meant to ask.
fn gix_merge_commits_to_tree<'repo>(
    repo: &'repo gix::Repository,
    our_commit: gix::Id<'repo>,
    their_commit: gix::Id<'repo>,
    their_label: &str,
) -> Result<gix::Id<'repo>> {
    use gix::merge::blob::builtin_driver::text::Labels;
    use gix::merge::tree::TreatAsUnresolved;
    use gix::prelude::ObjectIdExt as _;

    let labels = Labels {
        current: Some("HEAD".into()),
        other: Some(their_label.into()),
        ancestor: None, // auto-determined from merge-base
    };

    let mut outcome = forgekeep_merge_commits(repo, our_commit, their_commit, labels)
        .map_err(|e| anyhow::anyhow!("merge failed: {}", e))?;

    // Check for conflicts git would leave to a human — see the note above for
    // why the auto-resolved ones in the same list do not count.
    let unresolved = TreatAsUnresolved::git();
    if outcome.tree_merge.has_unresolved_conflicts(unresolved) {
        let count = outcome
            .tree_merge
            .conflicts
            .iter()
            .filter(|conflict| conflict.is_unresolved(unresolved))
            .count();
        tracing::warn!("merge has {} unresolved conflict(s)", count);
        return Err(crate::error::conflict(format!(
            "merge conflict detected: {} files with conflicts",
            count
        )));
    }

    // Write the merged tree to the object database
    let tree_id = outcome
        .tree_merge
        .tree
        .write(|tree| repo.write_object(tree).map(|id| id.detach()))
        .map_err(|e| anyhow::anyhow!("failed to write merged tree: {}", e))?;
    Ok(tree_id.attach(repo))
}

// ── Helpers ─────────────────────────────────────────────────────────────

async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<rg_db::entities::repository::Model> {
    crate::repo::service::find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| NotFound::new("repository").into())
}

#[cfg(test)]
mod number_allocation_tests {
    use super::*;

    /// A file-backed database with a pool: two creates have to be able to reach
    /// their inserts at the same time, which one shared in-memory connection
    /// cannot do.
    async fn repo_fixture(name: &str) -> (tempfile::TempDir, DatabaseConnection, i64, i64) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            4,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let user = rg_db::ops::user_ops::create_user(
            &db,
            name,
            &format!("{name}@example.invalid"),
            "",
            name,
        )
        .await
        .expect("create user");
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: sea_orm::NotSet,
                owner_id: Set(user.id),
                name: Set(name.to_string()),
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
            },
        )
        .await
        .expect("create repo row");

        (dir, db, user.id, repo.id)
    }

    fn pr_model(repo_id: i64, author_id: i64, title: &str) -> pull_request::ActiveModel {
        let now = Utc::now();
        pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: sea_orm::NotSet,
            title: Set(title.to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(None),
            head_branch: Set(format!("{title}-head")),
            base_branch: Set("main".to_string()),
            head_sha: Set(None),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        }
    }

    /// Both creates read the same `MAX(number) + 1` before either writes — the
    /// exact interleaving the UNIQUE index used to turn into a 500 for whoever
    /// was second. Held open by a barrier rather than by timing, so the window
    /// is opened on every run.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_creates_on_one_read_take_two_numbers() {
        let (_dir, db, user_id, repo_id) = repo_fixture("prrace").await;

        let same_read = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let gate = |barrier: std::sync::Arc<tokio::sync::Barrier>| {
            move |attempt: usize| {
                let barrier = barrier.clone();
                async move {
                    if attempt == 1 {
                        barrier.wait().await;
                    }
                }
            }
        };

        let first = insert_with_repo_number_gated(
            &db,
            repo_id,
            pr_model(repo_id, user_id, "first"),
            gate(same_read.clone()),
        );
        let second = insert_with_repo_number_gated(
            &db,
            repo_id,
            pr_model(repo_id, user_id, "second"),
            gate(same_read.clone()),
        );

        let (first, second) = tokio::join!(first, second);
        let first = first.expect("the first create of a same-read pair succeeds");
        let second = second.expect("losing the number is not the caller's failure");

        let mut numbers = [first.number, second.number];
        numbers.sort_unstable();
        assert_eq!(
            numbers,
            [1, 2],
            "both correct creates keep a number, and the numbers are consecutive"
        );
    }

    /// The loss itself, on every run rather than only when the scheduler
    /// arranges it: the seam commits a competing row under the number this
    /// create just read, before the create writes. The insert is not inside a
    /// transaction here, so what meets it is the UNIQUE index itself on every
    /// backend.
    #[tokio::test]
    async fn a_number_taken_between_read_and_write_is_re_read() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (_dir, db, user_id, repo_id) = repo_fixture("prthief").await;
        let planted = std::sync::Arc::new(AtomicBool::new(false));

        let pr = insert_with_repo_number_gated(
            &db,
            repo_id,
            pr_model(repo_id, user_id, "mine"),
            |attempt| {
                let db = db.clone();
                let planted = planted.clone();
                async move {
                    // Once: the second attempt must find the number gone and
                    // settle on the next one, not lose it again forever.
                    if attempt == 1 && !planted.swap(true, Ordering::SeqCst) {
                        sea_orm::ConnectionTrait::execute_unprepared(
                            &db,
                            "INSERT INTO pull_requests (repo_id, number, title, state, is_draft, \
                             auto_merge_enabled, author_id, head_branch, base_branch, created_at, \
                             updated_at) VALUES (1, 1, 'taken first', 'open', 0, 0, 1, 'thief', \
                             'main', '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00')",
                        )
                        .await
                        .expect("someone else takes the number");
                    }
                }
            },
        )
        .await
        .expect("losing the number is not the caller's failure");

        assert_eq!(
            pr.number, 2,
            "the loser re-reads the maximum instead of reporting a failed create"
        );
        assert_eq!(pr.title, "mine", "and it is this call's row that survives");
    }

    /// The retry is for one specific loss. A create that fails for a reason
    /// re-reading cannot fix must come back as an error on the first attempt,
    /// not be spun on until the attempt budget runs out.
    #[tokio::test]
    async fn a_broken_pull_requests_table_is_still_an_error() {
        let (_dir, db, user_id, repo_id) = repo_fixture("prbroken").await;
        sea_orm::ConnectionTrait::execute_unprepared(&db, "DROP TABLE pull_requests")
            .await
            .expect("take the pull_requests table away");

        let error = insert_with_repo_number(&db, repo_id, pr_model(repo_id, user_id, "x"))
            .await
            .expect_err("an unusable pull_requests table is a failed create");
        let chain = format!("{error:#}");
        assert!(
            chain.contains("no such table: pull_requests"),
            "the backend's own reason must survive, got: {chain}"
        );
        assert!(
            !chain.contains("stayed contended"),
            "a missing table is not a lost race, got: {chain}"
        );
    }
}

/// `card_318ec3e56901` — the tree a pull request merges to must be a property of
/// ForgeKeep, not of the machine the instance was deployed on.
///
/// The three ownership boundaries use different probes:
///
/// * the merge *options* (`merge.renames`, `merge.conflictStyle`,
///   `diff.algorithm`) are ForgeKeep's own values now, so they hold even against
///   configuration written inside the repository — the one placement an isolated
///   open cannot filter out;
/// * the blob-merge platform ignores repository-local `merge.default`,
///   `merge.renormalize`, `merge.<name>.driver` and `core.bigFileThreshold`;
/// * committed `.gitattributes` remains authoritative, including its built-in
///   merge-driver selection.
///
/// Each test merges its fixture twice: once the way ForgeKeep merges now, and
/// once the way it merged before (`gix::open` + `repo.tree_merge_options()`,
/// kept alive as [`merged_tree_the_old_way`]). That second half is what proves
/// the planted configuration genuinely reaches a merge — without it, "the tree
/// did not change" would be just as true of a probe that missed its target.
///
/// The production half runs through [`super::gix_merge_commits_to_tree`]. The
/// controls call `Repository::merge_commits` directly so a test can prove the
/// planted setting reaches gix's default cache without letting it leak back into
/// production.
///
/// The conflict gate those entry points apply is covered next door, in
/// [`super::merge_conflict_gate_tests`].
#[cfg(test)]
mod merge_configuration_ownership_tests {
    use std::path::{Path, PathBuf};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// Marks the child process spawned by
    /// [`merge_ignores_the_hosts_git_configuration`]; also its only input.
    const HOSTILE_HOST_CONFIG_CHILD: &str = "FORGEKEEP_TEST_HOSTILE_HOST_CONFIG";

    /// What an operator might have in `~/.gitconfig` for their own convenience.
    /// `merge.default` is the half that only an isolated open can deny —
    /// ForgeKeep's explicit options never see it, because `gix` reads it while
    /// building the blob-merge platform inside `merge_commits`.
    const HOSTILE_HOST_CONFIG: &str =
        "[merge]\n\trenames = false\n\tconflictStyle = diff3\n\tdefault = binary\n";

    /// The two knobs the card names, in the placement that survives an isolated
    /// open. Only ForgeKeep owning its options can answer these.
    const HOSTILE_REPOSITORY_CONFIG: &str = "[merge]\n\trenames = false\n\tconflictStyle = diff3\n";
    const HOSTILE_REPOSITORY_MERGE_DEFAULT: &str = "[merge]\n\tdefault = binary\n";
    const HOSTILE_REPOSITORY_DRIVER: &str =
        "[merge \"operator-owned\"]\n\tdriver = forgekeep-test-command-that-does-not-exist %O %A %B\n";

    pub(super) fn git(worktree: &Path, args: &[&str]) {
        let output = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway must initialize")
            .run(args, Some(worktree))
            .expect("git must run");
        assert!(
            output.success(),
            "git {args:?} failed: {}",
            output.stderr_str()
        );
    }

    /// A repository on `main` with a `feature` branch to merge into it.
    ///
    /// `user.*`, `commit.gpgsign` and `core.autocrlf` are pinned in the
    /// repository so the *fixture* is byte-identical in this process and in the
    /// child that runs with a planted host configuration. Nothing pins `merge.*`
    /// — that is what each test plants.
    pub(super) fn init_fixture(root: &Path) -> PathBuf {
        let worktree = root.join("repo");
        std::fs::create_dir_all(&worktree).expect("fixture directory");
        git(&worktree, &["init", "-q", "-b", "main"]);
        git(&worktree, &["config", "user.name", "ForgeKeep Test"]);
        git(
            &worktree,
            &["config", "user.email", "forgekeep@example.test"],
        );
        git(&worktree, &["config", "commit.gpgsign", "false"]);
        git(&worktree, &["config", "core.autocrlf", "false"]);
        worktree
    }

    /// Both branches edit the same file, far enough apart to merge cleanly.
    ///
    /// This is the shape `merge.default = binary` changes: with the text driver
    /// the two edits combine into one blob, with the binary driver one side is
    /// simply chosen and the other is lost.
    pub(super) fn content_merge_fixture(root: &Path) -> PathBuf {
        let worktree = init_fixture(root);
        let file = worktree.join("file.txt");

        std::fs::write(&file, "one\ntwo\nthree\nfour\nfive\nsix\n").expect("base blob");
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-q", "-m", "base"]);
        git(&worktree, &["branch", "feature"]);

        std::fs::write(&file, "ONE\ntwo\nthree\nfour\nfive\nsix\n").expect("our blob");
        git(&worktree, &["commit", "-q", "-am", "edit the first line"]);

        git(&worktree, &["checkout", "-q", "feature"]);
        std::fs::write(&file, "one\ntwo\nthree\nfour\nfive\nSIX\n").expect("their blob");
        git(&worktree, &["commit", "-q", "-am", "edit the last line"]);
        git(&worktree, &["checkout", "-q", "main"]);

        worktree
    }

    fn attributed_content_merge_fixture(root: &Path, merge_driver: &str) -> PathBuf {
        let worktree = init_fixture(root);
        let file = worktree.join("file.txt");

        std::fs::write(
            worktree.join(".gitattributes"),
            format!("file.txt merge={merge_driver}\n"),
        )
        .expect("merge attributes");
        std::fs::write(&file, "one\ntwo\nthree\nfour\nfive\nsix\n").expect("base blob");
        git(&worktree, &["add", "-A"]);
        git(
            &worktree,
            &["commit", "-q", "-m", "base with merge attributes"],
        );
        git(&worktree, &["branch", "feature"]);

        std::fs::write(&file, "ONE\ntwo\nthree\nfour\nfive\nsix\n").expect("our blob");
        git(&worktree, &["commit", "-q", "-am", "edit the first line"]);

        git(&worktree, &["checkout", "-q", "feature"]);
        std::fs::write(&file, "one\ntwo\nthree\nfour\nfive\nSIX\n").expect("their blob");
        git(&worktree, &["commit", "-q", "-am", "edit the last line"]);
        git(&worktree, &["checkout", "-q", "main"]);

        worktree
    }

    /// `main` renames a file that `feature` edits in place.
    ///
    /// This is the shape `merge.renames` decides: with rename tracking the two
    /// reconcile into the renamed file, without it they are a modify/delete pair
    /// and the edit is dropped on the floor.
    fn rename_fixture(root: &Path) -> PathBuf {
        let worktree = init_fixture(root);
        std::fs::create_dir_all(worktree.join("docs")).expect("fixture directory");
        let guide = worktree.join("docs/guide.md");

        std::fs::write(&guide, "line one\nline two\nline three\n").expect("base blob");
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-q", "-m", "base"]);
        git(&worktree, &["branch", "feature"]);

        git(&worktree, &["mv", "docs/guide.md", "docs/handbook.md"]);
        git(&worktree, &["commit", "-q", "-am", "rename the guide"]);

        git(&worktree, &["checkout", "-q", "feature"]);
        std::fs::write(&guide, "line one\nline two, edited\nline three\n").expect("their blob");
        git(&worktree, &["commit", "-q", "-am", "edit the guide"]);
        git(&worktree, &["checkout", "-q", "main"]);

        worktree
    }

    pub(super) fn plant_repository_config(worktree: &Path, text: &str) {
        let config = worktree.join(".git/config");
        let mut existing = std::fs::read_to_string(&config).expect("repository config");
        existing.push_str(text);
        std::fs::write(&config, existing).expect("planted repository config");
    }

    fn merge_tree(
        repo: &gix::Repository,
        options: gix::merge::commit::Options,
    ) -> anyhow::Result<String> {
        let our = repo.rev_parse_single("HEAD")?;
        let theirs = repo.rev_parse_single("refs/heads/feature")?;
        let mut outcome = repo.merge_commits(
            our,
            theirs,
            gix::merge::blob::builtin_driver::text::Labels {
                current: Some("HEAD".into()),
                other: Some("refs/heads/feature".into()),
                ancestor: None,
            },
            options,
        )?;
        let tree = outcome.tree_merge.tree.write()?.to_string();
        Ok(tree)
    }

    /// The merge as ForgeKeep performs it: the repository opened through
    /// `rg_git::repository::open`, the options stated by
    /// [`super::forgekeep_merge_options`].
    fn merged_tree_forgekeeps_way(worktree: &Path) -> String {
        let repo = rg_git::repository::open(worktree).expect("open the fixture");
        let our = repo.rev_parse_single("HEAD").expect("our commit");
        let theirs = repo
            .rev_parse_single("refs/heads/feature")
            .expect("their commit");
        super::gix_merge_commits_to_tree(&repo, our, theirs, "refs/heads/feature")
            .expect("ForgeKeep merges this fixture")
            .to_string()
    }

    /// The merge as ForgeKeep performed it before `card_318ec3e56901`: an open
    /// that reaches the host, and options read back out of the configuration.
    /// Only the "this probe has teeth" half of each test calls it.
    fn merged_tree_the_old_way(worktree: &Path) -> anyhow::Result<String> {
        let repo = gix::open(worktree)?;
        let options: gix::merge::commit::Options = repo.tree_merge_options()?.into();
        merge_tree(&repo, options)
    }

    /// Configuration inside the repository ForgeKeep opened is the placement an
    /// isolated open does *not* cover — repository-local config is loaded at
    /// every permission level — so here the merge options, and only the merge
    /// options, have to hold.
    #[test]
    fn merge_options_come_from_forgekeep_not_from_the_repository_configuration() {
        let clean_dir = tempfile::tempdir().expect("baseline fixture directory");
        let baseline = merged_tree_forgekeeps_way(&rename_fixture(clean_dir.path()));

        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = rename_fixture(dir.path());
        plant_repository_config(&worktree, HOSTILE_REPOSITORY_CONFIG);

        assert_ne!(
            merged_tree_the_old_way(&worktree).expect("the old way still merges"),
            baseline,
            "the planted `merge.renames = false` never reached the merge, so this test \
             would stay green with the bug in place"
        );
        assert_eq!(
            merged_tree_forgekeeps_way(&worktree),
            baseline,
            "merge.* written into the repository configuration changed the merged tree"
        );
    }

    /// `Options::isolated()` cannot remove `.git/config`. The control therefore
    /// opens through the same wrapper and differs only in using gix's
    /// repository-owned resource cache.
    #[test]
    fn merge_resource_cache_ignores_repository_merge_default() {
        let clean_dir = tempfile::tempdir().expect("baseline fixture directory");
        let baseline = merged_tree_forgekeeps_way(&content_merge_fixture(clean_dir.path()));

        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = content_merge_fixture(dir.path());
        plant_repository_config(&worktree, HOSTILE_REPOSITORY_MERGE_DEFAULT);
        let repo = rg_git::repository::open(&worktree).expect("open the fixture");

        assert_ne!(
            merge_tree(&repo, super::forgekeep_merge_options())
                .expect("the repository-owned cache still merges"),
            baseline,
            "the planted `merge.default = binary` never reached gix's repository cache, so \
             this test would stay green with the bug in place"
        );
        assert_eq!(
            merged_tree_forgekeeps_way(&worktree),
            baseline,
            "repository-local `merge.default` changed the pull-request merge tree"
        );
    }

    #[test]
    fn committed_builtin_merge_attributes_remain_authoritative() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = attributed_content_merge_fixture(dir.path(), "union");
        let repo = rg_git::repository::open(&worktree).expect("open the fixture");
        let expected = merge_tree(&repo, super::forgekeep_merge_options())
            .expect("gix's repository cache honours the committed union driver");

        assert_eq!(
            merged_tree_forgekeeps_way(&worktree),
            expected,
            "ForgeKeep's owned cache dropped the committed `merge=union` attribute"
        );
    }

    #[test]
    fn repository_configured_merge_drivers_are_not_executed() {
        let clean_dir = tempfile::tempdir().expect("baseline fixture directory");
        let baseline = merged_tree_forgekeeps_way(&attributed_content_merge_fixture(
            clean_dir.path(),
            "operator-owned",
        ));

        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = attributed_content_merge_fixture(dir.path(), "operator-owned");
        plant_repository_config(&worktree, HOSTILE_REPOSITORY_DRIVER);
        let repo = rg_git::repository::open(&worktree).expect("open the fixture");

        assert!(
            merge_tree(&repo, super::forgekeep_merge_options()).is_err(),
            "the configured driver command never reached gix's repository cache, so this \
             test would stay green if ForgeKeep imported configured drivers again"
        );
        assert_eq!(
            merged_tree_forgekeeps_way(&worktree),
            baseline,
            "a merge driver from repository configuration changed the pull-request tree"
        );
    }

    /// The half the isolated open is responsible for: `/etc/gitconfig` and
    /// `~/.gitconfig` reach the process only through environment variables, and
    /// a test may not mutate those in place — `rust_sources_do_not_mutate_process_environment`
    /// forbids it, and a shared thread pool is why. So the merge runs in a child
    /// process that inherits the planted variables honestly.
    #[test]
    fn merge_ignores_the_hosts_git_configuration() {
        let clean_dir = tempfile::tempdir().expect("baseline fixture directory");
        let baseline = merged_tree_forgekeeps_way(&content_merge_fixture(clean_dir.path()));

        let dir = tempfile::tempdir().expect("host config directory");
        let system = dir.path().join("system-gitconfig");
        let global = dir.path().join("global-gitconfig");
        std::fs::write(&system, HOSTILE_HOST_CONFIG).expect("system config");
        std::fs::write(&global, HOSTILE_HOST_CONFIG).expect("global config");

        let executable = std::env::current_exe().expect("current test executable");
        let output = std::process::Command::new(executable)
            .env(HOSTILE_HOST_CONFIG_CHILD, "1")
            // `GIT_CONFIG_SYSTEM` stands in for `/etc/gitconfig`, which a test
            // cannot write; `GIT_CONFIG_NOSYSTEM=0` keeps that level switched on.
            .env("GIT_CONFIG_SYSTEM", &system)
            .env("GIT_CONFIG_NOSYSTEM", "0")
            .env("GIT_CONFIG_GLOBAL", &global)
            .args([
                "--exact",
                "pull_request::service::merge_configuration_ownership_tests::\
                 merge_under_a_hostile_host_config_child",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .expect("spawn the host-config child");

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "host-config child failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );

        let reported = |key: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or_else(|| {
                    panic!("child printed no `{key}` line:\nstdout:\n{stdout}\nstderr:\n{stderr}")
                })
                .trim()
                .to_owned()
        };

        assert_ne!(
            reported("old-way="),
            baseline,
            "the planted host configuration never reached the merge, so this test would \
             stay green with the bug in place:\nstdout:\n{stdout}"
        );
        assert_eq!(
            reported("forgekeep="),
            baseline,
            "the host's merge.* changed the merged tree:\nstdout:\n{stdout}"
        );
    }

    /// Driven only by [`merge_ignores_the_hosts_git_configuration`], which is
    /// what supplies the planted environment. The early return keeps
    /// `--run-ignored all` honest instead of failing on a bare invocation.
    #[test]
    #[ignore = "spawned by merge_ignores_the_hosts_git_configuration"]
    fn merge_under_a_hostile_host_config_child() {
        if std::env::var_os(HOSTILE_HOST_CONFIG_CHILD).is_none() {
            return;
        }

        let dir = tempfile::tempdir().expect("child fixture directory");
        let worktree = content_merge_fixture(dir.path());

        match merged_tree_the_old_way(&worktree) {
            Ok(tree) => println!("old-way={tree}"),
            Err(error) => println!("old-way=failed: {error}"),
        }
        println!("forgekeep={}", merged_tree_forgekeeps_way(&worktree));
    }

    /// The behavioural tests above drive `forgekeep_merge_options` and
    /// `rg_git::repository::open` directly, because only a tree id is
    /// comparable against the old way. That leaves the two production call
    /// sites uncovered — reverting either of them to a bare `gix::open` would
    /// keep those tests green — so they are pinned here instead.
    ///
    /// Read from the production view, so the `gix::open` that
    /// [`merged_tree_the_old_way`] deliberately keeps alive a few lines up
    /// cannot satisfy the census. Each half asserts a presence as well as an
    /// absence: a census that has stopped finding the function at all would
    /// otherwise report "no bare open here" about a function it never read.
    #[test]
    fn the_merge_path_opens_and_configures_through_forgekeep() {
        let source = include_str!("service.rs");

        for opener in ["gix_merge_no_ff", "gix_squash_merge"] {
            assert_eq!(
                rust_source::production_function_call_sites(
                    source,
                    opener,
                    &["rg_git::repository::open"]
                )
                .len(),
                1,
                "`{opener}` no longer opens the repository through \
                 `rg_git::repository::open` — either it was reverted, or this census has \
                 stopped reading the function"
            );
            assert!(
                rust_source::production_function_call_sites(source, opener, &["gix::open"])
                    .is_empty(),
                "`{opener}` opens the repository with a bare `gix::open`, which reads the \
                 host's /etc/gitconfig, ~/.gitconfig and GIT_* — see card_318ec3e56901"
            );
        }

        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "gix_merge_commits_to_tree",
                &["forgekeep_merge_commits"]
            )
            .len(),
            1,
            "`gix_merge_commits_to_tree` no longer uses ForgeKeep's owned merge path, or \
             this census has stopped reading the function"
        );
        assert!(
            rust_source::production_function_call_sites(
                source,
                "gix_merge_commits_to_tree",
                &["merge_commits"]
            )
            .is_empty(),
            "`gix_merge_commits_to_tree` delegates to Repository::merge_commits, which \
             rebuilds the blob platform from repository configuration — see \
             card_4dd02123ac76"
        );
        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_merge_commits",
                &["forgekeep_merge_resource_cache"]
            )
            .len(),
            1,
            "the plumbing merge no longer constructs exactly one ForgeKeep-owned blob cache"
        );
        assert!(
            rust_source::production_function_call_sites(
                source,
                "forgekeep_merge_commits",
                &["merge_resource_cache"]
            )
            .is_empty(),
            "the plumbing merge imports repository-local merge configuration again — see \
             card_4dd02123ac76"
        );
    }
}

/// `card_dfee2b7016b9` — a rebase merge must produce the same commit on every
/// machine an instance is deployed on.
///
/// The module above answers that question for the `gix` strategies. This is the
/// subprocess half: `git rebase` runs as a child process, and a child process
/// reads `/etc/gitconfig`, the `~/.gitconfig` of the account the server runs
/// under, and the server's own `GIT_*`. The identity that signs the replay was
/// already ForgeKeep's; what the replay *is* was the host's.
///
/// The two knobs the card proposed — `merge.conflictStyle = diff3` and
/// `core.autocrlf = true` — were measured on git 2.43.0 and change nothing
/// about a replay that applies cleanly: the conflict style only renders markers
/// into files a *stopped* rebase leaves behind, and the CRLF round trip puts
/// the same blob back. A probe built on them would have been green with the bug
/// in place. What bites is `rebase.backend = apply` together with
/// `apply.whitespace = fix`: trailing whitespace is stripped out of somebody
/// else's commit, and the base branch ends up with bytes its author never
/// wrote. Planted with it is `core.hooksPath`, which runs the operator's own
/// scripts three times during one merge.
///
/// `commit.gpgsign = true` bites too — it turns every rebase merge into a `500`
/// on a server with no key — and is deliberately *not* planted: failing
/// outright would mask the content difference this test measures.
///
/// Unix-only for the same reason as `rg-git/tests/ambient_authority.rs`: the
/// planted hook has to be executable, and an executable bit is a Unix fact.
#[cfg(all(test, unix))]
mod rebase_configuration_ownership_tests {
    use std::path::{Path, PathBuf};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// Marks the child process spawned by
    /// [`rebase_ignores_the_hosts_git_configuration`], and carries the one path
    /// it cannot work out for itself.
    const HOSTILE_HOST_CONFIG_CHILD: &str = "FORGEKEEP_TEST_HOSTILE_REBASE_HOST_CONFIG";
    const HOOK_MARKER: &str = "FORGEKEEP_TEST_REBASE_HOOK_MARKER";

    /// A setting nothing reads, so planting it changes no behaviour — what it
    /// measures is whether the host could have set one at all.
    const HOST_PROBE_KEY: &str = "forgekeep.hostprobe";
    const HOST_PROBE_VALUE: &str = "the-host-decided-this";

    fn hostile_host_config(hooks: &Path) -> String {
        format!(
            "[rebase]\n\tbackend = apply\n[apply]\n\twhitespace = fix\n\
             [core]\n\thooksPath = {}\n",
            hooks.display()
        )
    }

    /// A `git` the policy under test does *not* apply to — the fixture builder,
    /// and the reader that looks at what a replay produced.
    fn gateway() -> &'static rg_git::cli_gateway::GitCommandGateway {
        rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway must initialize")
    }

    fn git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = gateway().run(args, Some(repo)).expect("git must run");
        assert!(
            output.success(),
            "git {args:?} failed: {}",
            output.stderr_str()
        );
        output.stdout_str().trim().to_string()
    }

    /// A served bare repository whose `feature` branch replays cleanly onto
    /// `main` — and whose replayed commit carries trailing whitespace, which is
    /// what `apply.whitespace = fix` silently removes.
    ///
    /// The two branches edit opposite ends of one file, so the replay applies
    /// without a conflict on any rename or diff setting.
    fn whitespace_fixture(root: &Path) -> PathBuf {
        let git = super::merge_configuration_ownership_tests::git;
        let worktree = super::merge_configuration_ownership_tests::init_fixture(root);
        let file = worktree.join("file.txt");

        std::fs::write(&file, "one\ntwo\nthree\nfour\nfive\nsix\n").expect("base blob");
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-q", "-m", "base"]);
        git(&worktree, &["branch", "feature"]);

        std::fs::write(&file, "ONE\ntwo\nthree\nfour\nfive\nsix\n").expect("base branch blob");
        git(
            &worktree,
            &["commit", "-q", "-am", "the base branch moves on"],
        );

        git(&worktree, &["checkout", "-q", "feature"]);
        std::fs::write(&file, "one\ntwo\nthree\nfour\nfive\nsix   \n").expect("head branch blob");
        git(
            &worktree,
            &["commit", "-q", "-am", "a line with trailing whitespace"],
        );
        git(&worktree, &["checkout", "-q", "main"]);

        let served = root.join("served.git");
        git(
            root,
            &[
                "clone",
                "--bare",
                worktree.to_string_lossy().as_ref(),
                served.to_string_lossy().as_ref(),
            ],
        );
        served
    }

    /// The rebase merge as ForgeKeep performs it, reported as the tree it wrote.
    ///
    /// The tree rather than the commit because a commit id also carries the
    /// times the replay happened to run at, which differ between two runs of the
    /// same correct code.
    fn rebased_tree_forgekeeps_way(served: &Path) -> String {
        let head = super::git_rebase_merge(served, "main", "refs/heads/feature")
            .expect("ForgeKeep rebases this fixture");
        git_stdout(served, &["rev-parse", &format!("{head}^{{tree}}")])
    }

    /// The inherited variables through which a host reaches a git subprocess,
    /// and which the gateway now removes from every child it starts.
    ///
    /// The controls below restate them as explicit values, which the gateway
    /// applies *after* its disarming — so the control is the same git an
    /// undisarmed child would have been, and it stays inside the one entry point
    /// the production callers live under.
    fn host_environment() -> Vec<(String, String)> {
        std::env::vars()
            .filter(|(key, _)| key.starts_with("GIT_") || key == "HOME" || key == "XDG_CONFIG_HOME")
            .collect()
    }

    /// The same replay as ForgeKeep performed it before `card_dfee2b7016b9`: the
    /// gateway, with an identity and the host's configuration in reach. Only the
    /// "this probe has teeth" half of the test calls it.
    ///
    /// It stops before the push, so the fixture is left exactly as the real path
    /// below expects to find it.
    fn rebased_tree_the_old_way(served: &Path) -> anyhow::Result<String> {
        let git = gateway();
        let replay = tempfile::tempdir()?;
        let worktree = replay.path().join("replay");

        let host = host_environment();
        let mut host: Vec<(&str, &str)> = host
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();

        git.run_with_env(
            &[
                "clone",
                "--no-checkout",
                served.to_string_lossy().as_ref(),
                worktree.to_string_lossy().as_ref(),
            ],
            None,
            &host,
        )?
        .ensure_success()?;
        git.run_with_env(
            &["fetch", "origin", "refs/heads/feature"],
            Some(&worktree),
            &host,
        )?
        .ensure_success()?;
        git.run_with_env(
            &["checkout", "--detach", "FETCH_HEAD"],
            Some(&worktree),
            &host,
        )?
        .ensure_success()?;

        host.extend_from_slice(&[
            ("GIT_AUTHOR_NAME", super::MERGE_SIGNATURE_NAME),
            ("GIT_AUTHOR_EMAIL", super::MERGE_SIGNATURE_EMAIL),
            ("GIT_COMMITTER_NAME", super::MERGE_SIGNATURE_NAME),
            ("GIT_COMMITTER_EMAIL", super::MERGE_SIGNATURE_EMAIL),
        ]);
        git.run_with_env(&["rebase", "origin/main"], Some(&worktree), &host)?
            .ensure_success()?;

        let tree = git.run(&["rev-parse", "HEAD^{tree}"], Some(&worktree))?;
        tree.ensure_success()?;
        Ok(tree.stdout_str().trim().to_string())
    }

    /// A hooks directory whose scripts record that they ran.
    fn planted_hooks(root: &Path) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let hooks = root.join("hooks");
        std::fs::create_dir_all(&hooks).expect("hooks directory");
        let marker = root.join("hook-ran");
        // `post-checkout` fires on the detached checkout of the head, and
        // `post-rewrite` on the finished replay: between them they cover both
        // ends of the operation.
        for hook in ["post-checkout", "post-rewrite"] {
            let script = hooks.join(hook);
            std::fs::write(
                &script,
                format!("#!/bin/sh\necho {hook} >> {}\n", marker.display()),
            )
            .expect("write hook");
            let mut permissions = std::fs::metadata(&script)
                .expect("hook metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&script, permissions).expect("make the hook executable");
        }
        (hooks, marker)
    }

    /// The host's configuration reaches the process only through environment
    /// variables, and a test may not mutate those in place —
    /// `rust_sources_do_not_mutate_process_environment` forbids it, and a shared
    /// thread pool is why. So the replay runs in a child process that inherits
    /// the planted variables honestly.
    #[test]
    fn rebase_ignores_the_hosts_git_configuration() {
        let clean_dir = tempfile::tempdir().expect("baseline fixture directory");
        let baseline = rebased_tree_forgekeeps_way(&whitespace_fixture(clean_dir.path()));

        let dir = tempfile::tempdir().expect("host config directory");
        let (hooks, marker) = planted_hooks(dir.path());
        let system = dir.path().join("system-gitconfig");
        let global = dir.path().join("global-gitconfig");
        let planted = hostile_host_config(&hooks);
        std::fs::write(&system, &planted).expect("system config");
        std::fs::write(&global, &planted).expect("global config");

        let executable = std::env::current_exe().expect("current test executable");
        let output = std::process::Command::new(executable)
            .env(HOSTILE_HOST_CONFIG_CHILD, "1")
            .env(HOOK_MARKER, &marker)
            // `GIT_CONFIG_SYSTEM` stands in for `/etc/gitconfig`, which a test
            // cannot write; `GIT_CONFIG_NOSYSTEM=0` keeps that level switched on.
            .env("GIT_CONFIG_SYSTEM", &system)
            .env("GIT_CONFIG_NOSYSTEM", "0")
            .env("GIT_CONFIG_GLOBAL", &global)
            // The placement neither `GIT_CONFIG_NOSYSTEM` nor `GIT_CONFIG_GLOBAL`
            // answers: indexed configuration injected straight into the
            // environment. Only removing the inherited `GIT_*` closes it, so the
            // key is one ForgeKeep deliberately does not pin — a pinned key would
            // be answered by the command line and prove nothing about removal.
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", HOST_PROBE_KEY)
            .env("GIT_CONFIG_VALUE_0", HOST_PROBE_VALUE)
            .args([
                "--exact",
                "pull_request::service::rebase_configuration_ownership_tests::\
                 rebase_under_a_hostile_host_config_child",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .expect("spawn the host-config child");

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "host-config child failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );

        let reported = |key: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or_else(|| {
                    panic!("child printed no `{key}` line:\nstdout:\n{stdout}\nstderr:\n{stderr}")
                })
                .trim()
                .to_owned()
        };

        assert_ne!(
            reported("old-way="),
            baseline,
            "the planted host configuration never reached the replay, so this test would \
             stay green with the bug in place:\nstdout:\n{stdout}"
        );
        assert_eq!(
            reported("old-way-hooks="),
            "true",
            "the planted `core.hooksPath` never ran either, so neither half of this test \
             proves anything:\nstdout:\n{stdout}"
        );
        assert_eq!(
            reported("forgekeep="),
            baseline,
            "the host's rebase configuration changed what the pull request merged to:\
             \nstdout:\n{stdout}"
        );
        assert_eq!(
            reported("forgekeep-hooks="),
            "false",
            "the host's `core.hooksPath` ran the operator's scripts inside a server-side \
             rebase merge:\nstdout:\n{stdout}"
        );
        assert_eq!(
            reported("injected-old-way="),
            HOST_PROBE_VALUE,
            "`GIT_CONFIG_COUNT` never reached git, so the half below proves nothing about \
             the inherited environment:\nstdout:\n{stdout}"
        );
        assert_eq!(
            reported("injected-forgekeep="),
            "",
            "configuration injected through the server process's own `GIT_*` still reaches \
             a repository-local git subprocess:\nstdout:\n{stdout}"
        );
    }

    /// Driven only by [`rebase_ignores_the_hosts_git_configuration`], which is
    /// what supplies the planted environment. The early return keeps
    /// `--run-ignored all` honest instead of failing on a bare invocation.
    #[test]
    #[ignore = "spawned by rebase_ignores_the_hosts_git_configuration"]
    fn rebase_under_a_hostile_host_config_child() {
        let Some(marker) = std::env::var_os(HOOK_MARKER) else {
            return;
        };
        if std::env::var_os(HOSTILE_HOST_CONFIG_CHILD).is_none() {
            return;
        }
        let marker = PathBuf::from(marker);
        let hooks_ran = |marker: &Path| marker.exists();
        let clear = |marker: &Path| match std::fs::remove_file(marker) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("could not clear the hook marker: {error}"),
        };

        let dir = tempfile::tempdir().expect("child fixture directory");
        let served = whitespace_fixture(dir.path());

        clear(&marker);
        match rebased_tree_the_old_way(&served) {
            Ok(tree) => println!("old-way={tree}"),
            Err(error) => println!("old-way=failed: {error}"),
        }
        println!("old-way-hooks={}", hooks_ran(&marker));

        clear(&marker);
        println!("forgekeep={}", rebased_tree_forgekeeps_way(&served));
        println!("forgekeep-hooks={}", hooks_ran(&marker));

        let probe = ["config", "--get", HOST_PROBE_KEY];
        let host = host_environment();
        let host: Vec<(&str, &str)> = host
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        println!(
            "injected-old-way={}",
            gateway()
                .run_with_env(&probe, Some(&served), &host)
                .expect("git must run")
                .stdout_str()
                .trim()
        );
        println!(
            "injected-forgekeep={}",
            rg_git::invocation::local(gateway())
                .run(&probe, Some(&served))
                .expect("git must run")
                .stdout_str()
                .trim()
        );
    }

    /// The behavioural test drives `git_rebase_merge` itself, so it covers the
    /// replay — but not the two helpers the same function hands its gateway to,
    /// and not a future step added beside them. Both are pinned here instead.
    ///
    /// Read from the production view, so the raw gateway calls
    /// [`rebased_tree_the_old_way`] deliberately keeps alive a few lines up
    /// cannot satisfy the census. The presence half matters as much as the
    /// absence: a census that has stopped finding the function at all would
    /// otherwise report "no raw gateway call here" about a function it never
    /// read.
    #[test]
    fn the_rebase_path_runs_git_under_forgekeeps_configuration() {
        let source = include_str!("service.rs");

        assert_eq!(
            rust_source::production_function_call_sites(
                source,
                "git_rebase_merge",
                &["rg_git::invocation::local"]
            )
            .len(),
            1,
            "`git_rebase_merge` no longer states the configuration it runs git under — \
             either it was reverted, or this census has stopped reading the function"
        );
        assert!(
            rust_source::production_function_call_sites(
                source,
                "git_rebase_merge",
                &["gateway.run", "gateway.run_with_env", "gateway.run_or_bail"]
            )
            .is_empty(),
            "`git_rebase_merge` runs git straight off the gateway again, which reads the \
             host's /etc/gitconfig, ~/.gitconfig and GIT_* — see card_dfee2b7016b9"
        );
    }
}

/// The conflict gate of [`super::gix_merge_commits_to_tree`], from both sides.
///
/// `gix` keeps auto-resolved content merges in `outcome.tree_merge.conflicts`
/// "for completeness", so "the list is non-empty" is true of a merge git
/// finished cleanly. Gating on the length of that list rejected every pull
/// request whose two branches touched one file — `MergeStrategy::Merge` and
/// `MergeStrategy::Squash` both go through here — with a `409` about a conflict
/// nobody had (`card_928f32287e37`).
///
/// So the gate needs pinning in both directions, and these tests drive the two
/// production entry points end to end rather than the helper underneath: a
/// merge git resolves has to produce a commit whose blob carries *both* edits,
/// and a merge git cannot resolve has to stay a `Conflict`, i.e. a `409` and
/// not a `500`.
#[cfg(test)]
mod merge_conflict_gate_tests {
    use super::merge_configuration_ownership_tests::{content_merge_fixture, git, init_fixture};
    use std::path::{Path, PathBuf};

    /// Both branches rewrite the *same* line — the one shape git genuinely
    /// cannot decide on its own.
    pub(super) fn line_conflict_fixture(root: &Path) -> PathBuf {
        let worktree = init_fixture(root);
        let file = worktree.join("file.txt");

        std::fs::write(&file, "one\ntwo\nthree\n").expect("base blob");
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-q", "-m", "base"]);
        git(&worktree, &["branch", "feature"]);

        std::fs::write(&file, "one\nours\nthree\n").expect("our blob");
        git(&worktree, &["commit", "-q", "-am", "our take on line two"]);

        git(&worktree, &["checkout", "-q", "feature"]);
        std::fs::write(&file, "one\ntheirs\nthree\n").expect("their blob");
        git(
            &worktree,
            &["commit", "-q", "-am", "their take on line two"],
        );
        git(&worktree, &["checkout", "-q", "main"]);

        worktree
    }

    /// Read a path out of a commit with git itself, so what is asserted is what
    /// a client cloning this repository would get — not gix's view of its own
    /// output.
    fn file_at(worktree: &Path, commit_sha: &str, path: &str) -> String {
        let spec = format!("{commit_sha}:{path}");
        let output = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway must initialize")
            .run(&["show", &spec], Some(worktree))
            .expect("git must run");
        assert!(
            output.success(),
            "git show {spec} failed: {}",
            output.stderr_str()
        );
        output.stdout_str()
    }

    /// The regression itself: first line against last line of one file is a
    /// clean merge, and both entry points have to commit it with both edits in
    /// the blob.
    #[test]
    fn a_content_merge_git_resolves_is_not_a_conflict() {
        for (label, merge) in [
            (
                "merge",
                super::gix_merge_no_ff as fn(&Path, &str, &str) -> anyhow::Result<String>,
            ),
            ("squash", super::gix_squash_merge),
        ] {
            let dir = tempfile::tempdir().expect("fixture directory");
            let worktree = content_merge_fixture(dir.path());

            let sha = merge(&worktree, "refs/heads/feature", "merge #1").unwrap_or_else(|error| {
                panic!(
                    "`{label}` rejected a merge git resolves cleanly: {error} — \
                         gix lists auto-resolved content merges in `conflicts` too, \
                         see card_928f32287e37"
                )
            });

            assert_eq!(
                file_at(&worktree, &sha, "file.txt"),
                "ONE\ntwo\nthree\nfour\nfive\nSIX\n",
                "`{label}` committed a tree that lost one side of a clean content merge"
            );
        }
    }

    /// The other side of the gate: loosening it must not start accepting the
    /// merges git really cannot make, and the refusal has to stay a `409`
    /// rather than becoming an internal error.
    #[test]
    fn a_merge_git_cannot_resolve_is_still_refused_as_a_conflict() {
        for (label, merge) in [
            (
                "merge",
                super::gix_merge_no_ff as fn(&Path, &str, &str) -> anyhow::Result<String>,
            ),
            ("squash", super::gix_squash_merge),
        ] {
            let dir = tempfile::tempdir().expect("fixture directory");
            let worktree = line_conflict_fixture(dir.path());

            let error = match merge(&worktree, "refs/heads/feature", "merge #1") {
                Ok(sha) => {
                    panic!("`{label}` accepted two rewrites of the same line as commit {sha}")
                }
                Err(error) => error,
            };

            let conflict = error
                .downcast_ref::<crate::error::Conflict>()
                .unwrap_or_else(|| {
                    panic!(
                        "`{label}` refused the conflict with {error:?}, which is not a \
                         `Conflict` — the client is told to fix the request, or to \
                         retry a server failure, instead of resolving the conflict"
                    )
                });
            assert!(
                conflict.message.contains("merge conflict detected"),
                "`{label}` reported a conflict as {:?}",
                conflict.message
            );
        }
    }
}

/// How [`super::git_rebase_merge`] reports the two outcomes that belong to the
/// caller rather than to the server.
///
/// `MergeStrategy::Merge` and `MergeStrategy::Squash` already answer a merge
/// conflict with a `409` (`merge_conflict_gate_tests`), but the rebase strategy
/// declared both its state outcomes with `bail!` — an anonymous `anyhow::Error`
/// the HTTP funnel can only classify as a `500`. The same endpoint, on the same
/// conflicting pull request, therefore answered differently depending on the
/// strategy: "the server is broken" about a situation the server understood
/// perfectly and the author can act on (`card_592138c542be`).
///
/// Both directions are pinned, because typing the conflict is only half of it:
/// a rebase that failed for our own reasons must stay a `500`, or the split has
/// simply moved the lie to the other side.
#[cfg(test)]
#[cfg(unix)]
mod rebase_merge_status_tests {
    use super::merge_configuration_ownership_tests::{content_merge_fixture, git};
    use super::merge_conflict_gate_tests::line_conflict_fixture;
    use std::path::Path;

    /// Publish a fixture worktree as the bare repository the merge path serves.
    fn serve_bare(worktree: &Path, bare: &Path) {
        let source = worktree.to_string_lossy().to_string();
        let target = bare.to_string_lossy().to_string();
        git(worktree, &["clone", "--bare", "-q", &source, &target]);
    }

    /// Plant a `pre-receive` hook in the served repository.
    ///
    /// This is the only way to act *inside* the window the race lives in: the
    /// rebase clones, replays and pushes within one call, so the base branch has
    /// to move while that push is being served. The hook reads its stdin so git
    /// never sees a broken pipe instead of the exit status under test.
    fn plant_pre_receive(bare: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        let hook = bare.join("hooks").join("pre-receive");
        std::fs::create_dir_all(hook.parent().expect("hooks directory")).expect("hooks directory");
        std::fs::write(&hook, format!("#!/bin/sh\ncat >/dev/null\n{body}")).expect("hook script");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("hook must be executable");
    }

    fn expect_conflict(error: &anyhow::Error, what: &str) -> String {
        let conflict = error
            .downcast_ref::<crate::error::Conflict>()
            .unwrap_or_else(|| {
                panic!(
                    "{what} was reported as {error:?}, which is not a `Conflict` — the client \
                     is told the server failed, and retries (and alerts) on an incident that \
                     never happened"
                )
            });
        conflict.message.clone()
    }

    /// The message of a `Conflict` reaches the client verbatim, so it must
    /// describe the state and carry nothing git said or where the server keeps
    /// its files (H-05).
    fn assert_no_internals(message: &str, bare: &Path) {
        for leak in [
            "CONFLICT",
            "Merge conflict",
            "hint:",
            "error:",
            "fatal:",
            "forgekeep-rebase",
            ".git",
        ] {
            assert!(
                !message.contains(leak),
                "the conflict message carries {leak:?} from git's own output: {message:?}"
            );
        }
        assert!(
            !message.contains(&*bare.to_string_lossy()),
            "the conflict message carries the server's repository path: {message:?}"
        );
    }

    /// Both branches rewrite the same line: git stops the replay and leaves it
    /// to a human. That is a `409`, exactly as it is for `merge` and `squash`.
    #[test]
    fn a_rebase_git_cannot_replay_is_a_conflict_not_a_server_failure() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = line_conflict_fixture(dir.path());
        let bare = dir.path().join("served.git");
        serve_bare(&worktree, &bare);

        let error = match super::git_rebase_merge(&bare, "main", "refs/heads/feature") {
            Ok(sha) => panic!("rebase accepted two rewrites of one line as commit {sha}"),
            Err(error) => error,
        };

        let message = expect_conflict(&error, "a rebase conflict");
        assert!(
            message.contains("rebase conflict"),
            "a rebase conflict was reported as {message:?}"
        );
        assert_no_internals(&message, &bare);

        assert_eq!(
            git_stdout(&bare, &["rev-parse", "refs/heads/main"]),
            git_stdout(&worktree, &["rev-parse", "refs/heads/main"]),
            "the refused rebase moved the base branch anyway"
        );
    }

    /// The base branch moves while the replay is running and the fast-forward is
    /// rejected. The request was correct and is worth retrying — a `409`, not an
    /// incident.
    #[test]
    fn a_base_branch_that_moved_under_the_rebase_is_a_conflict() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = content_merge_fixture(dir.path());
        let bare = dir.path().join("served.git");
        serve_bare(&worktree, &bare);
        plant_pre_receive(
            &bare,
            // Written as a loose ref rather than with `git update-ref`, which a
            // hook cannot use: git serves a push inside a quarantine
            // environment where ref updates are forbidden.
            "git rev-parse refs/heads/main^ > \"${GIT_DIR:-.}/refs/heads/main\"\nexit 1\n",
        );

        let error = match super::git_rebase_merge(&bare, "main", "refs/heads/feature") {
            Ok(sha) => panic!("the rejected push was reported as merge commit {sha}"),
            Err(error) => error,
        };

        let message = expect_conflict(&error, "a base branch that advanced mid-rebase");
        assert!(
            message.contains("base branch advanced"),
            "a lost race for the base branch was reported as {message:?}"
        );
        assert_no_internals(&message, &bare);
    }

    /// The other direction: the push is rejected with the base branch still
    /// exactly where the rebase found it. Nothing about the caller's request
    /// explains that, so it has to stay a server failure — otherwise the split
    /// only moved the lie, and a broken repository would answer "retry later"
    /// forever.
    #[test]
    fn a_push_that_failed_on_its_own_stays_a_server_failure() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let worktree = content_merge_fixture(dir.path());
        let bare = dir.path().join("served.git");
        serve_bare(&worktree, &bare);
        plant_pre_receive(&bare, "exit 1\n");

        let error = match super::git_rebase_merge(&bare, "main", "refs/heads/feature") {
            Ok(sha) => panic!("the rejected push was reported as merge commit {sha}"),
            Err(error) => error,
        };

        assert!(
            error.downcast_ref::<crate::error::Conflict>().is_none(),
            "a push the base branch does not explain was reported as a `Conflict`: {error:?}"
        );
    }

    fn git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway must initialize")
            .run(args, Some(repo))
            .expect("git must run");
        assert!(
            output.success(),
            "git {args:?} failed: {}",
            output.stderr_str()
        );
        output.stdout_str().trim().to_string()
    }
}
