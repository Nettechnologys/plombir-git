//! Repository-scoped FIFO merge queue.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use sea_orm::{DatabaseConnection, EntityTrait, Set};

use rg_db::entities::{merge_queue_entry, pull_request, repository};
use rg_db::ops::{merge_queue_ops, pull_request_ops};

use super::service::{self, MergeStrategy};

pub struct MergeQueueCi<'a> {
    pub trigger: &'a dyn crate::ci::CiTrigger,
    pub docker_enabled: bool,
    pub external_runners: bool,
    pub jwt_secret: Option<&'a str>,
    pub external_url: Option<&'a str>,
}

#[derive(Debug, serde::Serialize)]
pub struct MergeQueueProcessResult {
    pub merged: Vec<i64>,
    pub failed: Vec<i64>,
    pub waiting_reason: Option<String>,
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

pub async fn cancel(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    pr: &pull_request::Model,
    actor_id: i64,
) -> Result<bool> {
    let canceled = merge_queue_ops::cancel(db, pr.id).await?;
    if canceled {
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
        cleanup_merge_group_ref(db, repo_root, repository, pr.id).await;
    }
    Ok(canceled)
}

async fn finish_entry(
    db: &DatabaseConnection,
    repo_root: &Path,
    entry: &merge_queue_entry::Model,
    status: &str,
    failure_reason: Option<String>,
) -> Result<merge_queue_entry::Model> {
    let finished = merge_queue_ops::finish(db, entry.id, status, failure_reason.clone()).await?;
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
    if let Ok(Some(repository)) = repository::Entity::find_by_id(entry.repo_id).one(db).await {
        cleanup_merge_group_ref(db, repo_root, &repository, entry.pr_id).await;
    }
    Ok(finished)
}

async fn cleanup_merge_group_ref(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    pr_id: i64,
) {
    let Some(entry) = merge_queue_ops::find_by_pr(db, pr_id).await.ok().flatten() else {
        return;
    };
    if entry.merge_group_sha.is_none() {
        return;
    }
    let Ok(namespace) = service::repository_namespace(db, repository).await else {
        return;
    };
    let repo_path = repo_root.join(format!("{namespace}/{}.git", repository.name));
    let group_ref = format!("refs/merge-queue/{}", entry.id);
    if let Ok(git) = rg_git::cli_gateway::global_gateway().as_ref() {
        if let Ok(output) = git.run(&["update-ref", "-d", &group_ref], Some(&repo_path)) {
            if !output.success() {
                tracing::warn!(entry_id = entry.id, "failed to delete merge-group ref");
            }
        }
    }
}

pub async fn process_repository(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
) -> Result<MergeQueueProcessResult> {
    process_repository_inner(db, repo_root, repository, None).await
}

pub async fn process_repository_with_ci(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    ci: &MergeQueueCi<'_>,
) -> Result<MergeQueueProcessResult> {
    process_repository_inner(db, repo_root, repository, Some(ci)).await
}

async fn process_repository_inner(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    ci: Option<&MergeQueueCi<'_>>,
) -> Result<MergeQueueProcessResult> {
    let namespace = service::repository_namespace(db, repository).await?;
    let mut result = MergeQueueProcessResult {
        merged: Vec::new(),
        failed: Vec::new(),
        waiting_reason: None,
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
                finish_entry(
                    db,
                    repo_root,
                    &entry,
                    "failed",
                    Some("merge-queue worker lease expired".into()),
                )
                .await?;
                result.failed.push(entry.pr_id);
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
            finish_entry(
                db,
                repo_root,
                &entry,
                "failed",
                Some("pull request not found".into()),
            )
            .await?;
            result.failed.push(entry.pr_id);
            continue;
        };
        if pr.state == "merged" {
            finish_entry(db, repo_root, &entry, "merged", None).await?;
            result.merged.push(pr.id);
            continue;
        }
        if pr.state != "open" || pr.is_draft {
            finish_entry(
                db,
                repo_root,
                &entry,
                "failed",
                Some(format!("pull request is {} or draft", pr.state)),
            )
            .await?;
            result.failed.push(pr.id);
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
            result.waiting_reason = Some(error.to_string());
            break;
        }
        if let Some(ci) = ci {
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
            }
        } else if let Some(pipeline_id) = entry.merge_group_pipeline_id {
            let pipeline = rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id).await?;
            match pipeline.as_ref().map(|pipeline| pipeline.status.as_str()) {
                Some("success") => {}
                Some("failed" | "canceled") => {
                    finish_entry(
                        db,
                        repo_root,
                        &entry,
                        "failed",
                        Some("merge-group CI failed".into()),
                    )
                    .await?;
                    result.failed.push(pr.id);
                    continue;
                }
                _ => {
                    result.waiting_reason = Some("merge-group CI is still running".into());
                    break;
                }
            }
        }
        if !merge_queue_ops::claim(db, entry.id).await? {
            result.waiting_reason = Some("merge-queue head was claimed concurrently".into());
            break;
        }

        let strategy = MergeStrategy::parse(&entry.strategy)?;
        match service::merge_pr(
            db,
            repo_root,
            &namespace,
            &repository.name,
            pr.number,
            strategy,
        )
        .await
        {
            Ok(_) => {
                finish_entry(db, repo_root, &entry, "merged", None).await?;
                result.merged.push(pr.id);
            }
            Err(error) => {
                finish_entry(db, repo_root, &entry, "failed", Some(error.to_string())).await?;
                result.failed.push(pr.id);
            }
        }
    }
    Ok(result)
}

enum MergeGroupState {
    Ready,
    Waiting(String),
    Failed,
}

async fn ensure_merge_group_ci(
    db: &DatabaseConnection,
    repo_root: &Path,
    repository: &repository::Model,
    entry: &merge_queue_entry::Model,
    pr: &pull_request::Model,
    ci: &MergeQueueCi<'_>,
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
            return Ok(match pipeline.status.as_str() {
                "success" => MergeGroupState::Ready,
                "failed" | "canceled" => {
                    finish_entry(
                        db,
                        repo_root,
                        entry,
                        "failed",
                        Some(format!(
                            "merge-group pipeline #{} is {}",
                            pipeline.id, pipeline.status
                        )),
                    )
                    .await?;
                    MergeGroupState::Failed
                }
                _ => MergeGroupState::Waiting(format!(
                    "merge-group pipeline #{} is {}",
                    pipeline.id, pipeline.status
                )),
            });
        }
    }

    let tree_output = git.run(
        &["merge-tree", "--write-tree", &base_sha, &head_sha],
        Some(&repo_path),
    )?;
    if !tree_output.success() {
        finish_entry(
            db,
            repo_root,
            entry,
            "failed",
            Some(format!(
                "merge group conflicts: {}",
                tree_output.stderr_str().trim()
            )),
        )
        .await?;
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
    let message = format!("Merge queue group for PR #{}", pr.number);
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
            ("GIT_AUTHOR_NAME", "IronForge Merge Queue"),
            ("GIT_AUTHOR_EMAIL", "merge-queue@ironforge.local"),
            ("GIT_COMMITTER_NAME", "IronForge Merge Queue"),
            ("GIT_COMMITTER_EMAIL", "merge-queue@ironforge.local"),
        ],
    )?;
    commit_output.ensure_success()?;
    let group_sha = commit_output.stdout_str().trim().to_string();
    let group_ref = format!("refs/merge-queue/{}", entry.id);
    git.run(&["update-ref", &group_ref, &group_sha], Some(&repo_path))?
        .ensure_success()?;

    if !ci.trigger.has_ci_config(&repo_path, &group_sha) {
        return Ok(MergeGroupState::Ready);
    }
    let pipeline_id = ci
        .trigger
        .trigger_pipeline(crate::ci::TriggerPipelineParams {
            db,
            repo_path: &repo_path,
            repo_id: repository.id,
            commit_sha: &group_sha,
            ref_name: &group_ref,
            trigger_type: "merge_group",
            triggered_by: Some(entry.enqueued_by_id),
            docker_enabled: ci.docker_enabled,
            external_runners: ci.external_runners,
            jwt_secret: ci.jwt_secret,
            external_url: ci.external_url,
        })
        .await?;
    merge_queue_ops::set_merge_group(db, entry.id, &group_sha, &base_sha, &head_sha, pipeline_id)
        .await?;
    rg_db::ops::pr_event_ops::record(
        db,
        repository.id,
        pr.id,
        None,
        "merge_group_created",
        None,
        serde_json::json!({"commit_sha": group_sha, "pipeline_id": pipeline_id}),
    )
    .await?;
    Ok(MergeGroupState::Waiting(format!(
        "merge-group pipeline #{pipeline_id} is pending"
    )))
}

pub async fn process_for_head_commit(
    db: &DatabaseConnection,
    repo_root: &Path,
    source_repo_id: i64,
    commit_sha: &str,
) -> Result<Vec<MergeQueueProcessResult>> {
    process_for_head_commit_inner(db, repo_root, source_repo_id, commit_sha, None).await
}

pub async fn process_for_head_commit_with_ci(
    db: &DatabaseConnection,
    repo_root: &Path,
    source_repo_id: i64,
    commit_sha: &str,
    ci: &MergeQueueCi<'_>,
) -> Result<Vec<MergeQueueProcessResult>> {
    process_for_head_commit_inner(db, repo_root, source_repo_id, commit_sha, Some(ci)).await
}

async fn process_for_head_commit_inner(
    db: &DatabaseConnection,
    repo_root: &Path,
    source_repo_id: i64,
    commit_sha: &str,
    ci: Option<&MergeQueueCi<'_>>,
) -> Result<Vec<MergeQueueProcessResult>> {
    if let Some(entry) =
        merge_queue_ops::find_by_merge_group_sha(db, source_repo_id, commit_sha).await?
    {
        let repository = repository::Entity::find_by_id(entry.repo_id)
            .one(db)
            .await?
            .context("merge-group repository not found")?;
        let result = match ci {
            Some(ci) => process_repository_with_ci(db, repo_root, &repository, ci).await?,
            None => process_repository(db, repo_root, &repository).await?,
        };
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
        results.push(match ci {
            Some(ci) => process_repository_with_ci(db, repo_root, &repository, ci).await?,
            None => process_repository(db, repo_root, &repository).await?,
        });
    }
    Ok(results)
}
