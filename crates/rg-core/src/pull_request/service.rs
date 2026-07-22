//! Pull request service — PR creation, diff, merge strategies.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{DatabaseConnection, EntityTrait, Set};
use std::collections::HashMap;

use rg_db::entities::pull_request::{self, Model as PullRequest};
use rg_db::entities::repository as repo_entity;
use rg_db::ops::{pull_request_ops, repo_ops, user_ops};

// ── PR CRUD ─────────────────────────────────────────────────────────────

/// Create a new pull request.
///
/// If `head_repo_id` is provided, this is a fork PR (cross-repository).
/// The `head_branch` should contain just the branch name (not `owner:branch` format).
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
) -> Result<PullRequest> {
    if title.trim().is_empty() {
        bail!("PR title cannot be empty");
    }
    if head_branch == base_branch {
        bail!("head and base branches cannot be the same");
    }

    let number = pull_request_ops::next_number(db, repo_id).await?;

    let target_repo = repo_entity::Entity::find_by_id(repo_id)
        .one(db)
        .await?
        .context("target repository not found")?;
    let target_namespace = repository_namespace(db, &target_repo).await?;
    let target_path = repo_root.join(format!("{target_namespace}/{}.git", target_repo.name));

    // Resolve head SHA (for same-repo PRs, look up branch; for fork PRs, use the head repo)
    let head_sha = if let Some(head_repo_id) = head_repo_id {
        // For fork PRs, resolve from the fork repo's git data
        let head_repo = repo_entity::Entity::find_by_id(head_repo_id)
            .one(db)
            .await?
            .context("head repository not found")?;
        let head_namespace = repository_namespace(db, &head_repo).await?;
        let head_path = repo_root.join(format!("{head_namespace}/{}.git", head_repo.name));
        if head_path.exists() {
            get_ref_sha(&head_path, &head_branch).ok()
        } else {
            None
        }
    } else if target_path.exists() {
        get_ref_sha(&target_path, &head_branch).ok()
    } else {
        None
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
        head_sha: Set(head_sha),
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
        tracing::warn!("Failed to trigger PR opened webhook: {e}");
    }

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
        let head_owner_user = user_ops::find_by_username(db, head_owner)
            .await?
            .with_context(|| format!("head owner '{}' not found", head_owner))?;

        // Find the target repo to compare
        let target_repo = repo_entity::Entity::find_by_id(target_repo_id)
            .one(db)
            .await?
            .context("target repository not found")?;

        if head_owner_user.id != target_repo.owner_id {
            // Different owner — this is a fork PR
            // Find the fork repo by the head owner (user may have forked the same repo)
            let fork_repo =
                repo_ops::find_by_owner_and_name(db, head_owner_user.id, &target_repo.name)
                    .await?
                    .with_context(|| {
                        format!(
                            "no repository '{}/{}' found for head owner",
                            head_owner, target_repo.name
                        )
                    })?;

            // Verify it's actually a fork of the target
            if fork_repo.origin_repo_id != Some(target_repo_id) && fork_repo.id != target_repo_id {
                bail!(
                    "'{}/{}' is not a fork of the target repository",
                    head_owner,
                    target_repo.name
                );
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

/// Notify watchers of a PR event.
pub async fn notify_watchers_pr(
    db: &DatabaseConnection,
    repo_id: i64,
    repo_name: &str,
    author_name: &str,
    pr_number: i64,
    pr_title: &str,
    action: &str,
) -> Result<()> {
    crate::notification::notify_watchers(
        db,
        repo_id,
        author_name,
        &format!("PR #{} {} in {}", pr_number, action, repo_name),
        "pull_request",
        Some(format!("{} {}: {}", author_name, action, pr_title)),
    )
    .await
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
pub async fn get_pr(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    number: i64,
) -> Result<PullRequest> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    pull_request_ops::find_by_repo_and_number(db, repo.id, number)
        .await?
        .context("pull request not found")
}

/// Update PR metadata (title, body, state).
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
) -> Result<PullRequest> {
    let mut pr = get_pr(db, owner, repo_name, number).await?;
    let previous_state = pr.state.clone();
    let previous_draft = pr.is_draft;

    if let Some(t) = title {
        if t.trim().is_empty() {
            bail!("PR title cannot be empty");
        }
        pr.title = t;
    }
    if let Some(b) = body {
        pr.body = Some(b);
    }
    if let Some(draft) = is_draft {
        if pr.state != "open" {
            bail!("only an open pull request can change draft status");
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
                        tracing::warn!("Failed to trigger PR closed webhook: {e}");
                    }
                }
            }
            _ => bail!("invalid PR state: {}", s),
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

        if head_repo_path.exists() {
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

            if !fetch_output.success() {
                tracing::warn!(
                    "fetch of fork branch failed (non-fatal): {}",
                    String::from_utf8_lossy(&fetch_output.stderr)
                );
            }

            // Compute diff inside spawn_blocking (CPU-intensive gix tree-diff)
            let base_path = base_repo_path.clone();
            let pr_clone = pr.clone();
            let local_ref = local_ref.clone();
            return tokio::task::spawn_blocking(move || {
                compute_cross_repo_diff(&base_path, &pr_clone.base_branch, &local_ref, &pr_clone)
            })
            .await?;
        }
    }

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
/// On failure (e.g. missing refs), falls back with a generic error — the caller
/// should handle gracefully.
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
                    let location = change.location().to_str_lossy().to_string();

                    let (additions, deletions) = change
                        .diff(&mut resource_cache)
                        .ok()
                        .and_then(|mut p| p.line_counts().ok().flatten())
                        .map(|c| (c.insertions as i64, c.removals as i64))
                        .unwrap_or((0, 0));

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
            .map_err(|e| anyhow::anyhow!("tree-diff failed: {e}"))?;

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
    if pr.state != "open" {
        bail!("auto-merge can only be enabled for an open pull request");
    }
    if pr.is_draft {
        bail!("auto-merge cannot be enabled for a draft pull request");
    }
    if let Some(entry) = rg_db::ops::merge_queue_ops::find_by_pr(db, pr.id).await? {
        if entry.status == "running" {
            bail!("cannot enable auto-merge while the merge queue is processing this PR");
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
            status: "pending".into(),
            reason: Some(error.to_string()),
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
    let merge = match merge_pr(db, repo_root, owner, repo_name, number, strategy).await {
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
            Err(error) => tracing::warn!(pr_id = pr.id, %error, "automatic merge attempt failed"),
        }
    }
    Ok(outcomes)
}

/// Result of a merge operation.
#[derive(Debug, serde::Serialize)]
pub struct MergeResult {
    pub merge_commit_sha: String,
    pub strategy: String,
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

    if pr.state != "open" {
        bail!(
            "cannot merge a PR that is not in 'open' state (current: {})",
            pr.state
        );
    }
    if pr.is_draft {
        bail!("draft pull requests cannot be merged");
    }

    if !pull_request_ops::claim_merge(db, pr.id).await? {
        bail!("another merge attempt is already in progress");
    }

    let result = merge_claimed_pr(db, repo_root, owner, repo_name, pr.clone(), strategy).await;
    if result.is_err() {
        if let Err(error) = pull_request_ops::restore_merge_claim(db, pr.id).await {
            tracing::error!(pr_id = pr.id, %error, "failed to restore PR merge state");
        }
    }
    result
}

async fn merge_claimed_pr(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    owner: &str,
    repo_name: &str,
    pr: PullRequest,
    strategy: MergeStrategy,
) -> Result<MergeResult> {
    let repo_path = repo_root.join(format!("{}/{}.git", owner, repo_name));
    if !repo_path.exists() {
        bail!("repository path does not exist: {:?}", repo_path);
    }

    // For fork PRs, fetch head branch into target repo
    if let Some(head_repo_id) = pr.head_repo_id {
        let head_repo = repo_entity::Entity::find_by_id(head_repo_id)
            .one(db)
            .await?
            .context("head repository not found")?;
        let head_namespace = repository_namespace(db, &head_repo).await?;
        let head_repo_path = repo_root.join(format!("{}/{}.git", head_namespace, head_repo.name));

        if head_repo_path.exists() {
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

            return update_pr_merged(db, pr, merge_commit_sha, strategy).await;
        }
    }

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

    update_pr_merged(db, pr, merge_commit_sha, strategy).await
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
async fn update_pr_merged(
    db: &DatabaseConnection,
    mut pr: PullRequest,
    merge_commit_sha: String,
    strategy: MergeStrategy,
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
        tracing::warn!("Failed to trigger PR merged webhook: {e}");
    }

    Ok(MergeResult {
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
    let worktree = std::env::temp_dir().join(format!("ironforge-rebase-{}", uuid::Uuid::new_v4()));
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
                ("GIT_AUTHOR_NAME", "IronForge"),
                ("GIT_AUTHOR_EMAIL", "noreply@ironforge.local"),
                ("GIT_COMMITTER_NAME", "IronForge"),
                ("GIT_COMMITTER_EMAIL", "noreply@ironforge.local"),
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

/// Resolve a branch reference to its SHA using gix.
fn get_ref_sha(repo_path: &std::path::Path, branch: &str) -> Result<String> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let ref_str = format!("refs/heads/{}", branch);
    let id = repo
        .rev_parse_single(ref_str.as_str())
        .map_err(|e| anyhow::anyhow!("failed to resolve {}: {}", ref_str, e))?;
    Ok(id.to_string())
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
        bail!(
            "merge conflict detected: {} files with conflicts",
            conflicts.len()
        );
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
    let user = user_ops::find_by_username(db, owner)
        .await?
        .context("owner not found")?;
    repo_ops::find_by_owner_and_name(db, user.id, repo_name)
        .await?
        .context("repository not found")
}
