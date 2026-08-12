//! Repository-scoped FIFO merge queue.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use sea_orm::{DatabaseConnection, EntityTrait, Set};

use rg_db::entities::{merge_queue_entry, pipeline, pull_request, repository};
use rg_db::ops::{merge_queue_ops, pull_request_ops};

use super::ci::PipelineCi;
use super::service::{self, MergeStrategy};

#[derive(Debug, serde::Serialize)]
pub struct MergeQueueProcessResult {
    pub merged: Vec<i64>,
    pub failed: Vec<i64>,
    pub waiting_reason: Option<String>,
    /// The base-branch moves this run produced, one per merge, for the caller's
    /// post-push hooks — see [`super::service::MergeResult::base_ref_update`].
    /// `#[serde(skip)]`: this is plumbing, not part of the queue's API payload.
    #[serde(skip)]
    pub merged_ref_updates: Vec<service::MergedRef>,
}

pub async fn enqueue(
    db: &DatabaseConnection,
    repository: &repository::Model,
    pr: &pull_request::Model,
    actor_id: i64,
    strategy: MergeStrategy,
) -> Result<merge_queue_entry::Model> {
    if pr.repo_id != repository.id {
        bail!("pull request does not belong to this repository");
    }
    if pr.state != "open" || pr.is_draft {
        bail!("only an open, non-draft pull request can enter the merge queue");
    }

    // Queue ordering owns merge execution once a PR is enqueued.
    if pr.auto_merge_enabled {
        let mut active: pull_request::ActiveModel = pr.clone().into();
        active.auto_merge_enabled = Set(false);
        active.auto_merge_strategy = Set(None);
        active.auto_merge_enabled_by_id = Set(None);
        active.auto_merge_enabled_at = Set(None);
        active.updated_at = Set(Utc::now());
        pull_request_ops::update(db, active).await?;
        rg_db::ops::pr_event_ops::record(
            db,
            pr.repo_id,
            pr.id,
            Some(actor_id),
            "auto_merge_disabled",
            None,
            serde_json::json!({"reason": "merge_queue_enqueued"}),
        )
        .await?;
    }
    let entry =
        merge_queue_ops::enqueue(db, repository.id, pr.id, actor_id, strategy.as_str()).await?;
    rg_db::ops::pr_event_ops::record(
        db,
        pr.repo_id,
        pr.id,
        Some(actor_id),
        "merge_queue_enqueued",
        None,
        serde_json::json!({"entry_id": entry.id, "strategy": entry.strategy}),
    )
    .await?;
    Ok(entry)
}

/// What a cancellation request found when it got there.
///
/// `merge_queue_ops::cancel` only moves an entry that is still `queued` — by
/// design, since a worker that already claimed the entry is mid-merge and
/// nothing here can call that back. But its `bool` collapsed two very different
/// refusals into one: "there is nothing of yours in this queue" and "there is,
/// and it is being merged right now". The caller answered `404 pull request is
/// not queued` to both, which tells the second caller the opposite of the truth
/// — its PR is in the queue, further along than it thought.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The entry was `queued` and is now `canceled`.
    Canceled,
    /// No entry for this PR, or one that has already finished — nothing to
    /// cancel, and nothing the caller can do about it.
    NotQueued,
    /// The entry exists but a worker holds it: the merge is under way and the
    /// window for cancelling it has closed. A later state change (the merge
    /// finishing or failing) is what unblocks the caller, so this is a conflict,
    /// not a missing resource.
    AlreadyMerging,
}

pub async fn cancel(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    pr: &pull_request::Model,
    actor_id: i64,
) -> Result<CancelOutcome> {
    if let Some(canceled_entry) = merge_queue_ops::cancel(db, pr.id).await? {
        rg_db::ops::pr_event_ops::record(
            db,
            pr.repo_id,
            pr.id,
            Some(actor_id),
            "merge_queue_canceled",
            None,
            serde_json::json!({}),
        )
        .await?;
        // `merge_queue_ops::cancel` returns the exact attempt from the same
        // writer transaction as the status change. A concurrent re-enqueue can
        // therefore clear its row only after we have retained the old pipeline
        // and ref identity for cleanup.
        release_merge_group_pipeline(db, &canceled_entry, "the queue entry was canceled").await;
        cleanup_merge_group_ref(db, repo_root, repository, &canceled_entry, None).await;
        return Ok(CancelOutcome::Canceled);
    }

    // The conditional write refused. Reading the row afterwards is what tells
    // the two refusals apart — and both readings are honest whichever way the
    // race went: an entry claimed a moment ago really is being merged, and an
    // entry that finished a moment ago really is no longer cancellable.
    let outcome = match merge_queue_ops::find_by_pr(db, pr.id).await? {
        Some(entry) if entry.status == "running" => CancelOutcome::AlreadyMerging,
        _ => CancelOutcome::NotQueued,
    };
    Ok(outcome)
}

async fn finish_entry(
    db: &DatabaseConnection,
    repo_root: &Path,
    entry: &merge_queue_entry::Model,
    status: &str,
    failure_reason: Option<String>,
) -> Result<bool> {
    if !merge_queue_ops::finish(
        db,
        entry.id,
        entry.attempt_number,
        status,
        failure_reason.clone(),
    )
    .await?
    {
        return Ok(false);
    }
    // Reached from every terminal path, including the ones CI had nothing to do
    // with. When the pipeline is why the entry finished it is already terminal
    // and this changes nothing.
    release_merge_group_pipeline(db, entry, &format!("the queue entry finished as {status}")).await;
    rg_db::ops::pr_event_ops::record(
        db,
        entry.repo_id,
        entry.pr_id,
        None,
        &format!("merge_queue_{status}"),
        failure_reason,
        serde_json::json!({"entry_id": entry.id, "strategy": entry.strategy}),
    )
    .await?;
    match repository::Entity::find_by_id(entry.repo_id).one(db).await {
        Ok(Some(repository)) => {
            cleanup_merge_group_ref(db, repo_root, &repository, entry, None).await;
        }
        Ok(None) => tracing::warn!(
            entry_id = entry.id,
            pr_id = entry.pr_id,
            repo_id = entry.repo_id,
            "{STALE_REF}: the queue entry's repository row is gone"
        ),
        Err(error) => tracing::warn!(
            entry_id = entry.id,
            pr_id = entry.pr_id,
            repo_id = entry.repo_id,
            error = %format!("{error:#}"),
            "{STALE_REF}: the queue entry's repository could not be read"
        ),
    }
    Ok(true)
}

/// Opening of every log line the cleanup below emits, so an operator can grep
/// one phrase for the whole family instead of four different wordings.
const STALE_REF: &str = "merge-group ref left behind";

/// Cancel the merge-group pipeline an entry has stopped waiting for.
///
/// A merge-group pipeline exists to answer one question — "does this PR merge
/// cleanly into this base?" — for one group commit. Three things end that
/// question without ending the pipeline: the entry is cancelled, the entry
/// finishes for a reason that is not CI (a merge conflict, a lost worker lease,
/// a closed PR), and the group commit is rebuilt because the PR's head moved.
/// Left running, its jobs are handed to real runners and burn real minutes on a
/// group nobody will merge — and for a repository that declares `concurrency:`
/// without `cancel_in_progress`, the stale run makes every later queue pass fail
/// outright, because the group ref is stable and still has an active pipeline on
/// it (card_13d8ebde295b).
///
/// Best-effort, like the ref cleanup beside it: the state change the caller was
/// told about is already committed, and a pipeline that will not cancel must not
/// unwind it. Best-effort is not silent — a failure leaves a live pipeline, and
/// nothing else in the system will come back for it.
///
/// `cancel_pipeline_chain` is itself a no-op on a pipeline that already reached
/// a terminal status, so the common paths (the queue merged, or CI failed and
/// that is why the entry finished) cost one read and change nothing.
async fn release_merge_group_pipeline(
    db: &DatabaseConnection,
    entry: &merge_queue_entry::Model,
    reason: &str,
) {
    let Some(pipeline_id) = entry.merge_group_pipeline_id else {
        return;
    };
    cancel_merge_group_pipeline(db, entry.id, entry.pr_id, pipeline_id, reason).await;
}

async fn cancel_merge_group_pipeline(
    db: &DatabaseConnection,
    entry_id: i64,
    pr_id: i64,
    pipeline_id: i64,
    reason: &str,
) {
    match rg_db::ops::pipeline_ops::cancel_pipeline_chain(db, pipeline_id).await {
        Ok(true) => tracing::info!(
            entry_id,
            pr_id,
            pipeline_id,
            reason,
            "canceled the merge-group pipeline its queue entry no longer waits for"
        ),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            entry_id,
            pr_id,
            pipeline_id,
            reason,
            error = %format!("{error:#}"),
            "merge-group pipeline left running: its queue entry no longer waits for it and nothing else will"
        ),
    }
}

/// Delete the merge-group ref the entry created, best-effort.
///
/// Best-effort is the right contract: `cancel` and `finish_entry` have already
/// committed the state change the caller was told about, and a ref that will not
/// go away must not unwind it. Best-effort is *not* the same as silent, though —
/// every failure below leaves a live `refs/merge-queue/{entry.id}` on disk, and
/// with nothing logged that accumulation is indistinguishable from a clean run
/// (card_75646d7b017b). Each failure therefore names the entry, the PR, the
/// repository and the full cause chain.
async fn cleanup_merge_group_ref(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    entry: &merge_queue_entry::Model,
    expected_sha: Option<&str>,
) {
    // No gate on `merge_group_sha`. That field used to mean "a merge-group ref
    // exists", because `update-ref` and `set_merge_group` were written next to
    // each other. `ensure_merge_group_ci` now creates the ref and then returns
    // `Ready` for a repository with no CI config — several statements before
    // `set_merge_group` — so the field stopped meaning that and the gate stopped
    // matching reality: the ref was written on every queue pass and deleted on
    // none, one per queue entry, forever (card_884d819fc51b). The partial-write
    // path of card_55282a865b8e lands in the same gap, with the ref created and
    // the row not owning it.
    //
    // The ref itself remains the source of truth. Cleanup observes its commit
    // and deletes with Git's old-value guard, so a missing ref is a no-op and a
    // recycled entry's newer attempt is never removed by stale cleanup.
    let namespace = match service::repository_namespace(db, repository).await {
        Ok(namespace) => namespace,
        Err(error) => {
            tracing::warn!(
                entry_id = entry.id,
                pr_id = entry.pr_id,
                repo_id = repository.id,
                repo = %repository.name,
                error = %format!("{error:#}"),
                "{STALE_REF}: the repository namespace could not be resolved"
            );
            return;
        }
    };
    let repo_path = repo_root.join(format!("{namespace}/{}.git", repository.name));
    let group_ref = format!("refs/merge-queue/{}", entry.id);
    let git = match rg_git::cli_gateway::global_gateway().as_ref() {
        Ok(git) => git,
        Err(error) => {
            tracing::warn!(
                entry_id = entry.id,
                pr_id = entry.pr_id,
                repo_id = repository.id,
                repo = %repository.name,
                git_ref = %group_ref,
                error = %format!("{error:#}"),
                "{STALE_REF}: the git gateway is unavailable"
            );
            return;
        }
    };
    // A recycled queue row keeps its primary key, and therefore its ref name.
    // Delete by the old commit id so cleanup for attempt N cannot remove the
    // ref that attempt N+1 has already published. When the row never recorded a
    // SHA (the partial-publication path), observe the ref first and then verify
    // that the database still describes this attempt before using that SHA as
    // the compare-and-delete token.
    let expected_sha = match expected_sha.or(entry.merge_group_sha.as_deref()) {
        Some(expected_sha) => expected_sha.to_string(),
        None => {
            let observed = match git.run(
                &["rev-parse", "--verify", "--quiet", &group_ref],
                Some(&repo_path),
            ) {
                Ok(output) if output.success() => output.stdout_str().trim().to_string(),
                Ok(output) if output.status.code() == Some(1) => return,
                Ok(output) => {
                    tracing::warn!(
                        entry_id = entry.id,
                        pr_id = entry.pr_id,
                        repo_id = repository.id,
                        repo = %repository.name,
                        git_ref = %group_ref,
                        exit_code = ?output.status.code(),
                        stderr = %output.stderr_str().trim(),
                        "{STALE_REF}: its current commit could not be resolved"
                    );
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        entry_id = entry.id,
                        pr_id = entry.pr_id,
                        repo_id = repository.id,
                        repo = %repository.name,
                        git_ref = %group_ref,
                        error = %format!("{error:#}"),
                        "{STALE_REF}: git rev-parse could not be run"
                    );
                    return;
                }
            };
            let still_same_attempt = match merge_queue_ops::find_by_pr(db, entry.pr_id).await {
                Ok(Some(current)) => {
                    current.id == entry.id && current.attempt_number == entry.attempt_number
                }
                Ok(None) => false,
                Err(error) => {
                    tracing::warn!(
                        entry_id = entry.id,
                        pr_id = entry.pr_id,
                        repo_id = repository.id,
                        repo = %repository.name,
                        error = %format!("{error:#}"),
                        "{STALE_REF}: its queue attempt could not be verified"
                    );
                    return;
                }
            };
            if !still_same_attempt {
                return;
            }
            observed
        }
    };
    match git.run(
        &["update-ref", "-d", &group_ref, &expected_sha],
        Some(&repo_path),
    ) {
        Ok(output) if output.success() => {}
        Ok(output) => {
            // A different SHA is a newer attempt, not a failed cleanup. Re-read
            // only to classify the compare-and-delete refusal; the deletion
            // itself remains the single atomic Git operation.
            let current = git.run(
                &["rev-parse", "--verify", "--quiet", &group_ref],
                Some(&repo_path),
            );
            match current {
                Ok(current) if current.status.code() == Some(1) => {}
                Ok(current) if current.success() && current.stdout_str().trim() != expected_sha => {
                    tracing::debug!(
                        entry_id = entry.id,
                        pr_id = entry.pr_id,
                        git_ref = %group_ref,
                        "left a newer merge-group attempt's ref untouched"
                    );
                }
                _ => tracing::warn!(
                    entry_id = entry.id,
                    pr_id = entry.pr_id,
                    repo_id = repository.id,
                    repo = %repository.name,
                    git_ref = %group_ref,
                    exit_code = ?output.status.code(),
                    stderr = %output.stderr_str().trim(),
                    "{STALE_REF}: git update-ref refused to delete it"
                ),
            }
        }
        Err(error) => tracing::warn!(
            entry_id = entry.id,
            pr_id = entry.pr_id,
            repo_id = repository.id,
            repo = %repository.name,
            git_ref = %group_ref,
            error = %format!("{error:#}"),
            "{STALE_REF}: git update-ref could not be run"
        ),
    }
}

async fn queue_attempt_is_current(
    db: &DatabaseConnection,
    entry: &merge_queue_entry::Model,
) -> Result<bool> {
    Ok(matches!(
        merge_queue_ops::find_by_pr(db, entry.pr_id).await?,
        Some(current)
            if current.id == entry.id
                && current.attempt_number == entry.attempt_number
                && matches!(current.status.as_str(), "queued" | "running")
    ))
}

/// Compensate a pipeline that lost the conditional ownership write. Both
/// resources are addressed by values produced by this invocation, not by the
/// recycled queue row's current contents, so a later attempt remains intact.
async fn retire_unowned_merge_group_pipeline(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    entry: &merge_queue_entry::Model,
    pipeline_id: i64,
    group_sha: &str,
    reason: &str,
) {
    cancel_merge_group_pipeline(db, entry.id, entry.pr_id, pipeline_id, reason).await;
    cleanup_merge_group_ref(db, repo_root, repository, entry, Some(group_sha)).await;
}

/// A same-attempt producer that loses pipeline ownership must retire only its
/// graph. Both producers use the same stable ref name, and normally the same
/// deterministic group SHA, so the generic stale-attempt cleanup above would
/// delete the winner's ref along with the loser's graph.
///
/// If the base or head moved between the two snapshots, put the ref back on the
/// winner with Git's old-value guard. A later attempt may already have replaced
/// it; in that case the compare-and-swap deliberately leaves the newer ref
/// alone.
async fn retire_losing_merge_group_pipeline(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    entry: &merge_queue_entry::Model,
    winner: &merge_queue_entry::Model,
    pipeline_id: i64,
    losing_group_sha: &str,
) {
    cancel_merge_group_pipeline(
        db,
        entry.id,
        entry.pr_id,
        pipeline_id,
        "another producer of the same queue attempt published its pipeline first",
    )
    .await;

    let Some(winner_group_sha) = winner.merge_group_sha.as_deref() else {
        tracing::warn!(
            entry_id = entry.id,
            attempt_number = entry.attempt_number,
            pipeline_id,
            "the losing merge-group pipeline was retired, but the winning queue row has no group SHA to restore its synthetic ref"
        );
        return;
    };
    if winner_group_sha == losing_group_sha {
        return;
    }

    let namespace = match service::repository_namespace(db, repository).await {
        Ok(namespace) => namespace,
        Err(error) => {
            tracing::warn!(
                entry_id = entry.id,
                attempt_number = entry.attempt_number,
                pipeline_id,
                error = %format!("{error:#}"),
                "the losing merge-group pipeline was retired, but the winner's repository namespace could not be resolved"
            );
            return;
        }
    };
    let repo_path = repo_root.join(format!("{namespace}/{}.git", repository.name));
    let group_ref = format!("refs/merge-queue/{}", entry.id);
    let git = match rg_git::cli_gateway::global_gateway().as_ref() {
        Ok(git) => git,
        Err(error) => {
            tracing::warn!(
                entry_id = entry.id,
                attempt_number = entry.attempt_number,
                pipeline_id,
                git_ref = %group_ref,
                error = %format!("{error:#}"),
                "the losing merge-group pipeline was retired, but the winner's synthetic ref could not be restored"
            );
            return;
        }
    };
    match git.run(
        &["update-ref", &group_ref, winner_group_sha, losing_group_sha],
        Some(&repo_path),
    ) {
        Ok(output) if output.success() => {}
        Ok(output) => {
            let current = git.run(
                &["rev-parse", "--verify", "--quiet", &group_ref],
                Some(&repo_path),
            );
            match current {
                Ok(current)
                    if current.success() && current.stdout_str().trim() == winner_group_sha => {}
                Ok(current)
                    if current.status.code() == Some(1)
                        || (current.success()
                            && current.stdout_str().trim() != losing_group_sha) =>
                {
                    tracing::debug!(
                        entry_id = entry.id,
                        attempt_number = entry.attempt_number,
                        git_ref = %group_ref,
                        "left a newer merge-group attempt's ref untouched after a publication race"
                    );
                }
                _ => tracing::warn!(
                    entry_id = entry.id,
                    attempt_number = entry.attempt_number,
                    pipeline_id,
                    git_ref = %group_ref,
                    winner_group_sha,
                    losing_group_sha,
                    exit_code = ?output.status.code(),
                    stderr = %output.stderr_str().trim(),
                    "the losing merge-group pipeline was retired, but Git refused to restore the winner's synthetic ref"
                ),
            }
        }
        Err(error) => tracing::warn!(
            entry_id = entry.id,
            attempt_number = entry.attempt_number,
            pipeline_id,
            git_ref = %group_ref,
            winner_group_sha,
            losing_group_sha,
            error = %format!("{error:#}"),
            "the losing merge-group pipeline was retired, but Git could not restore the winner's synthetic ref"
        ),
    }
}

pub async fn process_repository_with_ci(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    ci: &PipelineCi<'_>,
) -> Result<MergeQueueProcessResult> {
    process_repository_inner(db, repo_root, repository, ci).await
}

async fn process_repository_inner(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    ci: &PipelineCi<'_>,
) -> Result<MergeQueueProcessResult> {
    let namespace = service::repository_namespace(db, repository).await?;
    let mut result = MergeQueueProcessResult {
        merged: Vec::new(),
        failed: Vec::new(),
        waiting_reason: None,
        merged_ref_updates: Vec::new(),
    };

    loop {
        let Some(entry) = merge_queue_ops::list_by_repo(db, repository.id)
            .await?
            .into_iter()
            .next()
        else {
            break;
        };

        if entry.status == "running" {
            let stale = entry
                .started_at
                .is_some_and(|started| started < Utc::now() - Duration::minutes(30));
            if stale {
                if finish_entry(
                    db,
                    repo_root,
                    &entry,
                    "failed",
                    Some("merge-queue worker lease expired".into()),
                )
                .await?
                {
                    result.failed.push(entry.pr_id);
                }
                continue;
            }
            result.waiting_reason =
                Some("merge-queue worker is already processing the head".into());
            break;
        }

        let Some(pr) = pull_request::Entity::find_by_id(entry.pr_id)
            .one(db)
            .await?
        else {
            if finish_entry(
                db,
                repo_root,
                &entry,
                "failed",
                Some("pull request not found".into()),
            )
            .await?
            {
                result.failed.push(entry.pr_id);
            }
            continue;
        };
        if pr.state == "merged" {
            if finish_entry(db, repo_root, &entry, "merged", None).await? {
                result.merged.push(pr.id);
            }
            continue;
        }
        if pr.state != "open" || pr.is_draft {
            if finish_entry(
                db,
                repo_root,
                &entry,
                "failed",
                Some(format!("pull request is {} or draft", pr.state)),
            )
            .await?
            {
                result.failed.push(pr.id);
            }
            continue;
        }
        if let Err(error) = crate::branch_protection::service::check_merge_allowed(
            db,
            repository.id,
            &pr.base_branch,
            pr.id,
        )
        .await
        {
            // `{:#}` and not `to_string()`: this reason is handed to the user as
            // the answer to "why is my PR stuck", and the inner cause is the
            // half that actually answers it (card_a997f30c142c).
            result.waiting_reason = Some(format!("{error:#}"));
            break;
        }
        match ensure_merge_group_ci(db, repo_root, repository, &entry, &pr, ci).await? {
            MergeGroupState::Ready => {}
            MergeGroupState::Waiting(reason) => {
                result.waiting_reason = Some(reason);
                break;
            }
            MergeGroupState::Failed => {
                result.failed.push(pr.id);
                continue;
            }
            MergeGroupState::Abandoned => continue,
        }
        if !merge_queue_ops::claim(db, entry.id, entry.attempt_number).await? {
            result.waiting_reason = Some("merge-queue head was claimed concurrently".into());
            break;
        }

        let strategy = MergeStrategy::parse(&entry.strategy)?;
        // The queue worker is a background loop, not a request path: the merge
        // announcement goes to the process-global delivery tracker.
        match service::merge_pr(
            db,
            repo_root,
            &namespace,
            &repository.name,
            pr.number,
            strategy,
            None,
        )
        .await
        {
            Ok(merge) => {
                if finish_entry(db, repo_root, &entry, "merged", None).await? {
                    result.merged.push(pr.id);
                    // The queue moved the base branch; the hooks for that move
                    // are the caller's to run (card_87c4912c51ed).
                    result.merged_ref_updates.extend(merge.base_ref_update);
                }
            }
            Err(error) => {
                // Persisted into the queue entry and shown in the UI — the
                // flattened chain is all the user ever gets to see.
                if finish_entry(db, repo_root, &entry, "failed", Some(format!("{error:#}"))).await?
                {
                    result.failed.push(pr.id);
                }
            }
        }
    }
    Ok(result)
}

enum MergeGroupState {
    Ready,
    Waiting(String),
    Failed,
    /// The row still exists, but this worker belongs to an older enqueue
    /// attempt. Its published resources have been compensated, so the queue
    /// loop should look at the current head rather than report a false wait.
    Abandoned,
}

async fn ensure_merge_group_ci(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    entry: &merge_queue_entry::Model,
    pr: &pull_request::Model,
    ci: &PipelineCi<'_>,
) -> Result<MergeGroupState> {
    let namespace = service::repository_namespace(db, repository).await?;
    let repo_path = repo_root.join(format!("{namespace}/{}.git", repository.name));
    let git = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let base_ref = format!("refs/heads/{}", pr.base_branch);
    let base_output = git.run(&["rev-parse", &base_ref], Some(&repo_path))?;
    base_output.ensure_success()?;
    let base_sha = base_output.stdout_str().trim().to_string();
    let head_sha = pr
        .head_sha
        .clone()
        .context("pull request head SHA is missing")?;

    if let Some(head_repo_id) = pr.head_repo_id {
        let head_repo = repository::Entity::find_by_id(head_repo_id)
            .one(db)
            .await?
            .context("pull request head repository not found")?;
        let head_namespace = service::repository_namespace(db, &head_repo).await?;
        let head_repo_path = repo_root.join(format!("{head_namespace}/{}.git", head_repo.name));
        let fetch = git.run(
            &["fetch", &head_repo_path.to_string_lossy(), &head_sha],
            Some(&repo_path),
        )?;
        fetch.ensure_success()?;
    }

    if entry.merge_group_base_sha.as_deref() == Some(&base_sha)
        && entry.merge_group_head_sha.as_deref() == Some(&head_sha)
    {
        if let Some(pipeline_id) = entry.merge_group_pipeline_id {
            let pipeline = rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id)
                .await?
                .context("merge-group pipeline not found")?;
            return merge_group_state(db, repo_root, entry, &pipeline).await;
        }
    }

    let tree_output = git.run(
        &["merge-tree", "--write-tree", &base_sha, &head_sha],
        Some(&repo_path),
    )?;
    if !tree_output.success() {
        if !finish_entry(
            db,
            repo_root,
            entry,
            "failed",
            Some(format!(
                "merge group conflicts: {}",
                tree_output.stderr_str().trim()
            )),
        )
        .await?
        {
            return Ok(MergeGroupState::Abandoned);
        }
        return Ok(MergeGroupState::Failed);
    }
    let tree_sha = tree_output
        .stdout_str()
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if tree_sha.is_empty() {
        anyhow::bail!("git merge-tree did not return a tree id");
    }
    // The row id is stable across re-enqueues. Include its monotonic attempt in
    // the commit itself so two attempts created within the same wall-clock
    // second still get different group SHAs; compare-and-delete cleanup can
    // then never mistake the newer attempt's ref for the older one.
    let message = format!(
        "Merge queue group for PR #{} (attempt {})",
        pr.number, entry.attempt_number
    );
    // `commit-tree` hashes the author/committer timestamps, so leaving them to
    // "now" gives a different `group_sha` on every pass — and the recovery below
    // is keyed on that SHA being stable. The entry's own `created_at` is the
    // natural pin: constant while the entry is queued. Re-enqueue resets it,
    // while the attempt number in the message above remains the definitive
    // discriminator even if two attempts land in the same second. Git's
    // internal date format (`<unix ts> <offset>`) is used rather than RFC 3339
    // because it is the one form git parses without a locale- or
    // precision-dependent guess.
    let commit_date = format!("{} +0000", entry.created_at.timestamp());
    let commit_output = git.run_with_env(
        &[
            "commit-tree",
            &tree_sha,
            "-p",
            &base_sha,
            "-p",
            &head_sha,
            "-m",
            &message,
        ],
        Some(&repo_path),
        &[
            ("GIT_AUTHOR_NAME", "ForgeKeep Merge Queue"),
            ("GIT_AUTHOR_EMAIL", "merge-queue@forgekeep.local"),
            ("GIT_AUTHOR_DATE", commit_date.as_str()),
            ("GIT_COMMITTER_NAME", "ForgeKeep Merge Queue"),
            ("GIT_COMMITTER_EMAIL", "merge-queue@forgekeep.local"),
            ("GIT_COMMITTER_DATE", commit_date.as_str()),
        ],
    )?;
    commit_output.ensure_success()?;
    let group_sha = commit_output.stdout_str().trim().to_string();
    let group_ref = format!("refs/merge-queue/{}", entry.id);

    // Reaching here with a different group commit than the entry recorded means
    // the PR's head (or its base) moved while it sat in the queue: the pipeline
    // the entry still names was asked about a merge that no longer exists.
    // Release it before the ref moves out from under it — the ref is stable per
    // entry, so a moment later `refs/merge-queue/{id}` points at the new group
    // commit while the old run is still marked active on it.
    let rebuilt = entry.merge_group_sha.as_deref() != Some(group_sha.as_str());
    if rebuilt && entry.merge_group_pipeline_id.is_some() {
        release_merge_group_pipeline(db, entry, "the merge group was rebuilt on a newer head")
            .await;
        // Stop the row naming a pipeline that has just been canceled. If the
        // trigger below fails, the next pass rebuilds from nothing rather than
        // adopting a dead run.
        if !merge_queue_ops::clear_merge_group(db, entry.id, entry.attempt_number).await? {
            return Ok(MergeGroupState::Abandoned);
        }
    }

    git.run(&["update-ref", &group_ref, &group_sha], Some(&repo_path))?
        .ensure_success()?;

    if !ci.trigger.has_ci_config(&repo_path, &group_sha) {
        if !queue_attempt_is_current(db, entry).await? {
            cleanup_merge_group_ref(db, repo_root, repository, entry, Some(&group_sha)).await;
            return Ok(MergeGroupState::Abandoned);
        }
        return Ok(MergeGroupState::Ready);
    }
    // The pipeline is created before the row that owns it, so the two can
    // disagree: when `set_merge_group` below fails, the entry keeps no trace of
    // a pipeline that is already running, and the pass after it used to build
    // another merge group and trigger another pipeline — one more per tick,
    // forever, because the merge condition reads a `merge_group_pipeline_id`
    // that never got written. The group SHA is deterministic, so that pipeline
    // is still findable: adopt it instead of triggering a second one
    // (card_55282a865b8e).
    let pipeline = match rg_db::ops::pipeline_ops::find_merge_group_pipeline(
        db,
        repository.id,
        &group_sha,
    )
    .await?
    {
        Some(existing) => {
            tracing::warn!(
                entry_id = entry.id,
                pipeline_id = existing.id,
                "adopting the merge-group pipeline already triggered for this group commit: the queue entry lost the row that owned it"
            );
            existing
        }
        None => {
            let pipeline_id = ci
                .trigger
                .trigger_pipeline(crate::ci::TriggerPipelineParams {
                    db,
                    repo_path: &repo_path,
                    repo_id: repository.id,
                    commit_sha: &group_sha,
                    ref_name: &group_ref,
                    trigger_type: "merge_group",
                    // `merge_group` shares the `on: pull_request` filter, and the
                    // ref above is the synthetic group ref — the branch the filter
                    // is about is the one the queue is merging into.
                    base_branch: Some(&pr.base_branch),
                    // The group commit is built fresh for this run; its first
                    // parent is the base branch tip, which is exactly the diff a
                    // `paths:` filter is asking about.
                    previous_sha: None,
                    triggered_by: Some(entry.enqueued_by_id),
                    docker_enabled: ci.docker_enabled,
                    external_runners: ci.external_runners,
                    allow_host_runner: ci.allow_host_runner,
                    jwt_secret: ci.jwt_secret,
                    encryption_key: ci.encryption_key,
                    external_url: ci.external_url,
                })
                .await?;
            rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id)
                .await?
                .context("merge-group pipeline vanished right after it was triggered")?
        }
    };

    let attached = merge_queue_ops::set_merge_group(
        db,
        entry.id,
        entry.attempt_number,
        &group_sha,
        &base_sha,
        &head_sha,
        pipeline.id,
    )
    .await;
    match attached {
        Ok(true) => {}
        Ok(false) => {
            // A refusal now has two meanings. A terminal/recycled attempt owns
            // nothing from this invocation, so both its graph and its exact ref
            // publication are stale. A live attempt may instead already own the
            // graph another same-attempt producer won; in that case deleting the
            // shared ref would damage the winner, and adopting the same pipeline
            // is already the desired outcome.
            match merge_queue_ops::find_by_pr(db, entry.pr_id).await {
                Ok(Some(winner))
                    if winner.id == entry.id
                        && winner.attempt_number == entry.attempt_number
                        && matches!(winner.status.as_str(), "queued" | "running")
                        && winner.merge_group_pipeline_id.is_some() =>
                {
                    if winner.merge_group_pipeline_id != Some(pipeline.id) {
                        retire_losing_merge_group_pipeline(
                            db,
                            repo_root,
                            repository,
                            entry,
                            &winner,
                            pipeline.id,
                            &group_sha,
                        )
                        .await;
                    }
                }
                Ok(_) => {
                    retire_unowned_merge_group_pipeline(
                        db,
                        repo_root,
                        repository,
                        entry,
                        pipeline.id,
                        &group_sha,
                        "the queue attempt ended before it could own the pipeline",
                    )
                    .await;
                }
                Err(error) => {
                    // The graph may already be the live row's winner. Canceling
                    // it without being able to read ownership would turn a DB
                    // read failure into destructive compensation. Preserve it
                    // and surface the read error instead of guessing.
                    tracing::warn!(
                        entry_id = entry.id,
                        attempt_number = entry.attempt_number,
                        pipeline_id = pipeline.id,
                        error = %format!("{error:#}"),
                        "merge-group ownership was refused but its winner could not be read; the graph was left intact"
                    );
                    return Err(error);
                }
            }
            return Ok(MergeGroupState::Abandoned);
        }
        Err(error) => {
            // A transient write failure is deliberately recoverable: as long as
            // this exact attempt is still queued, the deterministic group SHA
            // lets the next pass adopt the already-paid-for pipeline. If the
            // owner ended while the write failed, preserving the graph would
            // instead create the orphan this protocol is meant to prevent.
            let preserved_for_adoption = match queue_attempt_is_current(db, entry).await {
                Ok(true) => true,
                Ok(false) => {
                    retire_unowned_merge_group_pipeline(
                        db,
                        repo_root,
                        repository,
                        entry,
                        pipeline.id,
                        &group_sha,
                        "the queue attempt ended while pipeline ownership could not be recorded",
                    )
                    .await;
                    false
                }
                Err(reconcile_error) => {
                    tracing::warn!(
                        entry_id = entry.id,
                        attempt_number = entry.attempt_number,
                        pipeline_id = pipeline.id,
                        error = %format!("{reconcile_error:#}"),
                        "merge-group pipeline ownership could not be reconciled after the write failed; the graph was left for deterministic adoption"
                    );
                    true
                }
            };
            if preserved_for_adoption {
                // Deliberately no cancel here: the group SHA is stable, so the
                // next pass finds this graph and re-attempts the write.
                tracing::warn!(
                    entry_id = entry.id,
                    pipeline_id = pipeline.id,
                    error = %format!("{error:#}"),
                    "merge-queue entry could not record its merge-group pipeline; the next queue pass will adopt it"
                );
            } else {
                tracing::warn!(
                    entry_id = entry.id,
                    pipeline_id = pipeline.id,
                    error = %format!("{error:#}"),
                    "merge-group pipeline ownership write failed after its queue attempt ended; the published graph was retired"
                );
            }
            return Err(error);
        }
    }

    // The audit event is a side note of a state change that is now committed —
    // losing it must not unwind the ownership just written, or the entry is back
    // to owning nothing.
    if let Err(error) = rg_db::ops::pr_event_ops::record(
        db,
        repository.id,
        pr.id,
        None,
        "merge_group_created",
        None,
        serde_json::json!({"commit_sha": group_sha, "pipeline_id": pipeline.id}),
    )
    .await
    {
        tracing::warn!(
            entry_id = entry.id,
            pipeline_id = pipeline.id,
            error = %format!("{error:#}"),
            "failed to record the merge_group_created event"
        );
    }

    merge_group_state(db, repo_root, entry, &pipeline).await
}

/// The queue's verdict on a merge-group pipeline — shared by the entry that
/// already owns one and the pass that has just created or adopted it, so an
/// adopted pipeline that already finished is not reported as pending.
async fn merge_group_state(
    db: &DatabaseConnection,
    repo_root: &Path,
    entry: &merge_queue_entry::Model,
    pipeline: &pipeline::Model,
) -> Result<MergeGroupState> {
    Ok(match pipeline.status.as_str() {
        "success" => MergeGroupState::Ready,
        "failed" | "canceled" => {
            if !finish_entry(
                db,
                repo_root,
                entry,
                "failed",
                Some(format!(
                    "merge-group pipeline #{} is {}",
                    pipeline.id, pipeline.status
                )),
            )
            .await?
            {
                MergeGroupState::Abandoned
            } else {
                MergeGroupState::Failed
            }
        }
        status => {
            MergeGroupState::Waiting(format!("merge-group pipeline #{} is {status}", pipeline.id))
        }
    })
}

pub async fn process_for_head_commit_with_ci(
    db: &DatabaseConnection,
    repo_root: &Path,
    source_repo_id: i64,
    commit_sha: &str,
    ci: &PipelineCi<'_>,
) -> Result<Vec<MergeQueueProcessResult>> {
    process_for_head_commit_inner(db, repo_root, source_repo_id, commit_sha, ci).await
}

async fn process_for_head_commit_inner(
    db: &DatabaseConnection,
    repo_root: &Path,
    source_repo_id: i64,
    commit_sha: &str,
    ci: &PipelineCi<'_>,
) -> Result<Vec<MergeQueueProcessResult>> {
    if let Some(entry) =
        merge_queue_ops::find_by_merge_group_sha(db, source_repo_id, commit_sha).await?
    {
        let repository = repository::Entity::find_by_id(entry.repo_id)
            .one(db)
            .await?
            .context("merge-group repository not found")?;
        let result = process_repository_with_ci(db, repo_root, &repository, ci).await?;
        return Ok(vec![result]);
    }
    let prs = pull_request_ops::list_open_for_head_commit(db, source_repo_id, commit_sha).await?;
    let mut seen = HashSet::new();
    let mut results = Vec::new();
    for pr in prs {
        if !seen.insert(pr.repo_id) {
            continue;
        }
        let repository = repository::Entity::find_by_id(pr.repo_id)
            .one(db)
            .await?
            .context("merge-queue repository not found")?;
        results.push(process_repository_with_ci(db, repo_root, &repository, ci).await?);
    }
    Ok(results)
}

/// Deleting the merge-group ref is best-effort, so its failures never reach a
/// caller — the log line is the only channel an operator has, and these tests
/// hold every failure branch to producing one (card_75646d7b017b).
#[cfg(test)]
mod merge_group_ref_cleanup_tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ConnectionTrait, NotSet};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        fn rendered(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// `set_default` is thread-local and `#[tokio::test]` runs on the current
    /// thread, so the guard covers the awaits too.
    fn capture_warnings() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (logs, guard)
    }

    async fn setup_db() -> DatabaseConnection {
        let mut options = sea_orm::ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = sea_orm::Database::connect(options)
            .await
            .expect("connect in-memory database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    struct Fixture {
        db: DatabaseConnection,
        sandbox: tempfile::TempDir,
        repo_root: std::path::PathBuf,
        owner: rg_db::entities::user::Model,
        repository: repository::Model,
        pr: pull_request::Model,
        entry: merge_queue_entry::Model,
    }

    /// A repository with a real bare git repo on disk, a PR, and a queued entry.
    /// Individual tests publish the ref so they can exercise the
    /// partial-publication cleanup path as well.
    async fn fixture(name: &str) -> Fixture {
        let db = setup_db().await;
        let owner = rg_db::ops::user_ops::create_user(
            &db,
            &format!("{name}-owner"),
            &format!("{name}@example.invalid"),
            "unused",
            "Queue Owner",
        )
        .await
        .expect("create owner");
        let sandbox = tempfile::tempdir().expect("create sandbox");
        let repo_root = sandbox.path().join("repos");
        let repository =
            crate::repo::service::create_repo(&db, owner.id, name, None, false, &repo_root, None)
                .await
                .expect("create repository");
        let now = Utc::now();
        let pr = pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repository.id),
            number: Set(1),
            title: Set("queued".into()),
            body: Set(None),
            state: Set("open".into()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(owner.id),
            reviewer_id: Set(None),
            head_branch: Set("feature".into()),
            base_branch: Set("main".into()),
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
        .insert(&db)
        .await
        .expect("create pull request");
        let entry = merge_queue_ops::enqueue(&db, repository.id, pr.id, owner.id, "merge")
            .await
            .expect("enqueue");
        Fixture {
            db,
            sandbox,
            repo_root,
            owner,
            repository,
            pr,
            entry,
        }
    }

    fn git() -> &'static rg_git::cli_gateway::GitCommandGateway {
        rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway")
    }

    /// Point the entry's group ref at a real commit, so a successful delete is
    /// distinguishable from a delete that had nothing to do.
    fn create_group_ref(fixture: &Fixture) -> String {
        let repo_path = fixture.repo_root.join(format!(
            "{}/{}.git",
            fixture.owner.username, fixture.repository.name
        ));
        // `hash-object` over an empty file yields the empty tree without needing
        // a working copy or stdin; `commit-tree` then gives a real commit.
        let empty = fixture.sandbox.path().join("empty");
        std::fs::write(&empty, b"").expect("write empty file");
        let tree = git()
            .run(
                &["hash-object", "-w", "-t", "tree", &empty.to_string_lossy()],
                Some(&repo_path),
            )
            .expect("hash empty tree");
        tree.ensure_success().expect("hash empty tree");
        let tree = tree.stdout_str().trim().to_string();
        let commit = git()
            .run_with_env(
                &["commit-tree", &tree, "-m", "group"],
                Some(&repo_path),
                &[
                    ("GIT_AUTHOR_NAME", "Queue"),
                    ("GIT_AUTHOR_EMAIL", "queue@example.invalid"),
                    ("GIT_COMMITTER_NAME", "Queue"),
                    ("GIT_COMMITTER_EMAIL", "queue@example.invalid"),
                ],
            )
            .expect("commit-tree");
        commit.ensure_success().expect("commit-tree");
        let commit = commit.stdout_str().trim().to_string();
        let group_ref = format!("refs/merge-queue/{}", fixture.entry.id);
        git()
            .run(&["update-ref", &group_ref, &commit], Some(&repo_path))
            .expect("create group ref")
            .ensure_success()
            .expect("create group ref");
        group_ref
    }

    fn ref_exists(fixture: &Fixture, group_ref: &str) -> bool {
        let repo_path = fixture.repo_root.join(format!(
            "{}/{}.git",
            fixture.owner.username, fixture.repository.name
        ));
        git()
            .run(&["rev-parse", "--verify", group_ref], Some(&repo_path))
            .expect("rev-parse")
            .success()
    }

    #[tokio::test]
    async fn the_happy_path_deletes_the_ref_and_says_nothing() {
        let fixture = fixture("clean-delete").await;
        let group_ref = create_group_ref(&fixture);
        assert!(ref_exists(&fixture, &group_ref), "fixture ref was created");

        let (logs, _guard) = capture_warnings();
        cleanup_merge_group_ref(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &fixture.entry,
            None,
        )
        .await;

        assert!(!ref_exists(&fixture, &group_ref), "the ref is gone");
        assert!(logs.rendered().is_empty(), "{}", logs.rendered());
    }

    /// card_884d819fc51b: the cleanup used to skip on `merge_group_sha IS NULL`,
    /// and the ref is created several statements before that column is written.
    ///
    /// A repository with no CI config takes the `has_ci_config` early return in
    /// `ensure_merge_group_ci` — after `update-ref`, before `set_merge_group` —
    /// so the column stayed NULL on an entry that owned a ref, and the cleanup
    /// declined on every terminal path. One `refs/merge-queue/*` per queue
    /// entry, forever, each one advertised to every client and each one keeping
    /// its group commit alive against `gc`.
    ///
    /// Driven through `cancel`, not the helper: the point is that a real
    /// terminal path leaves nothing behind.
    #[tokio::test]
    async fn a_ref_whose_entry_never_recorded_a_group_sha_is_still_deleted() {
        let fixture = fixture("no-ci-config").await;
        let group_ref = create_group_ref(&fixture);
        // Put the entry back in the state `ensure_merge_group_ci` leaves it in
        // when the repository has no CI config: ref on disk, column unset.
        fixture
            .db
            .execute_unprepared(&format!(
                "UPDATE merge_queue_entries SET merge_group_sha = NULL WHERE id = {}",
                fixture.entry.id
            ))
            .await
            .expect("clear the recorded group sha");
        assert!(
            merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
                .await
                .expect("read entry")
                .expect("entry exists")
                .merge_group_sha
                .is_none(),
            "the fixture must reproduce the unset column"
        );

        let (logs, _guard) = capture_warnings();
        let outcome = cancel(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &fixture.pr,
            fixture.owner.id,
        )
        .await
        .expect("cancel succeeds");

        assert_eq!(outcome, CancelOutcome::Canceled);
        assert!(
            !ref_exists(&fixture, &group_ref),
            "the ref is deleted by its own existence, not by a column that no \
             longer means it exists"
        );
        assert!(logs.rendered().is_empty(), "{}", logs.rendered());
    }

    /// The other half: an entry that never created a ref must stay quiet.
    ///
    /// Dropping the column gate means `update-ref -d` now runs on every
    /// terminal path. It is idempotent and exits 0 on a ref that is not there,
    /// so the no-op must not turn into a warning an operator would chase.
    #[tokio::test]
    async fn an_entry_that_never_created_a_ref_deletes_nothing_and_warns_about_nothing() {
        let fixture = fixture("never-created").await;
        let group_ref = format!("refs/merge-queue/{}", fixture.entry.id);
        assert!(
            !ref_exists(&fixture, &group_ref),
            "the fixture must start with no ref"
        );

        let (logs, _guard) = capture_warnings();
        cleanup_merge_group_ref(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &fixture.entry,
            None,
        )
        .await;

        assert!(!ref_exists(&fixture, &group_ref));
        assert!(
            logs.rendered().is_empty(),
            "deleting an absent ref is a no-op, not something to report: {}",
            logs.rendered()
        );
    }

    #[tokio::test]
    async fn a_broken_queue_lookup_is_not_mistaken_for_an_absent_entry() {
        let fixture = fixture("broken-lookup").await;
        let group_ref = create_group_ref(&fixture);
        fixture
            .db
            .execute_unprepared("DROP TABLE merge_queue_entries")
            .await
            .expect("drop the queue table");

        let (logs, _guard) = capture_warnings();
        cleanup_merge_group_ref(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &fixture.entry,
            None,
        )
        .await;

        let rendered = logs.rendered();
        assert!(rendered.contains(STALE_REF), "{rendered}");
        assert!(
            rendered.contains(&format!("pr_id={}", fixture.pr.id)),
            "{rendered}"
        );
        // The cause chain, not just the outer context.
        assert!(rendered.contains("no such table"), "{rendered}");
        assert!(
            ref_exists(&fixture, &group_ref),
            "the ref really is the one left behind"
        );
    }

    /// The namespace failure is reached through the public entry point, because
    /// the point of best-effort cleanup is that the caller still hears success.
    #[tokio::test]
    async fn an_unresolvable_namespace_is_logged_without_failing_the_cancel() {
        let fixture = fixture("broken-namespace").await;
        // The namespace is derived from the owner account; an owner that cannot
        // be read is the failure, and pointing the model at a missing id gives
        // it without deleting rows the queue entry itself hangs off.
        let orphaned = repository::Model {
            owner_id: i64::MAX,
            ..fixture.repository.clone()
        };

        let (logs, _guard) = capture_warnings();
        let outcome = cancel(
            &fixture.db,
            &fixture.repo_root,
            &orphaned,
            &fixture.pr,
            fixture.owner.id,
        )
        .await
        .expect("cancel still succeeds");

        assert_eq!(
            outcome,
            CancelOutcome::Canceled,
            "the queue entry was canceled"
        );
        let rendered = logs.rendered();
        assert!(rendered.contains(STALE_REF), "{rendered}");
        assert!(
            rendered.contains(&format!("entry_id={}", fixture.entry.id)),
            "{rendered}"
        );
        assert!(
            rendered.contains("repository owner not found"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn a_git_delete_that_fails_is_logged_with_its_stderr() {
        let fixture = fixture("broken-git").await;
        // A repo root that holds no repository: `git -C` cannot even enter it,
        // which is the shape of a storage mount that went away.
        let missing_root = fixture.sandbox.path().join("vanished");

        let (logs, _guard) = capture_warnings();
        cleanup_merge_group_ref(
            &fixture.db,
            &missing_root,
            &fixture.repository,
            &fixture.entry,
            None,
        )
        .await;

        let rendered = logs.rendered();
        assert!(rendered.contains(STALE_REF), "{rendered}");
        assert!(
            rendered.contains(&format!("refs/merge-queue/{}", fixture.entry.id)),
            "{rendered}"
        );
        assert!(rendered.contains("stderr="), "{rendered}");
    }

    /// `finish_entry` reads the repository itself, and that read used to be
    /// swallowed by the same `if let Ok(Some(..))` shape.
    #[tokio::test]
    async fn a_finish_whose_repository_row_vanished_still_reports_the_stale_ref() {
        let fixture = fixture("vanished-repo").await;
        // The row has to go without taking the queue entry and the event rows
        // with it, so the deletion is done with the constraint lifted — what is
        // being tested is the reader's reaction to a missing row, not sqlite's.
        // The pool holds a single connection, so the pragma covers the test.
        fixture
            .db
            .execute_unprepared("PRAGMA foreign_keys = OFF")
            .await
            .expect("lift the foreign keys");
        rg_db::entities::repository::Entity::delete_by_id(fixture.repository.id)
            .exec(&fixture.db)
            .await
            .expect("delete the repository row");

        let (logs, _guard) = capture_warnings();
        assert!(finish_entry(
            &fixture.db,
            &fixture.repo_root,
            &fixture.entry,
            "failed",
            Some("test".into()),
        )
        .await
        .expect("finish still succeeds"));

        let rendered = logs.rendered();
        assert!(rendered.contains(STALE_REF), "{rendered}");
        assert!(
            rendered.contains(&format!("entry_id={}", fixture.entry.id)),
            "{rendered}"
        );
    }
}
