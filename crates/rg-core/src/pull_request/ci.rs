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
    // A fork PR's head is code the base repository's owner has not accepted, and
    // a pipeline carries that repository's CI secrets (`ci_secret_ops` are keyed
    // by `repo_id` and injected into every job). Running it automatically would
    // hand every one of them to anyone who can open a PR. Fork PRs therefore
    // wait for the maintainer action that already exists — enqueueing into the
    // merge queue, which builds the merge group under the same reasoning.
    if let Some(head_repo_id) = pr.head_repo_id {
        tracing::info!(
            pr_id = pr.id,
            head_repo_id,
            "pull_request CI skipped for a fork PR: an unreviewed head must not run with this repository's CI secrets"
        );
        return Ok(None);
    }
    let Some(head_sha) = pr.head_sha.as_deref() else {
        tracing::debug!(pr_id = pr.id, "pull_request CI skipped: PR has no head SHA");
        return Ok(None);
    };

    let repository = repository::Entity::find_by_id(pr.repo_id)
        .one(db)
        .await?
        .context("pull request repository not found")?;
    let namespace = super::service::repository_namespace(db, &repository).await?;
    let repo_path = repo_root.join(format!("{namespace}/{}.git", repository.name));

    let ref_name = pull_request_ref(pr);
    if !ci
        .trigger
        .has_workflow_for_event(crate::ci::WorkflowEventQuery {
            repo_path: &repo_path,
            commit_sha: head_sha,
            event: PULL_REQUEST_EVENT,
            ref_name: &ref_name,
            base_branch: Some(&pr.base_branch),
            // A PR trigger has no "previous revision of this ref" to hand over:
            // the event is about the PR's head, and a path filter falls back to
            // that commit's own diff.
            previous_sha: None,
        })
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
            triggered_by: actor_id,
            docker_enabled: ci.docker_enabled,
            external_runners: ci.external_runners,
            allow_host_runner: ci.allow_host_runner,
            jwt_secret: ci.jwt_secret,
            encryption_key: ci.encryption_key,
            external_url: ci.external_url,
        })
        .await?;

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
        tracing::warn!(
            pr_id = pr.id,
            error = %format!("{error:#}"),
            "failed to trigger the pull_request CI pipeline"
        );
    }
}
