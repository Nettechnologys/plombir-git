//! CI pipelines a pull request itself produces.
//!
//! A workflow that declares `on: pull_request` had no producer at all: every
//! path that reached `trigger_pipeline` named its event `push`, `merge_group`,
//! `manual` or `retry`, so `Workflow::matches_event` — which has handled
//! `pull_request` since the day it was written, with unit tests to prove it —
//! was never asked the question. A repository whose CI lives entirely in
//! `.gitea/workflows/pr.yml` got no pipeline when a PR was opened and none when
//! it was updated (card_074d93bfe327). This module is that missing producer.

use std::path::Path;

use anyhow::{Context, Result};
use sea_orm::{DatabaseConnection, EntityTrait};

use rg_db::entities::{pull_request, repository};

/// This process's CI wiring, as a pipeline-triggering path needs it.
///
/// Shared by the merge queue and by the pull-request trigger below; both hand
/// the same six values to `trigger_pipeline` and neither owns them.
pub struct PipelineCi<'a> {
    pub trigger: &'a dyn crate::ci::CiTrigger,
    pub docker_enabled: bool,
    pub external_runners: bool,
    /// See [`crate::ci::TriggerPipelineParams::allow_host_runner`].
    pub allow_host_runner: bool,
    pub jwt_secret: Option<&'a str>,
    /// See [`crate::ci::TriggerPipelineParams::encryption_key`].
    pub encryption_key: Option<&'a str>,
    pub external_url: Option<&'a str>,
}

/// The event name a pull-request pipeline is created under.
///
/// It is not a label for the database: `Workflow::matches_event` selects
/// workflows by exactly this string, so a producer that invents its own name
/// (the applied-suggestion path once sent `suggestion`) matches nothing and
/// silently creates no pipeline.
pub const PULL_REQUEST_EVENT: &str = "pull_request";

/// The ref a pull-request pipeline is recorded against.
///
/// Deliberately *not* the head branch: a push to that branch already gets its
/// own `push` pipeline, and sharing the ref would put both in the same
/// concurrency group, so `cancel_in_progress` would have them cancel each other.
fn pull_request_ref(pr: &pull_request::Model) -> String {
    format!("refs/pull/{}/head", pr.number)
}

/// Close the producer side of the PR-lifecycle race after publishing a graph.
///
/// The close/merge side cancels every pipeline it can see, but a detached
/// producer can still be awaiting workflow discovery or the database insert
/// when that cancellation query runs. Re-read the PR after publication and
/// compensate only the graph this invocation created. The exact id matters:
/// [`pull_request_ref`] is stable across the PR's lifetime, so canceling by ref
/// here could kill a newer run created after a reopen or head update.
///
/// Returns whether the new pipeline remains a usable run. A failed PR re-read
/// is deliberately non-destructive: without a live state we cannot distinguish
/// stale work from a valid open-PR run, so the uncertainty is logged rather
/// than guessed.
async fn reconcile_published_pull_request_pipeline(
    db: &DatabaseConnection,
    pr: &pull_request::Model,
    produced_head_sha: &str,
    pipeline_id: i64,
) -> bool {
    let current = match rg_db::ops::pull_request_ops::find_by_id(db, pr.id).await {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(
                pr_id = pr.id,
                pr_number = pr.number,
                pipeline_id,
                produced_head_sha,
                error = %format!("{error:#}"),
                "pull_request CI freshness could not be verified after publication; the new pipeline was left running"
            );
            return true;
        }
    };

    let (current_state, current_head_sha) = match current {
        Some(current) => (Some(current.state), current.head_sha),
        None => (None, None),
    };
    if current_state.as_deref() == Some("open")
        && current_head_sha.as_deref() == Some(produced_head_sha)
    {
        return true;
    }

    match rg_db::ops::pipeline_ops::cancel_pipeline_chain(db, pipeline_id).await {
        Ok(true) => tracing::info!(
            pr_id = pr.id,
            pr_number = pr.number,
            pipeline_id,
            produced_head_sha,
            current_state = current_state.as_deref().unwrap_or("<deleted>"),
            current_head_sha = current_head_sha.as_deref().unwrap_or("<none>"),
            "canceled a pull_request pipeline published after its PR snapshot stopped being current"
        ),
        // A runner may have made the graph terminal between publication and
        // compensation. There is no active work left to orphan in that case.
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(
                pr_id = pr.id,
                pr_number = pr.number,
                pipeline_id,
                produced_head_sha,
                current_state = current_state.as_deref().unwrap_or("<deleted>"),
                current_head_sha = current_head_sha.as_deref().unwrap_or("<none>"),
                error = %format!("{error:#}"),
                "pull_request pipeline left running after its published snapshot stopped being current"
            );
            return true;
        }
    }
    false
}

/// Trigger the `pull_request` pipeline for a PR whose head is current.
///
/// Returns the new pipeline's id, or `None` when there is nothing to run — the
/// ordinary outcome, since most repositories declare no `on: pull_request`
/// workflow at all.
///
/// The gate is [`CiTrigger::has_workflow_for_event`](crate::ci::CiTrigger::has_workflow_for_event)
/// rather than `has_ci_config`, and that distinction is the whole safety of this
/// path: `has_ci_config` answers "is there any pipeline definition here", which
/// is a yes for every repository using the native `.forgekeep-ci.yml`. Those
/// repositories would get a second copy of their push pipeline on every PR open
/// and every PR sync, forever.
pub async fn trigger_pull_request_ci(
    db: &DatabaseConnection,
    repo_root: &Path,
    pr: &pull_request::Model,
    actor_id: Option<i64>,
    ci: &PipelineCi<'_>,
) -> Result<Option<i64>> {
    if pr.state != "open" {
        return Ok(None);
    }
    let Some(head_sha) = pr.head_sha.as_deref() else {
        tracing::debug!(pr_id = pr.id, "pull_request CI skipped: PR has no head SHA");
        return Ok(None);
    };
    // A fork PR's head is code the base repository's owner has not accepted, and
    // a pipeline carries that repository's CI secrets (`ci_secret_ops` are keyed
    // by `repo_id` and injected into every job). Running it automatically would
    // hand every one of them to anyone who can open a PR.
    //
    // So it waits for a maintainer to say yes — [`approve_pull_request_ci`] —
    // and the permission is recorded against the head commit rather than against
    // the PR. That is the whole safety of the gate: `approve`, then push
    // something else, and `ci_approved_sha` no longer equals `head_sha`, so this
    // returns to refusing until the new head is approved in turn
    // (card_94834ecee708).
    //
    // Before that gate existed the refusal was unconditional, which meant the
    // one contribution shape that most needs a check — code from someone without
    // write access — was the only one that never got one.
    if let Some(head_repo_id) = pr.head_repo_id {
        if pr.ci_approved_sha.as_deref() != Some(head_sha) {
            tracing::info!(
                pr_id = pr.id,
                head_repo_id,
                approved_sha = pr.ci_approved_sha.as_deref().unwrap_or("<none>"),
                head_sha,
                "pull_request CI held for a fork PR: this head is not approved to run with this repository's CI secrets"
            );
            return Ok(None);
        }
        tracing::info!(
            pr_id = pr.id,
            head_repo_id,
            head_sha,
            approved_by = pr.ci_approved_by,
            "pull_request CI released for a fork PR: a maintainer approved this head"
        );
    }

    let repository = repository::Entity::find_by_id(pr.repo_id)
        .one(db)
        .await?
        .context("pull request repository not found")?;
    let namespace = super::service::repository_namespace(db, &repository).await?;
    let repo_path = repo_root.join(format!("{namespace}/{}.git", repository.name));

    let ref_name = pull_request_ref(pr);
    if !ci
        .trigger
        .has_workflow_for_event_checked(crate::ci::WorkflowEventQuery {
            repo_path: &repo_path,
            commit_sha: head_sha,
            event: PULL_REQUEST_EVENT,
            ref_name: &ref_name,
            base_branch: Some(&pr.base_branch),
            // A PR trigger has no "previous revision of this ref" to hand over:
            // the event is about the PR's head, and a path filter falls back to
            // that commit's own diff.
            previous_sha: None,
        })?
    {
        return Ok(None);
    }

    let pipeline_id = ci
        .trigger
        .trigger_pipeline(crate::ci::TriggerPipelineParams {
            db,
            repo_path: &repo_path,
            repo_id: repository.id,
            commit_sha: head_sha,
            ref_name: &ref_name,
            trigger_type: PULL_REQUEST_EVENT,
            // The `branches:` filter of `on: pull_request` applies to the branch
            // the PR targets, which nothing but this call knows.
            base_branch: Some(&pr.base_branch),
            previous_sha: None,
            inputs: None,
            triggered_by: actor_id,
            docker_enabled: ci.docker_enabled,
            external_runners: ci.external_runners,
            allow_host_runner: ci.allow_host_runner,
            jwt_secret: ci.jwt_secret,
            encryption_key: ci.encryption_key,
            external_url: ci.external_url,
        })
        .await?;

    if !reconcile_published_pull_request_pipeline(db, pr, head_sha, pipeline_id).await {
        return Ok(None);
    }

    tracing::info!(
        pipeline_id,
        pr_id = pr.id,
        pr_number = pr.number,
        base_branch = %pr.base_branch,
        "pull_request CI pipeline triggered"
    );
    Ok(Some(pipeline_id))
}

/// [`trigger_pull_request_ci`], with the failure logged instead of propagated.
///
/// Every caller is a side-effect of something already committed — a PR row, a
/// pushed branch — so a pipeline that could not be created must not turn that
/// into an error the user sees.
pub async fn trigger_pull_request_ci_best_effort(
    db: &DatabaseConnection,
    repo_root: &Path,
    pr: &pull_request::Model,
    actor_id: Option<i64>,
    ci: &PipelineCi<'_>,
) {
    if let Err(error) = trigger_pull_request_ci(db, repo_root, pr, actor_id, ci).await {
        let ref_name = pull_request_ref(pr);
        let recorded = match pr.head_sha.as_deref() {
            Some(head_sha) => {
                crate::ci::publish_configuration_failure(
                    crate::ci::ConfigurationFailureParams {
                        db,
                        repo_id: pr.repo_id,
                        commit_sha: head_sha,
                        ref_name: &ref_name,
                        trigger_type: PULL_REQUEST_EVENT,
                        triggered_by: actor_id,
                    },
                    &error,
                )
                .await
            }
            None => Ok(None),
        };
        match recorded {
            Ok(Some(pipeline_id)) => tracing::warn!(
                pr_id = pr.id,
                pipeline_id,
                error = %format!("{error:#}"),
                "pull_request CI configuration was rejected after the PR operation committed; recorded a failed pipeline"
            ),
            Ok(None) => tracing::warn!(
                pr_id = pr.id,
                error = %format!("{error:#}"),
                "failed to trigger the pull_request CI pipeline"
            ),
            Err(record_error) => tracing::warn!(
                pr_id = pr.id,
                trigger_error = %format!("{error:#}"),
                error = %format!("{record_error:#}"),
                "pull_request CI configuration was rejected, but its failed pipeline could not be recorded"
            ),
        }
    }
}

/// Record a maintainer's permission for this PR's current head to run CI.
///
/// The counterpart of the fork gate in [`trigger_pull_request_ci`]. Returns the
/// reloaded pull request, whose `ci_approved_sha` now names the head that was
/// approved — the caller triggers the pipeline from it.
///
/// Three refusals, and each is a different thing having gone wrong:
///
/// - a PR that is not `open` has nothing left to check;
/// - a PR with no `head_sha` has no commit to approve, which is a repository
///   that has not been walked yet rather than a caller error;
/// - the head moved between the approver reading the PR and this write. That
///   one is a **conflict**, not a bad request: the maintainer approved a diff
///   that is no longer there, and silently stamping the new commit instead is
///   precisely the bypass this gate exists to prevent.
pub async fn approve_pull_request_ci(
    db: &DatabaseConnection,
    pr: &pull_request::Model,
    actor_id: i64,
) -> Result<pull_request::Model> {
    if pr.state != "open" {
        return Err(crate::error::invalid_request(
            "only an open pull request can have its CI approved",
        ));
    }
    let Some(head_sha) = pr.head_sha.as_deref() else {
        return Err(crate::error::invalid_request(
            "this pull request has no head commit to approve",
        ));
    };
    if !rg_db::ops::pull_request_ops::approve_ci_for_head(db, pr.id, head_sha, actor_id).await? {
        return Err(crate::error::conflict(
            "the pull request head moved while the approval was being recorded; re-read it and approve the new head",
        ));
    }
    rg_db::ops::pull_request_ops::find_by_id(db, pr.id)
        .await?
        .context("approved pull request disappeared")
}

/// Cancel the `pull_request` pipelines a PR has stopped needing.
///
/// A `pull_request` pipeline exists to answer one question — "is this PR's head
/// fit to merge?" — and a PR that has left `open` has ended that question
/// without ending the run. Left going, its jobs are handed to real runners and
/// burn real minutes on a branch nobody will merge; and because
/// [`pull_request_ref`] is *stable* across the PR's whole life, a repository
/// that declares `concurrency:` without `cancel_in_progress` then has every
/// later trigger on that ref — a reopen, a push to the head branch — refused
/// outright for an active pipeline that answers a dead question
/// (card_f68eac170fa5).
///
/// Cancelled on the way to **both** terminal states, `closed` and `merged`, and
/// that is a deliberate answer rather than an oversight. A merge does not have
/// to wait for this pipeline — the merge queue builds its own merge-group run,
/// and a force-merge or a repository with no required checks does not wait for
/// anything — so a merge with the PR run still going is a real state, and once
/// the merge has happened nobody will act on that run's verdict either. The
/// common case where CI is what *caused* the merge costs one query and changes
/// nothing: the pipeline is already terminal, so
/// [`find_active_pipelines_by_ref`](rg_db::ops::pipeline_ops::find_active_pipelines_by_ref)
/// does not even return it.
///
/// Best-effort, like the merge queue's [`release_merge_group_pipeline`]: the
/// state change the caller was told about is already committed, and a pipeline
/// that will not cancel must not unwind it. Best-effort is not silent — a
/// failure leaves a live pipeline and nothing else in the system will come back
/// for it, so every failure names the PR, the ref and the full cause chain.
///
/// [`release_merge_group_pipeline`]: super::merge_queue
pub async fn cancel_pull_request_ci(
    db: &DatabaseConnection,
    pr: &pull_request::Model,
    reason: &str,
) {
    let ref_name = pull_request_ref(pr);
    let active = match rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(
        db, pr.repo_id, &ref_name,
    )
    .await
    {
        Ok(pipelines) => pipelines,
        Err(error) => {
            tracing::warn!(
                pr_id = pr.id,
                pr_number = pr.number,
                ref_name = %ref_name,
                reason,
                error = %format!("{error:#}"),
                "pull_request pipelines left running: the PR left `open` and its active runs could not be read"
            );
            return;
        }
    };
    for pipeline in active {
        match rg_db::ops::pipeline_ops::cancel_pipeline_chain(db, pipeline.id).await {
            Ok(true) => tracing::info!(
                pr_id = pr.id,
                pr_number = pr.number,
                pipeline_id = pipeline.id,
                reason,
                "canceled the pull_request pipeline its PR no longer needs"
            ),
            // Already terminal by the time the transaction ran — the ordinary
            // outcome for a PR merged because its CI went green.
            Ok(false) => {}
            Err(error) => tracing::warn!(
                pr_id = pr.id,
                pr_number = pr.number,
                pipeline_id = pipeline.id,
                reason,
                error = %format!("{error:#}"),
                "pull_request pipeline left running: its PR left `open` and nothing else will come back for it"
            ),
        }
    }
}
