//! Repository-scoped FIFO merge queue.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sea_orm::{DatabaseConnection, EntityTrait, Set, TransactionTrait};

use rg_db::entities::{merge_queue_entry, pipeline, pull_request, repository};
use rg_db::ops::{merge_queue_ops, pull_request_ops, repo_ops};

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

/// A merge-queue pass and, when it stopped part-way, the failure that stopped
/// it.
///
/// A pass merges the queue head over and over, and each merge is finished
/// before the next one starts: the entry is `merged` in the database and the
/// merge commit is already the tip of the base branch. The post-push hooks for
/// those moves — a pipeline on the merge commit, the `push` webhook, the watch
/// notifications — are the caller's to run, and it runs them from
/// [`MergeQueueProcessResult::merged_ref_updates`].
///
/// So a pass that answered a bare `Err` on its third entry threw away the two
/// merges it had already made: the branch moved for real and nothing was ever
/// told, while the caller answered `5xx` about a pull request further down the
/// queue (card_94dbd5fd4bce). The failure and the work are two separate facts,
/// and this carries both.
#[derive(Debug)]
pub struct MergeQueueRun<T> {
    /// What the pass did before it stopped — complete when `error` is `None`,
    /// and everything up to the failure when it is not.
    pub done: T,
    /// Why the pass stopped early, if it did.
    pub error: Option<anyhow::Error>,
}

pub async fn enqueue(
    db: &DatabaseConnection,
    repository: &repository::Model,
    pr: &pull_request::Model,
    actor_id: i64,
    strategy: MergeStrategy,
) -> Result<merge_queue_entry::Model> {
    let handoff = rg_db::contention::retry_transaction(
        "handoff PR from auto-merge to merge queue",
        || async {
            let transaction = db
                .begin()
                .await
                .context("db: begin auto-merge to merge-queue handoff")?;
            let current = pull_request_ops::lock_by_id_for_update(&transaction, pr.id)
                .await?
                .ok_or_else(|| crate::error::not_found("pull request"))?;

            // These checks use the locked row rather than the caller's snapshot:
            // a close/draft transition that won the lock must be the state this
            // request reports. SQLite's read-to-write upgrade can lose to a
            // concurrent handoff; the retry loop then re-reads a fresh snapshot.
            if current.repo_id != repository.id {
                return Err(crate::error::not_found("pull request"));
            }
            if current.state != "open" {
                return Err(crate::error::conflict(format!(
                    "only an open pull request can enter the merge queue (current: {})",
                    current.state
                )));
            }
            if current.is_draft {
                return Err(crate::error::conflict(
                    "a draft pull request cannot enter the merge queue",
                ));
            }

            let auto_merge_disabled =
                pull_request_ops::disable_auto_merge_for_queue(&transaction, current.id).await?;
            let entry = match merge_queue_ops::enqueue(
                &transaction,
                repository.id,
                current.id,
                actor_id,
                strategy.as_str(),
            )
            .await?
            {
                Some(entry) => entry,
                None => {
                    // A trigger-backed SQLite test models the parent cascade
                    // that a server database can commit on another connection.
                    // Here that cascade belongs to our transaction, so preserve
                    // it explicitly. If only the repository disappeared, first
                    // restore the still-live PR's original auto-merge owner.
                    let repository_exists = repository::Entity::find_by_id(repository.id)
                        .one(&transaction)
                        .await
                        .context("db: resolve repository after vanished queue handoff")?
                        .is_some();
                    let pull_request_exists =
                        pull_request_ops::lock_by_id_for_update(&transaction, current.id)
                            .await?
                            .is_some();
                    if !repository_exists {
                        if auto_merge_disabled && pull_request_exists {
                            let mut restore: pull_request::ActiveModel = current.clone().into();
                            restore.auto_merge_enabled = Set(current.auto_merge_enabled);
                            restore.auto_merge_strategy = Set(current.auto_merge_strategy.clone());
                            restore.auto_merge_enabled_by_id =
                                Set(current.auto_merge_enabled_by_id);
                            restore.auto_merge_enabled_at = Set(current.auto_merge_enabled_at);
                            restore.updated_at = Set(current.updated_at);
                            pull_request_ops::update_in_transaction(&transaction, restore)
                                .await
                                .context("db: restore auto-merge after repository cascade")?;
                        }
                        transaction
                            .commit()
                            .await
                            .context("db: commit repository cascade during queue handoff")?;
                        return Err(crate::error::not_found("repository"));
                    }
                    if !pull_request_exists {
                        transaction
                            .commit()
                            .await
                            .context("db: commit PR cascade during queue handoff")?;
                        return Err(crate::error::not_found("pull request"));
                    }
                    anyhow::bail!(
                        "db: merge-queue entry vanished inside ownership handoff for PR {}",
                        current.id
                    );
                }
            };

            transaction
                .commit()
                .await
                .context("db: commit auto-merge to merge-queue handoff")?;
            Ok((entry, auto_merge_disabled))
        },
    )
    .await;
    let (entry, auto_merge_disabled) = match handoff {
        Ok(done) => done,
        Err(error) => {
            // Server databases can surface a parent cascade as an FK or
            // serialization failure. The transaction has rolled back here, so
            // classify against committed parent state without losing the cause.
            if repo_ops::find_by_id(db, repository.id).await?.is_none() {
                return Err(crate::error::not_found("repository"));
            }
            if pull_request_ops::find_by_id(db, pr.id).await?.is_none() {
                return Err(crate::error::not_found("pull request"));
            }
            return Err(error);
        }
    };

    // Timeline rows describe the committed ownership handoff. They deliberately
    // stay outside its transaction and remain best-effort observability.
    if auto_merge_disabled {
        if let Err(error) = rg_db::ops::pr_event_ops::record(
            db,
            entry.repo_id,
            entry.pr_id,
            Some(actor_id),
            "auto_merge_disabled",
            None,
            serde_json::json!({"reason": "merge_queue_enqueued"}),
        )
        .await
        {
            tracing::error!(
                repo_id = entry.repo_id,
                pr_id = entry.pr_id,
                actor_id,
                error = %format!("{error:#}"),
                "auto-merge was disabled for merge-queue ownership, but its timeline event could not be recorded"
            );
        }
    }
    if let Err(error) = rg_db::ops::pr_event_ops::record(
        db,
        entry.repo_id,
        entry.pr_id,
        Some(actor_id),
        "merge_queue_enqueued",
        None,
        serde_json::json!({"entry_id": entry.id, "strategy": entry.strategy}),
    )
    .await
    {
        tracing::error!(
            repo_id = entry.repo_id,
            pr_id = entry.pr_id,
            entry_id = entry.id,
            actor_id,
            error = %format!("{error:#}"),
            "pull request is in the merge queue, but its timeline event could not be recorded"
        );
    }
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
    // The conditional write above already moved the entry to its terminal
    // status, and nothing below can take that back. So the timeline write is
    // bookkeeping *about* a settlement that has happened: reporting its failure
    // as this function's `Err` would tell the caller the entry was never
    // settled, and the queue pass would drop what it did with that entry —
    // including, on the merged path, the base branch's ref move, which is the
    // only channel the post-push hooks have (card_a0332b45eccd). The missing
    // row costs the timeline a line; it must not cost the branch its hooks.
    if let Err(error) = rg_db::ops::pr_event_ops::record(
        db,
        entry.repo_id,
        entry.pr_id,
        None,
        &format!("merge_queue_{status}"),
        failure_reason,
        serde_json::json!({"entry_id": entry.id, "strategy": entry.strategy}),
    )
    .await
    {
        tracing::error!(
            entry_id = entry.id,
            pr_id = entry.pr_id,
            repo_id = entry.repo_id,
            status,
            error = %format!("{error:#}"),
            "the queue entry finished, but its merge_queue_{status} timeline event could not be recorded"
        );
    }
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
) -> MergeQueueRun<MergeQueueProcessResult> {
    process_repository_inner(db, repo_root, repository, ci).await
}

async fn process_repository_inner(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    ci: &PipelineCi<'_>,
) -> MergeQueueRun<MergeQueueProcessResult> {
    let mut done = MergeQueueProcessResult {
        merged: Vec::new(),
        failed: Vec::new(),
        waiting_reason: None,
        merged_ref_updates: Vec::new(),
    };
    let error = process_repository_into(db, repo_root, repository, ci, &mut done)
        .await
        .err();
    MergeQueueRun { done, error }
}

/// The pass itself, writing what it did into `result` as it goes.
///
/// Every `?` below leaves a queue that may already have merged: the entries
/// ahead of the failure are `merged` in the database and their merge commits
/// are the new tip of the base branch. Handing those back is the whole point of
/// the split — the failure travels as the `Err`, the work travels in `result`.
async fn process_repository_into(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    ci: &PipelineCi<'_>,
    result: &mut MergeQueueProcessResult,
) -> Result<()> {
    let namespace = service::repository_namespace(db, repository).await?;

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
            // Two different answers hide in this one `Err`. A rule that
            // *refused* — too few approvals, a required check that has not
            // passed — is a condition the author can watch, and its message was
            // written for them, so it is handed over whole (card_a997f30c142c).
            //
            // A rule the server could not *read* decided nothing. Reporting it
            // here would put the pull request behind a condition nobody
            // evaluated — the queue page says "waiting for", the entry stays
            // `queued`, and the next pass fails the same way — and it would
            // carry the `db: …` chain of the failed read to everyone who can
            // enqueue (card_af2abe7904bd, H-05). The queue run fails instead,
            // and its callers answer 5xx or log a warning.
            let Some(reason) = crate::error::client_facing_message(&error) else {
                return Err(error.context(format!(
                    "merge queue: branch protection of '{}' could not be checked",
                    pr.base_branch
                )));
            };
            result.waiting_reason = Some(reason);
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
        // announcement goes to the process-global delivery tracker. The stored
        // enqueuer is durable provenance, not a durable grant: `merge_pr`
        // revalidates that actor's standing and current write permission.
        match service::merge_pr(
            db,
            repo_root,
            &namespace,
            &repository.name,
            pr.number,
            entry.enqueued_by_id,
            strategy,
            None,
        )
        .await
        {
            Ok(merge) => {
                // The queue moved the base branch; the hooks for that move are
                // the caller's to run (card_87c4912c51ed). Recorded here rather
                // than beside `merged` below, because by now the merge commit is
                // the tip of `refs/heads/<base>` and this vector is the only way
                // the caller hears about it. Settling the entry is a separate
                // fact that can go two other ways — `Err` leaves through the `?`
                // and `Ok(false)` means another attempt closed the row first —
                // and under either the ref move used to die with `merge`, so the
                // branch moved and no pipeline, webhook or watcher was ever told
                // (card_a0332b45eccd).
                result.merged_ref_updates.extend(merge.base_ref_update);
                if finish_entry(db, repo_root, &entry, "merged", None).await? {
                    result.merged.push(pr.id);
                }
            }
            Err(error) => {
                // Whatever `merge_pr` answered, only the half written for the
                // author may be persisted: `finish_entry` records this reason
                // as the body of the pull request's `merge_queue_failed` event.
                let reason = match merge_failure_reason(&error) {
                    Some(reason) => reason,
                    None => {
                        // The queue is the last caller this error reaches —
                        // nothing above it logs the chain, and the reason below
                        // deliberately keeps none of it.
                        tracing::error!(
                            entry_id = entry.id,
                            pr_id = pr.id,
                            repo_id = repository.id,
                            strategy = %entry.strategy,
                            error = %format!("{error:#}"),
                            "the merge queue could not merge the pull request"
                        );
                        MERGE_FAILED_REASON.to_string()
                    }
                };
                if finish_entry(db, repo_root, &entry, "failed", Some(reason)).await? {
                    result.failed.push(pr.id);
                }
            }
        }
    }
    Ok(())
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

/// Settle a queue attempt whose merge-group workflow the engine refuses.
///
/// card_1d7f511da1c8: this error used to leave `ensure_merge_group_ci` through
/// `?`, and by then the queue entry was already committed. What the author saw
/// then depended only on which producer happened to run the queue — a `5xx`
/// answering a successful enqueue, or, from a review or a post-push pass,
/// nothing at all. The entry stayed durable carrying no verdict, so the next
/// pass rebuilt the same group and re-refused the same configuration.
///
/// A workflow the engine cannot honour is the repository's own mistake and has
/// to *settle* the attempt: the entry finishes `failed` with the safe reason —
/// the same text `finish_entry` shows in the queue UI and records as a PR event
/// — and the refusal also becomes a terminal run on the pipelines page, which
/// is the record push and PR sync already publish for this class
/// (card_2cb963740aaa). Published before the entry finishes, because finishing
/// deletes the group ref this run is about.
///
/// Anything else is infrastructure — storage, Git, a lost connection. It goes
/// back to the caller untouched: it is retryable, it is not the author's typo,
/// and its text may name operator paths that must not cross into a repository.
async fn settle_refused_merge_group_config(
    db: &DatabaseConnection,
    repo_root: &Path,
    entry: &merge_queue_entry::Model,
    group_sha: &str,
    group_ref: &str,
    base_branch: &str,
    error: anyhow::Error,
) -> Result<MergeGroupState> {
    let Some(reason) = error
        .downcast_ref::<crate::error::InvalidRequest>()
        .map(|invalid| invalid.message.clone())
    else {
        return Err(error);
    };

    if let Err(publish_error) = crate::ci::publish_configuration_failure(
        crate::ci::ConfigurationFailureParams {
            db,
            repo_id: entry.repo_id,
            commit_sha: group_sha,
            ref_name: group_ref,
            trigger_type: "merge_group",
            triggered_by: Some(entry.enqueued_by_id),
            // `merge_group` shares the `on: pull_request` filter and the ref
            // above is the synthetic group ref, so the branch the filter is
            // about is the one the queue merges into — the same value the
            // successful trigger passes. The group commit is built fresh, so
            // there is no previous revision, here or there.
            base_branch: Some(base_branch),
            previous_sha: None,
        },
        &error,
    )
    .await
    {
        // The entry still settles below: a diagnostic run that could not be
        // written is worth less than a queue head that stays stuck behind a
        // configuration nobody can fix from the queue page.
        tracing::error!(
            entry_id = entry.id,
            pr_id = entry.pr_id,
            error = %format!("{publish_error:#}"),
            "merge-group CI configuration refusal could not be published as a failed run"
        );
    }

    if !finish_entry(db, repo_root, entry, "failed", Some(reason)).await? {
        return Ok(MergeGroupState::Abandoned);
    }
    Ok(MergeGroupState::Failed)
}

/// What the queue tells the author when the merge group will not build.
///
/// Fixed text, and deliberately not git's own: `finish_entry` stores this on the
/// entry and records it as a PR event, and the review timeline in `rg-http`
/// renders that event's reason as its body — so this string is shown to everyone
/// who can see the pull request. Git's own output names server-side repository
/// paths and object ids, and stays in the log (H-05).
const MERGE_GROUP_CONFLICT_REASON: &str =
    "merge group conflict: the pull request no longer merges cleanly into the base branch";

/// The object id `git merge-tree --write-tree` wrote as its first stdout line,
/// or `None` when it produced no tree at all.
///
/// This is the queue's discriminator between a merge that ran and conflicted and
/// a merge that never started, so it checks the shape rather than trusting
/// whatever line one happens to hold: a diagnostic printed on stdout must not be
/// read as a tree and settle somebody's pull request with a verdict git never
/// gave.
fn merge_tree_object_id(stdout: &str) -> Option<&str> {
    let first = stdout.lines().next()?.trim();
    (matches!(first.len(), 40 | 64) && first.chars().all(|c| c.is_ascii_hexdigit()))
        .then_some(first)
}

/// What the queue tells the author when the merge itself failed for a reason
/// only an operator can read.
///
/// Fixed text, and deliberately not the error's own, for the same reason as
/// [`MERGE_GROUP_CONFLICT_REASON`]: `finish_entry` stores this on the entry and
/// records it as a PR event whose body the review timeline renders, so the
/// string is shown to everyone who can see the pull request.
const MERGE_FAILED_REASON: &str =
    "merge failed: the server could not complete this merge; an operator has the details";

/// The part of a failed [`service::merge_pr`] that may be shown to everyone who
/// can see the pull request, or `None` when the failure is an operator's to read.
///
/// `merge_pr` answers with two kinds of error. The typed states of
/// [`crate::error`] carry a message written for the person who asked — a closed
/// or draft pull request, a merge that lost its race, a branch-protection rule
/// that refuses, an account that may no longer merge — and each of those types
/// documents that its message reaches a client verbatim, which is exactly what
/// `rg-http` does with them. Everything else is ours: a failed `git` invocation
/// carries the whole command line, `-C <server-side repository path>` included,
/// and the rebase strategy still bails with git's own stderr.
///
/// The queue is why that split has to be made here rather than at the edge.
/// `POST .../merge` funnels the untyped half through `AppError`, which answers
/// `500` and drops the body; the queue calls `merge_pr` past that funnel,
/// catches the `Err` itself, and hands whatever it holds to `finish_entry` —
/// which writes it into the pull request's timeline (card_ff19d8130b49, H-05).
///
/// The split itself is [`crate::error::client_facing_message`], which the branch
/// protection check above the merge now shares: `downcast_ref` sees through any
/// `.context(…)` a caller layered on the way up, and only the typed frame's own
/// message is taken, never the flattened chain.
fn merge_failure_reason(error: &anyhow::Error) -> Option<String> {
    crate::error::client_facing_message(error)
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
    // `git merge-tree --write-tree` does not separate "the merge ran and left
    // conflicts" from "the merge could not run at all" by exit code: both exit
    // 1. A missing object answers `merge-tree: <oid> - not something we can
    // merge`, also with 1. What separates them is whether a merge happened —
    // one that ran writes the merged tree's object id as its first stdout line
    // and lists the conflicted paths after it, one that never started writes
    // nothing there and puts its complaint on stderr. Asking for that tree is a
    // question about git's *state*, the form `rebase_stopped_on_conflict` in
    // `pull_request::service` settled on; reading the message instead would make
    // the verdict a property of git's wording.
    let tree_sha = merge_tree_object_id(&tree_output.stdout_str()).map(str::to_string);
    if !tree_output.success() {
        if tree_sha.is_none() {
            // Git merged nothing, so there is no conflict to report. This half
            // is infrastructure — a missing object, a broken store, a killed
            // process. It is retryable, it is not the author's mistake, and its
            // text names operator paths that must not cross into a repository,
            // so it goes back to the caller untouched exactly as
            // `settle_refused_merge_group_config` documents for its own.
            tree_output
                .ensure_success()
                .context("failed to build the merge-group tree")?;
        }
        // The conflict listing is on stdout and stderr is usually empty here;
        // both are carried so an operator reading this line has what git said,
        // whichever stream it chose.
        tracing::warn!(
            entry_id = entry.id,
            pr_id = entry.pr_id,
            repo_id = entry.repo_id,
            exit_code = ?tree_output.status.code(),
            stdout = %tree_output.stdout_str().trim(),
            stderr = %tree_output.stderr_str().trim(),
            "merge group does not merge cleanly into its base branch"
        );
        if !finish_entry(
            db,
            repo_root,
            entry,
            "failed",
            Some(MERGE_GROUP_CONFLICT_REASON.to_string()),
        )
        .await?
        {
            return Ok(MergeGroupState::Abandoned);
        }
        return Ok(MergeGroupState::Failed);
    }
    let tree_sha = tree_sha.context("git merge-tree did not return a tree id")?;
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
            let triggered = ci
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
                    inputs: None,
                    triggered_by: Some(entry.enqueued_by_id),
                    docker_enabled: ci.docker_enabled,
                    external_runners: ci.external_runners,
                    allow_host_runner: ci.allow_host_runner,
                    jwt_secret: ci.jwt_secret,
                    encryption_key: ci.encryption_key,
                    external_url: ci.external_url,
                })
                .await;
            let pipeline_id = match triggered {
                Ok(pipeline_id) => pipeline_id,
                Err(error)
                    if error
                        .downcast_ref::<crate::ci::NoMatchingCiJobs>()
                        .is_some() =>
                {
                    tracing::info!(
                        entry_id = entry.id,
                        ref_name = %group_ref,
                        "CI config selected no jobs for this merge group"
                    );
                    return Ok(MergeGroupState::Ready);
                }
                Err(error) => {
                    return settle_refused_merge_group_config(
                        db,
                        repo_root,
                        entry,
                        &group_sha,
                        &group_ref,
                        &pr.base_branch,
                        error,
                    )
                    .await
                }
            };
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
) -> MergeQueueRun<Vec<MergeQueueProcessResult>> {
    process_for_head_commit_inner(db, repo_root, source_repo_id, commit_sha, ci).await
}

async fn process_for_head_commit_inner(
    db: &DatabaseConnection,
    repo_root: &Path,
    source_repo_id: i64,
    commit_sha: &str,
    ci: &PipelineCi<'_>,
) -> MergeQueueRun<Vec<MergeQueueProcessResult>> {
    let mut done = Vec::new();
    let error =
        process_for_head_commit_into(db, repo_root, source_repo_id, commit_sha, ci, &mut done)
            .await
            .err();
    MergeQueueRun { done, error }
}

/// One commit can unblock queues in several repositories, and each pass merges.
/// A repository whose pass fails must not take the passes before it down with
/// it, so every pass — including the failing one's own partial result — is
/// pushed into `results` before the error travels on (card_94dbd5fd4bce).
async fn process_for_head_commit_into(
    db: &DatabaseConnection,
    repo_root: &Path,
    source_repo_id: i64,
    commit_sha: &str,
    ci: &PipelineCi<'_>,
    results: &mut Vec<MergeQueueProcessResult>,
) -> Result<()> {
    if let Some(entry) =
        merge_queue_ops::find_by_merge_group_sha(db, source_repo_id, commit_sha).await?
    {
        let repository = repository::Entity::find_by_id(entry.repo_id)
            .one(db)
            .await?
            .context("merge-group repository not found")?;
        let run = process_repository_with_ci(db, repo_root, &repository, ci).await;
        results.push(run.done);
        return match run.error {
            Some(error) => Err(error),
            None => Ok(()),
        };
    }
    let prs = pull_request_ops::list_open_for_head_commit(db, source_repo_id, commit_sha).await?;
    let mut seen = HashSet::new();
    for pr in prs {
        if !seen.insert(pr.repo_id) {
            continue;
        }
        let repository = repository::Entity::find_by_id(pr.repo_id)
            .one(db)
            .await?
            .context("merge-queue repository not found")?;
        let run = process_repository_with_ci(db, repo_root, &repository, ci).await;
        results.push(run.done);
        if let Some(error) = run.error {
            return Err(error);
        }
    }
    Ok(())
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
    pub(super) struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

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
        pub(super) fn rendered(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// `set_default` is thread-local and `#[tokio::test]` runs on the current
    /// thread, so the guard covers the awaits too.
    pub(super) fn capture_warnings() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (logs, guard)
    }

    pub(super) async fn setup_db() -> DatabaseConnection {
        let mut options = sea_orm::ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = sea_orm::Database::connect(options)
            .await
            .expect("connect in-memory database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    pub(super) struct Fixture {
        pub(super) db: DatabaseConnection,
        pub(super) sandbox: tempfile::TempDir,
        pub(super) repo_root: std::path::PathBuf,
        pub(super) owner: rg_db::entities::user::Model,
        pub(super) repository: repository::Model,
        pub(super) pr: pull_request::Model,
        pub(super) entry: merge_queue_entry::Model,
    }

    /// A repository with a real bare git repo on disk, a PR, and a queued entry.
    /// Individual tests publish the ref so they can exercise the
    /// partial-publication cleanup path as well.
    pub(super) async fn fixture(name: &str) -> Fixture {
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
            .expect("enqueue")
            .expect("the fixture repository and pull request remain live");
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

    /// A pass that must have reached its end. The queue reports the failure
    /// beside the work now, so "no failure" is asserted rather than assumed by
    /// an `.expect()` on a `Result` that no longer exists.
    pub(super) fn completed<T>(run: MergeQueueRun<T>, expectation: &str) -> T {
        match run.error {
            None => run.done,
            Some(error) => panic!("{expectation}: {error:#}"),
        }
    }

    pub(super) fn git() -> &'static rg_git::cli_gateway::GitCommandGateway {
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

/// card_1d7f511da1c8: what happens to a committed queue entry when the
/// merge-group workflow is one the engine refuses.
///
/// Driven through `process_repository_with_ci` — the function every producer
/// calls (`pulls::enqueue_merge_queue`, `reviews`, the post-push pass) — rather
/// than through the helper, because the defect was precisely that those three
/// producers disagreed about what the refusal meant, and only the shared entry
/// point can show they no longer do.
#[cfg(test)]
mod merge_group_config_refusal_tests {
    use super::merge_group_ref_cleanup_tests::{completed, fixture, git, Fixture};
    use super::*;
    use crate::ci::CiTrigger;
    use sea_orm::ActiveModelTrait;

    /// The refusal a real engine raises for an unsupported workflow key. It
    /// names the file and the key, and that text is what has to survive.
    const REFUSAL: &str = "unsupported key `branch` in .gitea/workflows/merge-group.yml";

    /// A CI engine that refuses the configuration it is handed.
    struct RefusingMergeGroupCi;

    impl CiTrigger for RefusingMergeGroupCi {
        fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
            true
        }

        fn has_workflow_for_event(&self, _query: crate::ci::WorkflowEventQuery<'_>) -> bool {
            true
        }

        fn trigger_pipeline<'a>(
            &'a self,
            _params: crate::ci::TriggerPipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
            Box::pin(async { Err(crate::error::invalid_request(REFUSAL)) })
        }

        fn resume_pipeline<'a>(
            &'a self,
            _params: crate::ci::ResumePipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
            unreachable!("the test never resumes a pipeline")
        }
    }

    /// The other half of the discrimination: storage or Git gave out. Untyped,
    /// retryable, and not the author's mistake.
    struct BrokenStorageCi;

    impl CiTrigger for BrokenStorageCi {
        fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
            true
        }

        fn has_workflow_for_event(&self, _query: crate::ci::WorkflowEventQuery<'_>) -> bool {
            true
        }

        fn trigger_pipeline<'a>(
            &'a self,
            _params: crate::ci::TriggerPipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
            Box::pin(async {
                Err(anyhow::anyhow!(
                    "db: pipeline insert failed at /var/lib/forgekeep/db.sqlite"
                ))
            })
        }

        fn resume_pipeline<'a>(
            &'a self,
            _params: crate::ci::ResumePipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
            unreachable!("the test never resumes a pipeline")
        }
    }

    /// A valid native config whose `only:` selectors exclude the synthetic
    /// merge-group ref. This is neither a failed configuration nor a retryable
    /// infrastructure error: the queue has no check to wait for.
    struct NoMatchingJobsCi;

    impl CiTrigger for NoMatchingJobsCi {
        fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
            true
        }

        fn has_workflow_for_event(&self, _query: crate::ci::WorkflowEventQuery<'_>) -> bool {
            true
        }

        fn trigger_pipeline<'a>(
            &'a self,
            _params: crate::ci::TriggerPipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
            Box::pin(async {
                Err(anyhow::Error::new(crate::ci::NoMatchingCiJobs::new(
                    "refs/merge-queue/1",
                )))
            })
        }

        fn resume_pipeline<'a>(
            &'a self,
            _params: crate::ci::ResumePipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
            unreachable!("the test never resumes a pipeline")
        }
    }

    pub(super) fn ci(trigger: &dyn CiTrigger) -> PipelineCi<'_> {
        PipelineCi {
            trigger,
            docker_enabled: false,
            external_runners: false,
            allow_host_runner: false,
            jwt_secret: None,
            encryption_key: None,
            external_url: None,
        }
    }

    pub(super) fn repo_path(fixture: &Fixture) -> std::path::PathBuf {
        fixture.repo_root.join(format!(
            "{}/{}.git",
            fixture.owner.username, fixture.repository.name
        ))
    }

    /// Give the fixture a real base branch and a head commit on top of it, so
    /// `ensure_merge_group_ci` gets as far as asking the engine for a pipeline.
    pub(super) async fn make_mergeable(fixture: &Fixture) {
        let repo_path = repo_path(fixture);
        let empty = fixture.sandbox.path().join("empty-tree-src");
        std::fs::write(&empty, b"").expect("write empty file");
        let tree = git()
            .run(
                &["hash-object", "-w", "-t", "tree", &empty.to_string_lossy()],
                Some(&repo_path),
            )
            .expect("hash empty tree");
        tree.ensure_success().expect("hash empty tree");
        let tree = tree.stdout_str().trim().to_string();

        let commit = |args: &[&str]| {
            let out = git()
                .run_with_env(
                    args,
                    Some(&repo_path),
                    &[
                        ("GIT_AUTHOR_NAME", "Queue"),
                        ("GIT_AUTHOR_EMAIL", "queue@example.invalid"),
                        ("GIT_AUTHOR_DATE", "1700000000 +0000"),
                        ("GIT_COMMITTER_NAME", "Queue"),
                        ("GIT_COMMITTER_EMAIL", "queue@example.invalid"),
                        ("GIT_COMMITTER_DATE", "1700000000 +0000"),
                    ],
                )
                .expect("commit-tree");
            out.ensure_success().expect("commit-tree");
            out.stdout_str().trim().to_string()
        };

        let base = commit(&["commit-tree", &tree, "-m", "base"]);
        git()
            .run(&["update-ref", "refs/heads/main", &base], Some(&repo_path))
            .expect("set base branch")
            .ensure_success()
            .expect("set base branch");
        let head = commit(&["commit-tree", &tree, "-p", &base, "-m", "head"]);

        let mut active: pull_request::ActiveModel = fixture.pr.clone().into();
        active.head_sha = Set(Some(head));
        active.update(&fixture.db).await.expect("set head sha");
    }

    /// The queue entry is already committed when the engine refuses, so the
    /// refusal has to settle it rather than escape. It used to leave through
    /// `?`: an enqueue answered `5xx` after a successful enqueue, a review or a
    /// post-push pass logged a warning and nothing else, and the entry sat
    /// there with no verdict for the next pass to rebuild and re-refuse.
    #[tokio::test]
    async fn a_refused_merge_group_workflow_settles_the_queue_entry_with_its_reason() {
        let fixture = fixture("refused-config").await;
        make_mergeable(&fixture).await;

        let result = completed(
            process_repository_with_ci(
                &fixture.db,
                &fixture.repo_root,
                &fixture.repository,
                &ci(&RefusingMergeGroupCi),
            )
            .await,
            "a configuration the repository owns is not a queue-run failure",
        );

        assert_eq!(
            result.failed,
            vec![fixture.pr.id],
            "the pass reports the PR as failed rather than propagating: {result:?}"
        );

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("the entry is still there");
        assert_eq!(
            entry.status, "failed",
            "the attempt is settled, not waiting"
        );
        assert_eq!(
            entry.failure_reason.as_deref(),
            Some(REFUSAL),
            "the safe reason is durable on the entry the user is looking at"
        );

        // And the same record push and PR sync publish: a terminal run whose
        // job log names the workflow and the key.
        let (pipelines, _) = rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(
            &fixture.db,
            fixture.repository.id,
            0,
            50,
        )
        .await
        .expect("list pipelines");
        let refused = pipelines
            .iter()
            .find(|pipeline| pipeline.trigger_type == "merge_group")
            .unwrap_or_else(|| panic!("no merge_group pipeline was published: {pipelines:?}"));
        assert_eq!(refused.status, "failed");
        // card_32422b3fdab1: the row carries the event context the producer was
        // holding, because `retry` takes it like any other and a merge-group run
        // shares the `on: pull_request` filter — retried without the branch the
        // queue merges into, it is judged against the default branch instead.
        assert_eq!(
            refused.base_branch.as_deref(),
            Some(fixture.pr.base_branch.as_str()),
            "the diagnostic row did not record the branch the queue was merging into"
        );
        assert_eq!(
            refused.previous_sha, None,
            "the group commit is built fresh for the run, so there is no previous revision to claim"
        );
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&fixture.db, refused.id)
            .await
            .expect("list jobs");
        let log = jobs
            .first()
            .and_then(|job| job.log.clone())
            .unwrap_or_default();
        assert!(
            log.contains(REFUSAL),
            "the run must carry the reason: {log}"
        );
    }

    /// Storage giving out is not a repository mistake: it stays retryable, it
    /// does not settle the entry, and its text — which names an operator path —
    /// never reaches the queue page.
    #[tokio::test]
    async fn an_infrastructure_failure_is_not_reported_as_a_configuration_mistake() {
        let fixture = fixture("broken-storage").await;
        make_mergeable(&fixture).await;

        let error = process_repository_with_ci(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &ci(&BrokenStorageCi),
        )
        .await
        .error
        .expect("an infrastructure failure is still the caller's to handle");
        assert!(
            format!("{error:#}").contains("pipeline insert failed"),
            "the operator keeps the real cause: {error:#}"
        );

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("the entry is still there");
        assert_eq!(
            entry.status, "queued",
            "a retryable failure must not settle the attempt"
        );
        assert_eq!(
            entry.failure_reason, None,
            "an operator path must not be published as the author's mistake"
        );
    }

    #[tokio::test]
    async fn a_merge_group_with_no_selected_jobs_is_ready_without_a_pipeline() {
        let fixture = fixture("no-matching-jobs").await;
        make_mergeable(&fixture).await;

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("entry exists");
        let pr = pull_request_ops::find_by_id(&fixture.db, fixture.pr.id)
            .await
            .expect("read pull request")
            .expect("pull request exists");
        let state = ensure_merge_group_ci(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &entry,
            &pr,
            &ci(&NoMatchingJobsCi),
        )
        .await
        .expect("a valid no-match outcome is not a queue-run failure");

        assert!(
            matches!(state, MergeGroupState::Ready),
            "the queue must not wait for a pipeline the config deliberately omitted"
        );
        let (_, total) = rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(
            &fixture.db,
            fixture.repository.id,
            0,
            50,
        )
        .await
        .expect("list pipelines");
        assert_eq!(total, 0, "no-match published a merge-group pipeline");
    }
}

/// card_1dc8db5f5eb3: the two state refusals at the top of [`enqueue`] used to
/// be bare `bail!`s, so `PUT .../merge-queue` answered `500` to a closed PR, to
/// a draft, and to a pull request addressed through the wrong repository —
/// while `/merge`, the other entrance to the same merge, already answered `409`
/// to the very same draft.
///
/// These assert the error *type* rather than a status code, because the type is
/// what the HTTP funnel classifies on: a message that happens to read right but
/// travels as a plain `anyhow::Error` is exactly the defect.
#[cfg(test)]
mod enqueue_state_refusal_tests {
    use super::merge_group_ref_cleanup_tests::fixture;
    use super::*;
    use sea_orm::ActiveModelTrait;

    async fn enqueued_events(db: &DatabaseConnection, pr_id: i64) -> u64 {
        use sea_orm::{ColumnTrait, PaginatorTrait, QueryFilter};
        rg_db::entities::pr_event::Entity::find()
            .filter(rg_db::entities::pr_event::Column::PrId.eq(pr_id))
            .filter(rg_db::entities::pr_event::Column::EventType.eq("merge_queue_enqueued"))
            .count(db)
            .await
            .expect("count merge-queue enqueue events")
    }

    #[tokio::test]
    async fn a_closed_pull_request_is_a_conflict_not_a_server_failure() {
        let fixture = fixture("queue-closed-state").await;
        let mut active: pull_request::ActiveModel = fixture.pr.clone().into();
        active.state = Set("closed".into());
        let pr = active.update(&fixture.db).await.expect("close the PR");

        let error = enqueue(
            &fixture.db,
            &fixture.repository,
            &pr,
            fixture.owner.id,
            MergeStrategy::Merge,
        )
        .await
        .expect_err("a closed pull request must not enter the queue");

        let conflict = error
            .downcast_ref::<crate::error::Conflict>()
            .unwrap_or_else(|| panic!("a closed PR must travel as Conflict, got: {error:#}"));
        assert!(
            conflict.message.contains("open pull request"),
            "the 409 body must name the state, got: {}",
            conflict.message
        );
        assert_eq!(
            enqueued_events(&fixture.db, pr.id).await,
            0,
            "the refused enqueue published merge_queue_enqueued anyway"
        );
    }

    #[tokio::test]
    async fn a_draft_pull_request_is_a_conflict_not_a_server_failure() {
        let fixture = fixture("queue-draft-state").await;
        let mut active: pull_request::ActiveModel = fixture.pr.clone().into();
        active.is_draft = Set(true);
        let pr = active.update(&fixture.db).await.expect("draft the PR");

        let error = enqueue(
            &fixture.db,
            &fixture.repository,
            &pr,
            fixture.owner.id,
            MergeStrategy::Merge,
        )
        .await
        .expect_err("a draft pull request must not enter the queue");

        let conflict = error
            .downcast_ref::<crate::error::Conflict>()
            .unwrap_or_else(|| panic!("a draft PR must travel as Conflict, got: {error:#}"));
        assert!(
            conflict.message.contains("draft"),
            "the 409 body must name the state, got: {}",
            conflict.message
        );
        assert_eq!(
            enqueued_events(&fixture.db, pr.id).await,
            0,
            "the refused enqueue published merge_queue_enqueued anyway"
        );
    }

    /// The third refusal is a different answer on purpose: the request is not
    /// waiting on a state change, the pull request just does not live in the
    /// repository it was addressed through. `404`, like the cascade branch
    /// further down the same function.
    #[tokio::test]
    async fn a_pull_request_from_another_repository_is_typed_absence() {
        let fixture = fixture("queue-foreign-repo").await;
        let other = crate::repo::service::create_repo(
            &fixture.db,
            fixture.owner.id,
            "queue-foreign-repo-other",
            None,
            false,
            &fixture.repo_root,
            None,
        )
        .await
        .expect("create the second repository");

        let error = enqueue(
            &fixture.db,
            &other,
            &fixture.pr,
            fixture.owner.id,
            MergeStrategy::Merge,
        )
        .await
        .expect_err("a foreign pull request must not enter this repository's queue");

        let not_found = error
            .downcast_ref::<crate::error::NotFound>()
            .unwrap_or_else(|| {
                panic!("a foreign pull request must travel as NotFound, got: {error:#}")
            });
        assert_eq!(not_found.resource, "pull request");
        assert!(
            error.downcast_ref::<crate::error::Conflict>().is_none(),
            "a foreign pull request is absence, not a state to wait out"
        );
    }
}

/// The queue's verdict on a merge group is shown to everyone who can see the
/// pull request: `finish_entry` stores it on the entry and records it as a PR
/// event, and the review timeline renders that event's body verbatim. These
/// tests hold the two halves apart — a conflict settles the attempt with fixed
/// text, a merge git could not run stays the caller's retryable error — and hold
/// git's own account of either to the operator log (card_3b467917ce10, H-05).
#[cfg(test)]
mod merge_group_conflict_reason_tests {
    use super::merge_group_ref_cleanup_tests::{
        capture_warnings, completed, fixture, git, Fixture,
    };
    use super::*;
    use crate::ci::CiTrigger;
    use sea_orm::ActiveModelTrait;

    /// The merge-tree gate runs before the queue asks about CI, so nothing here
    /// is ever reached — being asked at all would mean the group was built.
    struct UnusedCi;

    impl CiTrigger for UnusedCi {
        fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
            unreachable!("the merge group never builds, so CI is never consulted")
        }

        fn has_workflow_for_event(&self, _query: crate::ci::WorkflowEventQuery<'_>) -> bool {
            unreachable!("the merge group never builds, so CI is never consulted")
        }

        fn trigger_pipeline<'a>(
            &'a self,
            _params: crate::ci::TriggerPipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
            unreachable!("the merge group never builds, so CI is never consulted")
        }

        fn resume_pipeline<'a>(
            &'a self,
            _params: crate::ci::ResumePipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
            unreachable!("the merge group never builds, so CI is never consulted")
        }
    }

    fn ci(trigger: &dyn CiTrigger) -> PipelineCi<'_> {
        PipelineCi {
            trigger,
            docker_enabled: false,
            external_runners: false,
            allow_host_runner: false,
            jwt_secret: None,
            encryption_key: None,
            external_url: None,
        }
    }

    fn repo_path(fixture: &Fixture) -> std::path::PathBuf {
        fixture.repo_root.join(format!(
            "{}/{}.git",
            fixture.owner.username, fixture.repository.name
        ))
    }

    /// A base branch and a PR head that changed the same line of the same file
    /// since their common ancestor — the shape `git merge-tree` answers with a
    /// content conflict.
    async fn make_conflicting(fixture: &Fixture) {
        let repo_path = repo_path(fixture);
        let index = fixture.sandbox.path().join("conflict-index");
        let index_env = index.to_string_lossy().to_string();

        // A bare repository has no worktree to stage from, so each tree is built
        // by hand: hash the blob into the object store, place it in a scratch
        // index by object id, and write the tree out of that index.
        let tree_with = |content: &str, name: &str| {
            let source = fixture.sandbox.path().join(name);
            std::fs::write(&source, content).expect("write blob source");
            let blob = git()
                .run(
                    &["hash-object", "-w", &source.to_string_lossy()],
                    Some(&repo_path),
                )
                .expect("hash blob");
            blob.ensure_success().expect("hash blob");
            let blob = blob.stdout_str().trim().to_string();

            if let Err(error) = std::fs::remove_file(&index) {
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::NotFound,
                    "clear the scratch index between trees: {error}"
                );
            }
            let cacheinfo = format!("100644,{blob},conflicted.txt");
            git()
                .run_with_env(
                    &["update-index", "--add", "--cacheinfo", &cacheinfo],
                    Some(&repo_path),
                    &[("GIT_INDEX_FILE", index_env.as_str())],
                )
                .expect("stage blob")
                .ensure_success()
                .expect("stage blob");
            let tree = git()
                .run_with_env(
                    &["write-tree"],
                    Some(&repo_path),
                    &[("GIT_INDEX_FILE", index_env.as_str())],
                )
                .expect("write tree");
            tree.ensure_success().expect("write tree");
            tree.stdout_str().trim().to_string()
        };

        let commit = |args: &[&str]| {
            let out = git()
                .run_with_env(
                    args,
                    Some(&repo_path),
                    &[
                        ("GIT_AUTHOR_NAME", "Queue"),
                        ("GIT_AUTHOR_EMAIL", "queue@example.invalid"),
                        ("GIT_AUTHOR_DATE", "1700000000 +0000"),
                        ("GIT_COMMITTER_NAME", "Queue"),
                        ("GIT_COMMITTER_EMAIL", "queue@example.invalid"),
                        ("GIT_COMMITTER_DATE", "1700000000 +0000"),
                    ],
                )
                .expect("commit-tree");
            out.ensure_success().expect("commit-tree");
            out.stdout_str().trim().to_string()
        };

        let root_tree = tree_with("one\ntwo\nthree\n", "root-blob");
        let base_tree = tree_with("one\nbase\nthree\n", "base-blob");
        let head_tree = tree_with("one\nhead\nthree\n", "head-blob");

        let root = commit(&["commit-tree", &root_tree, "-m", "root"]);
        let base = commit(&["commit-tree", &base_tree, "-p", &root, "-m", "base"]);
        let head = commit(&["commit-tree", &head_tree, "-p", &root, "-m", "head"]);

        git()
            .run(&["update-ref", "refs/heads/main", &base], Some(&repo_path))
            .expect("set base branch")
            .ensure_success()
            .expect("set base branch");

        let mut active: pull_request::ActiveModel = fixture.pr.clone().into();
        active.head_sha = Set(Some(head));
        active.update(&fixture.db).await.expect("set head sha");
    }

    /// What the author is shown has to describe the state, not quote the
    /// command — this reason is the body of the `merge_queue_failed` event in
    /// the pull request's timeline.
    #[tokio::test]
    async fn a_conflicting_merge_group_settles_with_a_reason_carrying_no_git_output() {
        let fixture = fixture("merge-group-conflict").await;
        make_conflicting(&fixture).await;

        let (logs, guard) = capture_warnings();
        let result = completed(
            process_repository_with_ci(
                &fixture.db,
                &fixture.repo_root,
                &fixture.repository,
                &ci(&UnusedCi),
            )
            .await,
            "a conflicting merge group settles the attempt, it is not a queue-run failure",
        );
        drop(guard);

        assert_eq!(
            result.failed,
            vec![fixture.pr.id],
            "the pass reports the PR as failed: {result:?}"
        );

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("the entry is still there");
        assert_eq!(
            entry.status, "failed",
            "the attempt is settled, not waiting"
        );
        let reason = entry
            .failure_reason
            .clone()
            .expect("the settled attempt carries a reason");
        assert_eq!(reason, MERGE_GROUP_CONFLICT_REASON);

        // Asserted again where the reader actually meets it: the timeline builds
        // its event body out of this record.
        let events = rg_db::ops::pr_event_ops::list_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("list pull-request events");
        let failed = events
            .iter()
            .find(|event| event.event_type == "merge_queue_failed")
            .expect("the settled attempt was recorded as a pull-request event");
        assert_eq!(
            failed.body.as_deref(),
            Some(MERGE_GROUP_CONFLICT_REASON),
            "the timeline event body is git's output, not the queue's verdict"
        );

        for leak in [
            "CONFLICT (",
            "Auto-merging",
            "error:",
            "fatal:",
            "merge-tree:",
            "conflicted.txt",
            "100644",
        ] {
            assert!(
                !reason.contains(leak),
                "git's own output reached the pull request timeline through {leak:?}: {reason}"
            );
        }
        assert!(
            !reason.contains(&*fixture.repo_root.to_string_lossy()),
            "a server-side path reached the pull request timeline: {reason}"
        );

        // And the diagnostic is not lost — it is where an operator can read it.
        let rendered = logs.rendered();
        assert!(
            rendered.contains("CONFLICT (content)") && rendered.contains("conflicted.txt"),
            "git's account of the conflict must survive in the operator log: {rendered}"
        );
    }

    /// The other half. `git merge-tree` exits 1 for an argument it cannot merge
    /// exactly as it does for a real conflict, so a queue that reads the exit
    /// code alone tells the author their pull request conflicts when git in fact
    /// never merged anything.
    #[tokio::test]
    async fn a_merge_tree_that_never_ran_is_not_reported_as_a_conflict() {
        let fixture = fixture("merge-group-unmergeable").await;
        make_conflicting(&fixture).await;

        // A well-formed object id the repository does not have: git refuses it,
        // writes no tree, and still exits 1.
        let mut active: pull_request::ActiveModel = fixture.pr.clone().into();
        active.head_sha = Set(Some("0".repeat(40)));
        active
            .update(&fixture.db)
            .await
            .expect("point the PR at an absent head");

        let error = process_repository_with_ci(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &ci(&UnusedCi),
        )
        .await
        .error
        .expect("a merge git could not run is still the caller's to handle");
        assert!(
            format!("{error:#}").contains("not something we can merge"),
            "the operator keeps the real cause: {error:#}"
        );

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("the entry is still there");
        assert_eq!(
            entry.status, "queued",
            "a merge that never ran is retryable and must not settle the attempt"
        );
        assert_eq!(
            entry.failure_reason, None,
            "the author must not be told their pull request conflicts when git never merged it"
        );
    }
}

/// A merge the queue could not perform ends up in the pull request's timeline:
/// `finish_entry` records the reason as the body of the `merge_queue_failed`
/// event, and the review timeline renders that body verbatim. These tests hold
/// the two halves of a failed `merge_pr` apart — a state the author can act on
/// keeps its own words, everything else settles with fixed text and leaves the
/// chain in the operator log (card_ff19d8130b49, H-05).
#[cfg(test)]
mod merge_failure_reason_tests {
    use super::merge_group_config_refusal_tests::{ci, make_mergeable, repo_path};
    use super::merge_group_ref_cleanup_tests::{capture_warnings, completed, fixture};
    use super::*;
    use crate::ci::CiTrigger;
    use sea_orm::ActiveModelTrait;

    /// The merge group builds and needs no pipeline, so the queue goes straight
    /// on to the merge.
    struct NoCi;

    impl CiTrigger for NoCi {
        fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
            false
        }

        fn has_workflow_for_event(&self, _query: crate::ci::WorkflowEventQuery<'_>) -> bool {
            unreachable!("a repository with no CI config is never asked about an event")
        }

        fn trigger_pipeline<'a>(
            &'a self,
            _params: crate::ci::TriggerPipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
            unreachable!("a repository with no CI config triggers no pipeline")
        }

        fn resume_pipeline<'a>(
            &'a self,
            _params: crate::ci::ResumePipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
            unreachable!("the test never resumes a pipeline")
        }
    }

    /// Nothing is wrong with the merge group; the repository stops being
    /// readable between the group check and the merge itself.
    ///
    /// `has_ci_config` is the last question the queue asks before it claims the
    /// entry and calls `merge_pr`, which makes it the seam where a failure that
    /// belongs to the *merge* can be injected. Any of the class would do — a
    /// killed process, an unreadable object store, a volume that went away — and
    /// a repository directory that is gone is the one whose message carries a
    /// server-side absolute path.
    struct RepositoryVanishesBeforeTheMerge {
        repo_path: std::path::PathBuf,
    }

    impl CiTrigger for RepositoryVanishesBeforeTheMerge {
        fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
            std::fs::remove_dir_all(&self.repo_path).expect("take the repository away");
            false
        }

        fn has_workflow_for_event(&self, _query: crate::ci::WorkflowEventQuery<'_>) -> bool {
            unreachable!("a repository with no CI config is never asked about an event")
        }

        fn trigger_pipeline<'a>(
            &'a self,
            _params: crate::ci::TriggerPipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
            unreachable!("a repository with no CI config triggers no pipeline")
        }

        fn resume_pipeline<'a>(
            &'a self,
            _params: crate::ci::ResumePipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
            unreachable!("the test never resumes a pipeline")
        }
    }

    fn timeline_body(events: &[rg_db::entities::pr_event::Model]) -> Option<&str> {
        events
            .iter()
            .find(|event| event.event_type == "merge_queue_failed")
            .expect("the settled attempt was recorded as a pull-request event")
            .body
            .as_deref()
    }

    /// The failure is the server's, so what the author is shown must describe
    /// that and nothing else — this reason is the body of the
    /// `merge_queue_failed` event in the pull request's timeline.
    #[tokio::test]
    async fn a_merge_that_failed_inside_the_server_settles_with_a_reason_carrying_no_operator_detail(
    ) {
        let fixture = fixture("merge-failed-internally").await;
        make_mergeable(&fixture).await;
        let repo_path = repo_path(&fixture);

        let (logs, guard) = capture_warnings();
        let result = completed(
            process_repository_with_ci(
                &fixture.db,
                &fixture.repo_root,
                &fixture.repository,
                &ci(&RepositoryVanishesBeforeTheMerge {
                    repo_path: repo_path.clone(),
                }),
            )
            .await,
            "a merge that failed settles the attempt, it is not a queue-run failure",
        );
        drop(guard);

        assert_eq!(
            result.failed,
            vec![fixture.pr.id],
            "the pass reports the PR as failed: {result:?}"
        );

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("the entry is still there");
        assert_eq!(
            entry.status, "failed",
            "the attempt is settled, not waiting"
        );
        let reason = entry
            .failure_reason
            .clone()
            .expect("the settled attempt carries a reason");
        assert_eq!(reason, MERGE_FAILED_REASON);

        // Asserted again where the reader actually meets it: the timeline builds
        // its event body out of this record.
        let events = rg_db::ops::pr_event_ops::list_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("list pull-request events");
        assert_eq!(
            timeline_body(&events),
            Some(MERGE_FAILED_REASON),
            "the timeline event body is the merge's own error, not the queue's verdict"
        );

        assert!(
            !reason.contains('/'),
            "a server-side path reached the pull request timeline: {reason}"
        );
        for leak in ["does not exist", "git ", ".git", "fatal:", "error:"] {
            assert!(
                !reason.contains(leak),
                "the merge's internal error reached the pull request timeline through {leak:?}: {reason}"
            );
        }

        // And the diagnostic is not lost — it is where an operator can read it.
        // Asserted whole, path included: the best-effort ref cleanup that runs
        // right after names the same directory, so a check for the path alone
        // would pass with this log line gone.
        let rendered = logs.rendered();
        let logged = format!("repository path does not exist: {repo_path:?}");
        assert!(
            rendered.contains(&logged),
            "the merge's own error must survive in the operator log as {logged:?}: {rendered}"
        );
    }

    /// The other half. A state the author can do something about was written for
    /// them — dropping it for the fixed text above would leave the queue page
    /// and the timeline saying "ask an operator" about a pull request whose
    /// merge only ever needed an account that is still allowed to merge.
    #[tokio::test]
    async fn a_state_the_author_can_act_on_keeps_its_own_words() {
        let fixture = fixture("merge-refused-by-state").await;
        make_mergeable(&fixture).await;

        // Enqueued while the account could merge, run after it was deactivated:
        // `merge_pr` revalidates the enqueuer and answers with a typed refusal.
        let mut owner: rg_db::entities::user::ActiveModel = fixture.owner.clone().into();
        owner.is_active = Set(false);
        owner
            .update(&fixture.db)
            .await
            .expect("deactivate the enqueuer");

        let result = completed(
            process_repository_with_ci(
                &fixture.db,
                &fixture.repo_root,
                &fixture.repository,
                &ci(&NoCi),
            )
            .await,
            "a refused merge settles the attempt, it is not a queue-run failure",
        );

        assert_eq!(
            result.failed,
            vec![fixture.pr.id],
            "the pass reports the PR as failed: {result:?}"
        );

        const REFUSAL: &str = "pull request merge requires an active account";
        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("the entry is still there");
        assert_eq!(
            entry.failure_reason.as_deref(),
            Some(REFUSAL),
            "the state the merge refused on is what the author is told"
        );

        let events = rg_db::ops::pr_event_ops::list_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("list pull-request events");
        assert_eq!(
            timeline_body(&events),
            Some(REFUSAL),
            "the timeline carries the same refusal the entry does"
        );
    }
}

/// The queue asks branch protection whether the head may merge, and that one
/// `Err` carries two unrelated answers: a rule that *refused*, which the author
/// can act on, and a rule the server could not *read*, which decided nothing.
/// Only the first is a reason to wait, and only the first was written for
/// anybody who can see the pull request (card_af2abe7904bd, H-05).
#[cfg(test)]
mod branch_protection_check_failure_tests {
    use super::merge_group_config_refusal_tests::ci;
    use super::merge_group_ref_cleanup_tests::{completed, fixture};
    use super::*;
    use crate::ci::CiTrigger;
    use sea_orm::ConnectionTrait;

    /// The head is settled — waiting or failing — before the queue builds a
    /// merge group, so any question asked of CI here is a break in that order.
    struct NeverAskedCi;

    impl CiTrigger for NeverAskedCi {
        fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
            unreachable!("the head never got past branch protection")
        }

        fn has_workflow_for_event(&self, _query: crate::ci::WorkflowEventQuery<'_>) -> bool {
            unreachable!("the head never got past branch protection")
        }

        fn trigger_pipeline<'a>(
            &'a self,
            _params: crate::ci::TriggerPipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
            unreachable!("the head never got past branch protection")
        }

        fn resume_pipeline<'a>(
            &'a self,
            _params: crate::ci::ResumePipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
            unreachable!("the head never got past branch protection")
        }
    }

    /// The defect: a failed read of the protection rule used to become the
    /// queue's answer to "why is my pull request stuck".
    #[tokio::test]
    async fn a_protection_rule_that_could_not_be_read_fails_the_queue_run() {
        let fixture = fixture("protection-unreadable").await;

        // Any failure of the read would do — an unreachable database, a dropped
        // pool. The table going away is the one whose message is unmistakably
        // internal, and it is where `find_by_repo_and_branch` puts its context.
        fixture
            .db
            .execute_unprepared("DROP TABLE protected_branches")
            .await
            .expect("take branch-protection storage away");

        let error = process_repository_with_ci(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &ci(&NeverAskedCi),
        )
        .await
        .error
        .expect("a check that never ran is not a queue run that succeeded");

        // The chain survives to the caller, which is what logs it: `AppError`'s
        // `{:#}` funnel on the enqueue route, a `tracing::warn!` on the review
        // hook. Nothing of it was handed to the author on the way.
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("branch protection of 'main' could not be checked"),
            "the queue must name what it could not check: {rendered}"
        );
        assert!(
            rendered.contains("db: find protected branch by repo and branch"),
            "the failed read must reach the operator whole: {rendered}"
        );

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("the entry is still there");
        assert_eq!(
            entry.status, "queued",
            "a check that never ran settles nothing: {entry:?}"
        );
        assert_eq!(
            entry.failure_reason, None,
            "the entry must carry no verdict: {entry:?}"
        );
    }

    /// The other half, and the regression card_a997f30c142c left behind: a rule
    /// that genuinely refuses is the answer the author is waiting for, and it
    /// reaches them in the words branch protection wrote.
    #[tokio::test]
    async fn a_rule_that_refused_still_answers_in_its_own_words() {
        let fixture = fixture("protection-refuses").await;
        crate::branch_protection::service::create_protection(
            &fixture.db,
            &fixture.owner.username,
            &fixture.repository.name,
            "main".to_string(),
            false,
            false,
            None,
            true,
            Some(2),
            false,
            false,
            None,
        )
        .await
        .expect("protect the base branch");

        let result = completed(
            process_repository_with_ci(
                &fixture.db,
                &fixture.repo_root,
                &fixture.repository,
                &ci(&NeverAskedCi),
            )
            .await,
            "a rule that refused is not a queue-run failure",
        );

        assert_eq!(
            result.waiting_reason.as_deref(),
            Some("merging into protected branch 'main' requires at least 2 approval(s), got 0"),
            "the refusal reaches the author naming the rule and the count: {result:?}"
        );
        assert!(
            result.failed.is_empty() && result.merged.is_empty(),
            "an unmet condition settles nothing: {result:?}"
        );

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read entry")
            .expect("the entry is still there");
        assert_eq!(
            entry.status, "queued",
            "the pull request waits for the approvals: {entry:?}"
        );
    }
}

/// card_a0332b45eccd: a merge the queue has already made must reach the caller,
/// whatever settling its entry does.
///
/// When `service::merge_pr` answers `Ok`, the merge commit is the tip of
/// `refs/heads/<base>` and no `?` can take that back.
/// [`MergeQueueProcessResult::merged_ref_updates`] is the only place the caller
/// hears about that move — `pulls::enqueue_merge_queue`, the review path and the
/// post-push pass all feed it to `push_hooks::post_push_hooks`. Recording it
/// used to sit *inside* `if finish_entry(..).await?`, so both ways settlement
/// can go wrong took the ref move with it: a failed write left through the `?`
/// before the `extend`, and a `false` skipped it outright. The branch had moved
/// either way, and no pipeline, webhook or watcher was ever told.
#[cfg(test)]
mod merged_ref_survives_entry_settlement_tests {
    use super::merge_group_config_refusal_tests::ci;
    use super::merge_group_ref_cleanup_tests::{fixture, git, Fixture};
    use super::*;
    use crate::ci::CiTrigger;
    use sea_orm::{ActiveModelTrait, ConnectionTrait};

    /// A repository that declares no workflows: `ensure_merge_group_ci` answers
    /// `Ready` without building a pipeline, which puts the pass straight on the
    /// merge — the only part of it this module is about.
    struct NoCiConfig;

    impl CiTrigger for NoCiConfig {
        fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
            false
        }

        fn has_workflow_for_event(&self, _query: crate::ci::WorkflowEventQuery<'_>) -> bool {
            false
        }

        fn trigger_pipeline<'a>(
            &'a self,
            _params: crate::ci::TriggerPipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
            unreachable!("a repository with no CI config never triggers a pipeline")
        }

        fn resume_pipeline<'a>(
            &'a self,
            _params: crate::ci::ResumePipelineParams<'a>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
            unreachable!("the test never resumes a pipeline")
        }
    }

    fn repo_path(fixture: &Fixture) -> std::path::PathBuf {
        fixture.repo_root.join(format!(
            "{}/{}.git",
            fixture.owner.username, fixture.repository.name
        ))
    }

    fn rev_parse(fixture: &Fixture, revision: &str) -> String {
        let output = git()
            .run(&["rev-parse", revision], Some(&repo_path(fixture)))
            .expect("run git rev-parse");
        output.ensure_success().expect("git rev-parse succeeded");
        output.stdout_str().trim().to_string()
    }

    /// Give the fixture's pull request two real branches — `main` and a
    /// `feature` one commit ahead of it — so the queue reaches a merge that
    /// actually moves `refs/heads/main`. Answers with the base tip as it stood
    /// before, the `before` half of the ref move under test.
    async fn make_mergeable(fixture: &Fixture) -> String {
        let repo_path = repo_path(fixture);
        let empty = fixture.sandbox.path().join("empty-tree-src");
        std::fs::write(&empty, b"").expect("write empty file");
        let tree = git()
            .run(
                &["hash-object", "-w", "-t", "tree", &empty.to_string_lossy()],
                Some(&repo_path),
            )
            .expect("hash empty tree");
        tree.ensure_success().expect("hash empty tree");
        let tree = tree.stdout_str().trim().to_string();

        let commit = |args: &[&str]| {
            let out = git()
                .run_with_env(
                    args,
                    Some(&repo_path),
                    &[
                        ("GIT_AUTHOR_NAME", "Queue"),
                        ("GIT_AUTHOR_EMAIL", "queue@example.invalid"),
                        ("GIT_AUTHOR_DATE", "1700000000 +0000"),
                        ("GIT_COMMITTER_NAME", "Queue"),
                        ("GIT_COMMITTER_EMAIL", "queue@example.invalid"),
                        ("GIT_COMMITTER_DATE", "1700000000 +0000"),
                    ],
                )
                .expect("commit-tree");
            out.ensure_success().expect("commit-tree");
            out.stdout_str().trim().to_string()
        };

        let base = commit(&["commit-tree", &tree, "-m", "base"]);
        let head = commit(&["commit-tree", &tree, "-p", &base, "-m", "head"]);
        for (name, sha) in [("refs/heads/main", &base), ("refs/heads/feature", &head)] {
            git()
                .run(&["update-ref", name, sha], Some(&repo_path))
                .expect("publish branch")
                .ensure_success()
                .expect("publish branch");
        }

        let mut active: pull_request::ActiveModel = fixture.pr.clone().into();
        active.head_sha = Set(Some(head));
        active.update(&fixture.db).await.expect("set head sha");
        base
    }

    /// The whole point of `merged_ref_updates`: the caller must be handed the
    /// base branch's move, and that move must describe the repository as it now
    /// is.
    fn assert_hooks_can_run(
        result: &MergeQueueProcessResult,
        fixture: &Fixture,
        base_before: &str,
    ) {
        let [moved] = result.merged_ref_updates.as_slice() else {
            panic!(
                "the queue merged one pull request, so one ref move is owed to the hooks: {:?}",
                result.merged_ref_updates
            );
        };
        assert_eq!(
            moved.repo_id, fixture.repository.id,
            "the move belongs to the repository whose queue ran"
        );
        assert_eq!(
            moved.update.refname, "refs/heads/main",
            "the hooks are owed the base branch's own ref"
        );
        assert_eq!(
            moved.update.old_sha, base_before,
            "the `before` half must be the tip read before the merge"
        );
        assert_eq!(
            moved.update.new_sha,
            rev_parse(fixture, "refs/heads/main"),
            "the ref update must describe a move the repository really made"
        );
        assert_ne!(
            moved.update.new_sha, base_before,
            "the base branch really did move, which is why the hooks are owed it"
        );
    }

    /// The timeline write is the last thing between settling the entry and the
    /// pass carrying on. A missing `merge_queue_merged` event costs the timeline
    /// a row; it must not cost the branch its post-push hooks.
    #[tokio::test]
    async fn a_broken_timeline_write_still_hands_back_the_queue_merge() {
        let fixture = fixture("queue-merged-ref-timeline").await;
        let base_before = make_mergeable(&fixture).await;

        // Both timeline writes on this path go through `pr_events`: the one
        // `update_pr_merged` makes for the merge itself, already best-effort
        // (card_1cf8a3004b6d), and `finish_entry`'s, which is the subject here.
        fixture
            .db
            .execute_unprepared("DROP TABLE pr_events;")
            .await
            .expect("break the timeline table");

        let run = process_repository_with_ci(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &ci(&NoCiConfig),
        )
        .await;

        assert!(
            run.error.is_none(),
            "the entry was settled and only its timeline row was lost: {:?}",
            run.error.map(|error| format!("{error:#}"))
        );
        assert_hooks_can_run(&run.done, &fixture, &base_before);
        assert_eq!(
            run.done.merged,
            vec![fixture.pr.id],
            "the entry itself finished, so the pass reports the merge: {:?}",
            run.done
        );

        let entry = merge_queue_ops::find_by_pr(&fixture.db, fixture.pr.id)
            .await
            .expect("read the queue entry back");
        assert!(
            entry.is_none_or(|entry| entry.status == "merged"),
            "the entry is settled, whatever its timeline lost"
        );
    }

    /// The other way settlement goes wrong, and the quiet one: `finish` matches
    /// no row and answers `false`. The attempt this worker was holding is no
    /// longer the row's — a re-enqueue, another worker — so somebody else owns
    /// the entry now. But this worker is the one that made the merge, and
    /// nobody else holds its ref move.
    ///
    /// This half never raised anything: the pass carried on and reported a
    /// clean run with an empty `merged_ref_updates`, so the base branch moved
    /// and every caller was told there was nothing to run hooks for.
    #[tokio::test]
    async fn an_entry_settled_by_someone_else_still_hands_back_the_queue_merge() {
        let fixture = fixture("queue-merged-ref-lost-race").await;
        let base_before = make_mergeable(&fixture).await;

        // `RAISE(IGNORE)` in a `BEFORE UPDATE` trigger skips the row and leaves
        // the statement reporting no rows changed — which is exactly what
        // `merge_queue_ops::finish` reads as "this attempt no longer owns the
        // entry". The claim taken on the way in writes `running` and is left
        // alone, or the merge would never be reached.
        fixture
            .db
            .execute_unprepared(
                "CREATE TRIGGER merge_queue_finish_lost_race BEFORE UPDATE ON merge_queue_entries \
                 WHEN new.status = 'merged' \
                 BEGIN SELECT RAISE(IGNORE); END;",
            )
            .await
            .expect("install the settlement fault");

        let run = process_repository_with_ci(
            &fixture.db,
            &fixture.repo_root,
            &fixture.repository,
            &ci(&NoCiConfig),
        )
        .await;

        assert!(
            run.error.is_none(),
            "losing the entry to another attempt is not a failed pass: {:?}",
            run.error.map(|error| format!("{error:#}"))
        );
        assert_hooks_can_run(&run.done, &fixture, &base_before);
        assert!(
            run.done.merged.is_empty(),
            "this pass did not settle the entry, so it does not claim to have: {:?}",
            run.done
        );
    }
}
