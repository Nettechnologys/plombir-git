//! Pull request service — PR creation, diff, merge strategies.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{DatabaseConnection, EntityTrait, Set};
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

/// Compute diff for same-repo PR.
fn compute_same_repo_diff(repo_path: &std::path::Path, pr: &PullRequest) -> Result<PrDiff> {
    // Use gix tree-diff for numstat (files_changed + per-file additions/deletions)
    let (files_changed, stats) = gix_diff_numstat(
        repo_path,
        format!("refs/heads/{}", pr.base_branch),
        format!("refs/heads/{}", pr.head_branch),
    )?;

    // Get unified diff patch via gateway (TODO(gix): replace with gix blob-diff
    // when byte-identical output is achievable — see plan.md Phase 3)
    let git = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let range = format!("{}...{}", pr.base_branch, pr.head_branch);
    let patch_output = git.run(
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--find-renames",
            &range,
        ],
        Some(repo_path),
    )?;
    patch_output.ensure_success()?;
    let patch_text = patch_output.stdout_str();

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
    // Use gix tree-diff for numstat (files_changed + per-file additions/deletions)
    let (files_changed, stats) = gix_diff_numstat(
        repo_path,
        format!("refs/heads/{}", base_branch),
        fork_ref.to_string(),
    )?;

    // Get unified diff patch via gateway (TODO(gix): replace with gix blob-diff when feasible)
    let git = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let range = format!("{}...{}", base_branch, fork_ref);
    let patch_output = git.run(
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--find-renames",
            &range,
        ],
        Some(repo_path),
    )?;
    patch_output.ensure_success()?;
    let patch_text = patch_output.stdout_str();

    let mut files = files_changed;
    attach_patches(&mut files, &patch_text);

    Ok(PrDiff {
        base_branch: pr.base_branch.clone(),
        head_branch: pr.head_branch.clone(),
        stats,
        files_changed: files,
    })
}

fn attach_patches(files: &mut [FileDiff], unified_diff: &str) {
    let patches = split_unified_diff(unified_diff);
    for file in files {
        if let Some(patch) = patches.get(&file.path) {
            file.lines = parse_diff_lines(patch);
            file.patch = Some(patch.clone());
        }
    }
}

fn split_unified_diff(unified_diff: &str) -> HashMap<String, String> {
    let mut patches = HashMap::new();
    let mut current_path: Option<String> = None;
    let mut current_patch = String::new();

    let flush =
        |path: &mut Option<String>, patch: &mut String, patches: &mut HashMap<String, String>| {
            if let Some(path) = path.take() {
                patches.insert(path, std::mem::take(patch));
            }
        };

    for line in unified_diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            flush(&mut current_path, &mut current_patch, &mut patches);
            let header = line.trim_end();
            current_path = header
                .split_whitespace()
                .nth(3)
                .map(|path| path.trim_start_matches("b/").trim_matches('"').to_string());
        } else if let Some(path) = line.strip_prefix("+++ ").map(str::trim) {
            if path != "/dev/null" {
                current_path = Some(path.trim_start_matches("b/").trim_matches('"').to_string());
            }
        }
        if current_path.is_some() {
            current_patch.push_str(line);
        }
    }
    flush(&mut current_path, &mut current_patch, &mut patches);
    patches
}

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
        } else if old_line.is_some() && raw_line.starts_with('+') && !raw_line.starts_with("+++") {
            let line_number = new_line;
            new_line = new_line.map(|line| line + 1);
            lines.push(DiffLine {
                kind: "addition".into(),
                content: raw_line[1..].into(),
                old_line: None,
                new_line: line_number,
            });
        } else if old_line.is_some() && raw_line.starts_with('-') && !raw_line.starts_with("---") {
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

    let mut resource_cache = repo.diff_resource_cache(
        gix::diff::blob::pipeline::Mode::ToGit,
        gix::diff::blob::pipeline::WorktreeRoots::default(),
    )?;

    let file_count;
    {
        let files_ref = &mut files;
        let total_add_ref = &mut total_additions;
        let total_del_ref = &mut total_deletions;

        platform
            .for_each_to_obtain_tree(
                &new_tree,
                |change| -> Result<std::ops::ControlFlow<()>, anyhow::Error> {
                    // The tree walker emits directory entries as well as their
                    // leaf children. A directory has no blob representation, so
                    // handing it to `Change::diff` fails with "Can only diff
                    // blobs and links, not Tree". The children that follow are
                    // the file-level changes we expose to callers.
                    let is_tree = match &change {
                        gix::object::tree::diff::Change::Addition { entry_mode, .. }
                        | gix::object::tree::diff::Change::Deletion { entry_mode, .. } => {
                            entry_mode.is_tree()
                        }
                        gix::object::tree::diff::Change::Modification {
                            previous_entry_mode,
                            entry_mode,
                            ..
                        } => previous_entry_mode.is_tree() || entry_mode.is_tree(),
                        gix::object::tree::diff::Change::Rewrite {
                            source_entry_mode,
                            entry_mode,
                            ..
                        } => source_entry_mode.is_tree() || entry_mode.is_tree(),
                    };
                    if is_tree {
                        return Ok(std::ops::ControlFlow::Continue(()));
                    }

                    let location = change.location().to_str_lossy().to_string();

                    // Only `Ok(None)` means "this file has no line count" — gix
                    // answers that for a binary blob, and a zero numstat is the
                    // right report for it. An `Err` from either step means we
                    // could not read or diff the blob at all; swallowing it here
                    // would publish an unreadable file as an unchanged one.
                    let (additions, deletions) = match change
                        .diff(&mut resource_cache)
                        .with_context(|| format!("failed to diff changed blob: {location}"))?
                        .line_counts()
                        .with_context(|| {
                            format!("failed to count changed lines of blob: {location}")
                        })? {
                        Some(counts) => (counts.insertions as i64, counts.removals as i64),
                        None => (0, 0),
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
    // All three refusals are about the PR's *state*, which is what `Conflict`
    // (409) says: the same request succeeds once the state changes. The merge
    // endpoint next door already answers this way; here they left as a blanket
    // 400 together with the queue lookup and the update below.
    if pr.state != "open" {
        return Err(crate::error::conflict(
            "auto-merge can only be enabled for an open pull request",
        ));
    }
    if pr.is_draft {
        return Err(crate::error::conflict(
            "auto-merge cannot be enabled for a draft pull request",
        ));
    }
    if let Some(entry) = rg_db::ops::merge_queue_ops::find_by_pr(db, pr.id).await? {
        if entry.status == "running" {
            return Err(crate::error::conflict(
                "cannot enable auto-merge while the merge queue is processing this PR",
            ));
        }
        if entry.status == "queued" {
            rg_db::ops::merge_queue_ops::cancel(db, pr.id).await?;
        }
    }
    let mut active: pull_request::ActiveModel = pr.into();
    active.auto_merge_enabled = Set(true);
    active.auto_merge_strategy = Set(Some(strategy.as_str().to_string()));
    active.auto_merge_enabled_by_id = Set(Some(actor_id));
    active.auto_merge_enabled_at = Set(Some(Utc::now()));
    active.updated_at = Set(Utc::now());
    let updated = pull_request_ops::update(db, active).await?;
    rg_db::ops::pr_event_ops::record(
        db,
        updated.repo_id,
        updated.id,
        Some(actor_id),
        "auto_merge_enabled",
        None,
        serde_json::json!({"strategy": strategy.as_str()}),
    )
    .await?;
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
        rg_db::ops::pr_event_ops::record(
            db,
            updated.repo_id,
            updated.id,
            Some(actor_id),
            "auto_merge_disabled",
            None,
            serde_json::json!({}),
        )
        .await?;
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
    let merge = match merge_pr(
        db, repo_root, owner, repo_name, number, actor_id, strategy, None,
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
/// ([`try_auto_merges_for_head_commit`], [`super::merge_queue::process_for_head_commit`]),
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

    crate::branch_protection::service::check_merge_allowed(db, pr.repo_id, &pr.base_branch, pr.id)
        .await?;

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
        let merge_commit_sha = {
            let repo_path = repo_path.clone();
            let pr = pr.clone();
            let merge_ref = merge_ref.clone();
            tokio::task::spawn_blocking(move || -> Result<String> {
                let sha = merge_from_ref(&repo_path, &pr, &merge_ref, strategy)?;
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

    // Same-repo merge — offload gix merge operations to spawn_blocking
    let merge_commit_sha = {
        let repo_path = repo_path.clone();
        let pr = pr.clone();
        tokio::task::spawn_blocking(move || -> Result<String> {
            match strategy {
                MergeStrategy::Merge => do_merge_commit(&repo_path, &pr),
                MergeStrategy::Squash => do_squash_merge(&repo_path, &pr),
                MergeStrategy::Rebase => do_rebase_merge(&repo_path, &pr),
            }
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

/// Merge from an arbitrary ref (used for fork PRs).
/// Uses gix merge APIs for Merge and Squash strategies; Rebase still uses git CLI.
fn merge_from_ref(
    repo_path: &std::path::Path,
    pr: &PullRequest,
    merge_ref: &str,
    strategy: MergeStrategy,
) -> Result<String> {
    match strategy {
        MergeStrategy::Merge => {
            let merge_msg = format!("Merge pull request #{} from {}", pr.number, pr.head_branch);
            gix_merge_no_ff(repo_path, merge_ref, &merge_msg)
        }
        MergeStrategy::Squash => {
            let squash_msg = format!(
                "Squash merge pull request #{} from {}",
                pr.number, pr.head_branch
            );
            gix_squash_merge(repo_path, merge_ref, &squash_msg)
        }
        MergeStrategy::Rebase => git_rebase_merge(repo_path, &pr.base_branch, merge_ref),
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

fn do_merge_commit(repo_path: &std::path::Path, pr: &PullRequest) -> Result<String> {
    let merge_msg = format!("Merge pull request #{} from {}", pr.number, pr.head_branch);
    gix_merge_no_ff(repo_path, &pr.head_branch, &merge_msg)
}

fn do_squash_merge(repo_path: &std::path::Path, pr: &PullRequest) -> Result<String> {
    let squash_msg = format!(
        "Squash merge pull request #{} from {}",
        pr.number, pr.head_branch
    );
    gix_squash_merge(repo_path, &pr.head_branch, &squash_msg)
}

fn do_rebase_merge(repo_path: &std::path::Path, pr: &PullRequest) -> Result<String> {
    // TODO(gix): Replace rebase with gix rebase API (complex operation)
    let head_ref = format!("refs/heads/{}", pr.head_branch);
    git_rebase_merge(repo_path, &pr.base_branch, &head_ref)
}

/// Rebase a PR head in an isolated worktree and fast-forward the bare repository's base ref.
///
/// `git rebase` cannot run directly inside a bare repository. Cloning into a unique temporary
/// worktree also keeps an interrupted/conflicting rebase from leaving mutable index state in the
/// served repository. The final push is a normal fast-forward, so a concurrently advanced base
/// branch is rejected instead of overwritten.
fn git_rebase_merge(
    repo_path: &std::path::Path,
    base_branch: &str,
    head_ref: &str,
) -> Result<String> {
    let canonical_repo = std::fs::canonicalize(repo_path)
        .with_context(|| format!("failed to canonicalize repository: {:?}", repo_path))?;
    let worktree = std::env::temp_dir().join(format!("forgekeep-rebase-{}", uuid::Uuid::new_v4()));
    let git = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let result = (|| -> Result<String> {
        let repo_arg = canonical_repo.to_string_lossy();
        let worktree_arg = worktree.to_string_lossy();
        git.run(&["clone", "--no-checkout", &repo_arg, &worktree_arg], None)?
            .ensure_success()
            .context("failed to create temporary rebase worktree")?;

        let fetch = git.run(&["fetch", "origin", head_ref], Some(&worktree))?;
        if !fetch.success() {
            bail!("failed to fetch rebase head: {}", fetch.stderr_str());
        }
        git.run(&["checkout", "--detach", "FETCH_HEAD"], Some(&worktree))?
            .ensure_success()
            .context("failed to check out rebase head")?;

        let upstream = format!("origin/{base_branch}");
        // Read the base the replay is about to be built on. A rejected push is
        // only the caller's conflict if this moved underneath us, and the
        // comparison needs the "before" side taken before the rebase runs.
        let upstream_before = git.run(&["rev-parse", &upstream], Some(&worktree))?;
        upstream_before
            .ensure_success()
            .context("failed to resolve the rebase upstream")?;
        let upstream_before = upstream_before.stdout_str().trim().to_string();

        let rebase = git.run_with_env(
            &["rebase", &upstream],
            Some(&worktree),
            &[
                ("GIT_AUTHOR_NAME", MERGE_SIGNATURE_NAME),
                ("GIT_AUTHOR_EMAIL", MERGE_SIGNATURE_EMAIL),
                ("GIT_COMMITTER_NAME", MERGE_SIGNATURE_NAME),
                ("GIT_COMMITTER_EMAIL", MERGE_SIGNATURE_EMAIL),
            ],
        )?;
        if !rebase.success() {
            // A rebase git stopped on is the same *state* outcome the merge and
            // squash strategies already report as a `409`: the request was
            // correct, the server understood it, and the author has something to
            // do about it. Only the strategy differed, so the status must not.
            // The stderr stays in the log — a `Conflict` renders verbatim to the
            // client and must not carry a git command line (H-05).
            if rebase_stopped_on_conflict(git, &worktree) {
                tracing::warn!(
                    base_branch,
                    head_ref,
                    stderr = %rebase.stderr_str(),
                    "rebase merge stopped on a conflict"
                );
                return Err(crate::error::conflict(
                    "rebase conflict: the pull request no longer applies onto the base branch",
                ));
            }
            bail!("rebase merge failed: {}", rebase.stderr_str());
        }

        let target_ref = format!("HEAD:refs/heads/{base_branch}");
        let push = git.run(&["push", "origin", &target_ref], Some(&worktree))?;
        if !push.success() {
            // Split the lost race from our own failures the way
            // `repo::service::push_branch_with_lease` does: ask the repository
            // what the base branch says now, rather than reading the rejection
            // text. A base that moved (or was deleted) while the replay ran is a
            // retriable `409`; a push that failed with the base still where we
            // found it is ours.
            let upstream_now = remote_branch_sha(git, &worktree, base_branch)
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

        let head = git.run(&["rev-parse", "HEAD"], Some(&worktree))?;
        head.ensure_success()
            .context("failed to resolve rebased HEAD")?;
        Ok(head.stdout_str().trim().to_string())
    })();

    if let Err(error) = std::fs::remove_dir_all(&worktree) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = ?worktree, %error, "failed to remove temporary rebase worktree");
        }
    }
    result
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
    git: &rg_git::cli_gateway::GitCommandGateway,
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
    git: &rg_git::cli_gateway::GitCommandGateway,
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
/// What this does *not* reach is the blob-merge platform `gix` builds inside
/// `merge_commits`: `merge.renormalize`, `merge.default` and
/// `merge.<name>.driver` are read from the opened repository's own config, so
/// they are bounded by the isolated open rather than by this function.
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

    let labels = Labels {
        current: Some("HEAD".into()),
        other: Some(their_label.into()),
        ancestor: None, // auto-determined from merge-base
    };

    let mut outcome = repo
        .merge_commits(our_commit, their_commit, labels, forgekeep_merge_options())
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
    outcome
        .tree_merge
        .tree
        .write()
        .map_err(|e| anyhow::anyhow!("failed to write merged tree: {}", e))
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
/// The two halves of the fix close different sources, and a different knob
/// reaches each, so each gets its own test:
///
/// * the merge *options* (`merge.renames`, `merge.conflictStyle`,
///   `diff.algorithm`) are ForgeKeep's own values now, so they hold even against
///   configuration written inside the repository — the one placement an isolated
///   open cannot filter out;
/// * everything `gix` reads for itself while building the blob-merge platform
///   (`merge.default`, `merge.renormalize`, `merge.<name>.driver`) is still a
///   config lookup, and is bounded instead by opening the repository isolated.
///
/// Each test merges its fixture twice: once the way ForgeKeep merges now, and
/// once the way it merged before (`gix::open` + `repo.tree_merge_options()`,
/// kept alive as [`merged_tree_the_old_way`]). That second half is what proves
/// the planted configuration genuinely reaches a merge — without it, "the tree
/// did not change" would be just as true of a probe that missed its target.
///
/// The merges run through [`super::forgekeep_merge_options`] and
/// `rg_git::repository::open` rather than through [`super::gix_merge_no_ff`],
/// and that is not a shortcut: what has to be compared here is the merged
/// *tree*, and the two entry points hand back a commit id built on top of a
/// signature and a message. Reading the tree directly is what lets
/// [`merged_tree_the_old_way`] be the same measurement as the new way, differing
/// only in the configuration it was allowed to see.
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

    fn plant_repository_config(worktree: &Path, text: &str) {
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
        merge_tree(&repo, super::forgekeep_merge_options()).expect("ForgeKeep merges this fixture")
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
                &["forgekeep_merge_options"]
            )
            .len(),
            1,
            "`gix_merge_commits_to_tree` no longer states its merge options, or this census \
             has stopped reading the function"
        );
        assert!(
            rust_source::production_function_call_sites(
                source,
                "gix_merge_commits_to_tree",
                &["tree_merge_options"]
            )
            .is_empty(),
            "`gix_merge_commits_to_tree` reads its merge options back out of the git \
             configuration again — see card_318ec3e56901"
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
