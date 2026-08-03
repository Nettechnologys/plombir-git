//! Pull request service — PR creation, diff, merge strategies.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{DatabaseConnection, EntityTrait, Set};
use std::collections::HashMap;

use crate::error::NotFound;
use rg_git::protocol::receive_pack::RefUpdate;

use rg_db::entities::pull_request::{self, Model as PullRequest};
use rg_db::entities::repository as repo_entity;
use rg_db::ops::{pull_request_ops, repo_ops, user_ops};

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

    let number = pull_request_ops::next_number(db, repo_id).await?;

    let target_repo = repo_entity::Entity::find_by_id(repo_id)
        .one(db)
        .await?
        .context("target repository not found")?;
    let target_namespace = repository_namespace(db, &target_repo).await?;
    let target_path = repo_root.join(format!("{target_namespace}/{}.git", target_repo.name));

    // Resolve head SHA (for same-repo PRs, look up branch; for fork PRs, use the head repo).
    // A missing branch is a caller error, but an unreadable repository or ref store
    // is ours: `try_get_ref_sha` preserves that distinction instead of turning both
    // into a nullable `head_sha` on a newly-created PR.
    let head_sha = if let Some(head_repo_id) = head_repo_id {
        // For fork PRs, resolve from the fork repo's git data
        let head_repo = repo_entity::Entity::find_by_id(head_repo_id)
            .one(db)
            .await?
            .context("head repository not found")?;
        let head_namespace = repository_namespace(db, &head_repo).await?;
        let head_path = repo_root.join(format!("{head_namespace}/{}.git", head_repo.name));
        try_get_ref_sha(&head_path, &head_branch)?.ok_or_else(|| {
            crate::error::invalid_request(format!("head branch '{head_branch}' not found"))
        })?
    } else {
        try_get_ref_sha(&target_path, &head_branch)?.ok_or_else(|| {
            crate::error::invalid_request(format!("head branch '{head_branch}' not found"))
        })?
    };

    let model = pull_request::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        number: Set(number),
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
        milestone_id: Set(None),
        labels: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
        closed_at: Set(None),
        merged_at: Set(None),
    };

    let pr = pull_request_ops::create(db, model).await?;
    rg_db::ops::pr_event_ops::record(
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
    .await?;

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

/// Resolve a head reference in `owner:branch` format to (head_branch, head_repo_id).
/// Returns (branch_name, Some(head_repo_id)) if owner differs from the target repo owner,
/// or (branch_name, None) if same owner (same-repo PR).
pub async fn resolve_head_ref(
    db: &DatabaseConnection,
    target_repo_id: i64,
    head_ref: &str,
) -> Result<(String, Option<i64>)> {
    if let Some((head_owner, head_branch)) = head_ref.split_once(':') {
        // Cross-repo (fork) PR: "owner:branch"
        let head_branch = head_branch.to_string();
        // The head ref is client input, so an unknown owner in it is the
        // caller's mistake — `InvalidRequest` keeps it a 400 while a failed
        // lookup on the same line stays a 5xx.
        let head_owner_user = user_ops::find_by_username(db, head_owner)
            .await?
            .ok_or_else(|| {
                crate::error::invalid_request(format!("head owner '{head_owner}' not found"))
            })?;

        // Find the target repo to compare
        let target_repo = repo_entity::Entity::find_by_id(target_repo_id)
            .one(db)
            .await?
            .context("target repository not found")?;

        if head_owner_user.id != target_repo.owner_id {
            // Different owner — this is a fork PR
            // Find the fork repo by the head owner (user may have forked the same repo)
            let fork_repo = repo_ops::find_personal_by_owner_and_name(
                db,
                head_owner_user.id,
                &target_repo.name,
            )
            .await?
            .ok_or_else(|| {
                crate::error::invalid_request(format!(
                    "no repository '{}/{}' found for head owner",
                    head_owner, target_repo.name
                ))
            })?;

            // Verify it's actually a fork of the target
            if fork_repo.origin_repo_id != Some(target_repo_id) && fork_repo.id != target_repo_id {
                return Err(crate::error::invalid_request(format!(
                    "'{}/{}' is not a fork of the target repository",
                    head_owner, target_repo.name
                )));
            }

            return Ok((head_branch, Some(fork_repo.id)));
        }

        // Same owner — not a fork, just a branch reference with owner prefix
        Ok((head_branch, None))
    } else {
        // Simple branch name — same-repo PR
        Ok((head_ref.to_string(), None))
    }
}

pub(super) async fn repository_namespace(
    db: &DatabaseConnection,
    repository: &repo_entity::Model,
) -> Result<String> {
    if let Some(org_id) = repository.org_id {
        return rg_db::ops::org_ops::get_org(db, org_id)
            .await?
            .map(|org| org.name)
            .context("repository organization not found");
    }
    user_ops::find_by_id(db, repository.owner_id)
        .await?
        .map(|user| user.username)
        .context("repository owner not found")
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

/// List PRs for a repo, optionally filtered by state.
pub async fn list_prs(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    state: Option<&str>,
) -> Result<Vec<PullRequest>> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    pull_request_ops::list_by_repo(db, repo.id, state).await
}

/// Paginated list of PRs. Returns (prs, total).
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
        if pr.state != "open" {
            return Err(crate::error::invalid_request(
                "only an open pull request can change draft status",
            ));
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
        rg_db::ops::pr_event_ops::record(
            db,
            updated.repo_id,
            updated.id,
            Some(actor_id),
            if updated.is_draft {
                "pull_request_converted_to_draft"
            } else {
                "pull_request_marked_ready"
            },
            None,
            serde_json::json!({}),
        )
        .await?;
    }
    if previous_state != updated.state {
        let event_type = match updated.state.as_str() {
            "open" => "pull_request_reopened",
            "closed" => "pull_request_closed",
            _ => "pull_request_state_changed",
        };
        rg_db::ops::pr_event_ops::record(
            db,
            updated.repo_id,
            updated.id,
            Some(actor_id),
            event_type,
            None,
            serde_json::json!({"from": previous_state, "to": updated.state}),
        )
        .await?;
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
        let head_owner = user_ops::find_by_id(db, head_repo.owner_id)
            .await?
            .context("head repo owner not found")?;
        let head_repo_path =
            repo_root.join(format!("{}/{}.git", head_owner.username, head_repo.name));

        // Do not let a failed fetch silently reuse an old `refs/forks/...` ref:
        // a deleted head branch is a stale PR state (409), whereas an unreadable
        // fork repository or a failed fetch is our retryable failure (5xx).
        require_pull_request_branch(&head_repo_path, "head", &pr.head_branch)?;
        let fetch_ref = format!("refs/heads/{}", pr.head_branch);
        let local_ref = format!("refs/forks/{}/{}", head_owner.username, pr.head_branch);

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

    let repo = gix::open(repo_path)
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

/// Attempt an enabled auto-merge. Unsatisfied protection rules are returned as
/// a pending outcome, while actual Git/DB failures remain errors.
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
        return Ok(AutoMergeOutcome {
            // `{:#}` — this reason is the whole payload of the outcome; a bare
            // `to_string()` drops the cause the user needs (card_a997f30c142c).
            status: "pending".into(),
            reason: Some(format!("{error:#}")),
            merge: None,
        });
    }

    let strategy = MergeStrategy::parse(
        pr.auto_merge_strategy
            .as_deref()
            .context("auto-merge strategy is missing")?,
    )?;
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
    let merge = match merge_pr(db, repo_root, owner, repo_name, number, strategy, None).await {
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
        let repository = repo_entity::Entity::find_by_id(pr.repo_id)
            .one(db)
            .await?
            .context("auto-merge target repository not found")?;
        let namespace = repository_namespace(db, &repository).await?;
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
///
/// Gix merge operations (tree merge, commit creation) are offloaded to
/// `spawn_blocking` to avoid blocking the tokio async runtime.
pub async fn merge_pr(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    owner: &str,
    repo_name: &str,
    number: i64,
    strategy: MergeStrategy,
    delivery_tracker: Option<&crate::task_tracker::TaskTracker>,
) -> Result<MergeResult> {
    let mut pr = get_pr(db, owner, repo_name, number).await?;

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

    let final_state = pr.state.clone();
    let final_strategy = pr.merge_strategy.clone();
    let final_commit_sha = pr.merge_commit_sha.clone();
    let final_merged_at = pr.merged_at;
    let final_closed_at = pr.closed_at;
    let final_updated_at = pr.updated_at;
    let mut active: pull_request::ActiveModel = pr.into();
    active.state = Set(final_state);
    active.merge_strategy = Set(final_strategy);
    active.merge_commit_sha = Set(final_commit_sha);
    active.auto_merge_enabled = Set(false);
    active.merged_at = Set(final_merged_at);
    active.closed_at = Set(final_closed_at);
    active.updated_at = Set(final_updated_at);
    let merged_pr = pull_request_ops::update(db, active).await?;
    rg_db::ops::pr_event_ops::record(
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
    .await?;

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
        let rebase = git.run_with_env(
            &["rebase", &upstream],
            Some(&worktree),
            &[
                ("GIT_AUTHOR_NAME", "ForgeKeep"),
                ("GIT_AUTHOR_EMAIL", "noreply@forgekeep.local"),
                ("GIT_COMMITTER_NAME", "ForgeKeep"),
                ("GIT_COMMITTER_EMAIL", "noreply@forgekeep.local"),
            ],
        )?;
        if !rebase.success() {
            bail!("rebase merge failed: {}", rebase.stderr_str());
        }

        let target_ref = format!("HEAD:refs/heads/{base_branch}");
        let push = git.run(&["push", "origin", &target_ref], Some(&worktree))?;
        if !push.success() {
            bail!(
                "base branch advanced while rebasing or push failed: {}",
                push.stderr_str()
            );
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

/// Set HEAD to point to a branch (equivalent to `git checkout <branch>` in a bare repo).
/// Uses gix to update the HEAD symbolic reference.
#[allow(dead_code)]
fn gix_set_head_to_branch(repo_path: &std::path::Path, branch: &str) -> Result<()> {
    let repo = gix::open(repo_path)
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
    let repo = gix::open(repo_path)
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
    let repo = gix::open(repo_path)
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

/// Look up a branch SHA, distinguishing an absent ref from a failed repository read.
fn try_get_ref_sha(repo_path: &std::path::Path, branch: &str) -> Result<Option<String>> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let ref_str = format!("refs/heads/{}", branch);
    let Some(mut reference) = repo.try_find_reference(ref_str.as_str()).with_context(|| {
        format!(
            "failed to look up {} in repository: {:?}",
            ref_str, repo_path
        )
    })?
    else {
        return Ok(None);
    };
    let id = reference.peel_to_id().with_context(|| {
        format!(
            "failed to resolve {} in repository: {:?}",
            ref_str, repo_path
        )
    })?;
    Ok(Some(id.to_string()))
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
    try_get_ref_sha(repo_path, branch)?.ok_or_else(|| {
        crate::error::conflict(format!(
            "pull request {kind} branch '{branch}' no longer exists"
        ))
    })?;
    Ok(())
}

/// Resolve a branch reference to its SHA using gix.
fn get_ref_sha(repo_path: &std::path::Path, branch: &str) -> Result<String> {
    try_get_ref_sha(repo_path, branch)?.ok_or_else(|| {
        anyhow::anyhow!(
            "failed to resolve refs/heads/{}: reference does not exist",
            branch
        )
    })
}

// ── Gix merge helpers ───────────────────────────────────────────────────

/// Delete a reference using gix (replaces `git update-ref -d <ref>`).
fn gix_delete_ref(repo_path: &std::path::Path, ref_name: &str) -> Result<()> {
    let repo = gix::open(repo_path)
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
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let our_commit = repo
        .rev_parse_single("HEAD")
        .map_err(|e| anyhow::anyhow!("failed to resolve HEAD: {}", e))?;
    let their_commit = repo
        .rev_parse_single(head_ref)
        .with_context(|| format!("failed to resolve merge ref '{}'", head_ref))?;

    let (merged_tree_id, _conflicts) =
        gix_merge_commits_to_tree(&repo, our_commit, their_commit, head_ref)?;

    // Create merge commit (two parents)
    let commit_id = repo
        .commit(
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
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let our_commit = repo
        .rev_parse_single("HEAD")
        .map_err(|e| anyhow::anyhow!("failed to resolve HEAD: {}", e))?;
    let their_commit = repo
        .rev_parse_single(head_ref)
        .with_context(|| format!("failed to resolve merge ref '{}'", head_ref))?;

    let (merged_tree_id, _conflicts) =
        gix_merge_commits_to_tree(&repo, our_commit, their_commit, head_ref)?;

    // Squash merge: single-parent commit
    let commit_id = repo
        .commit(
            "HEAD",
            message,
            merged_tree_id.detach(),
            [our_commit.detach()],
        )
        .map_err(|e| anyhow::anyhow!("failed to create squash commit: {}", e))?;

    Ok(commit_id.detach().to_string())
}

/// Core merge logic: merge two commits and return the merged tree id + conflicts.
fn gix_merge_commits_to_tree<'repo>(
    repo: &'repo gix::Repository,
    our_commit: gix::Id<'repo>,
    their_commit: gix::Id<'repo>,
    their_label: &str,
) -> Result<(gix::Id<'repo>, Vec<gix::merge::tree::Conflict>)> {
    use gix::merge::blob::builtin_driver::text::Labels;

    let labels = Labels {
        current: Some("HEAD".into()),
        other: Some(their_label.into()),
        ancestor: None, // auto-determined from merge-base
    };

    let options: gix::merge::commit::Options = repo
        .tree_merge_options()
        .map_err(|e| anyhow::anyhow!("failed to get tree merge options: {}", e))?
        .into();

    let mut outcome = repo
        .merge_commits(our_commit, their_commit, labels, options)
        .map_err(|e| anyhow::anyhow!("merge failed: {}", e))?;

    // Check for unresolved conflicts
    let conflicts = outcome.tree_merge.conflicts;
    if !conflicts.is_empty() {
        tracing::warn!("merge has {} conflict(s)", conflicts.len());
        return Err(crate::error::conflict(format!(
            "merge conflict detected: {} files with conflicts",
            conflicts.len()
        )));
    }

    // Write the merged tree to the object database
    let tree_id = outcome
        .tree_merge
        .tree
        .write()
        .map_err(|e| anyhow::anyhow!("failed to write merged tree: {}", e))?;

    Ok((tree_id, conflicts))
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
