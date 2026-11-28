//! Starting an agent run when a person assigns an issue to a bot (ADR-0001 D1,
//! D3, D6; card_b6a1860baf04).
//!
//! An assignment is a request from whoever made it, and the bot's model key is
//! its owner's. So a run starts only when every check of D6 holds, and every
//! one of them is asked here, in the core, whatever door the assignment came
//! through — the HTTP route already requires write access, but a check that
//! lives only there is a check the next caller of `update_issue` does not
//! make:
//!
//! 1. the instance allows agents at all (`[agents] enabled`);
//! 2. a **person** assigned it — not a bot, not a token issued for a run;
//! 3. that person may write to the repository;
//! 4. the bot has an agent profile, and it is switched on;
//! 5. the profile lists this repository (the owner's consent to spend the key
//!    here, since a repository admin can add anybody's bot as a collaborator);
//! 6. the bot itself may write to the repository;
//! 7. the instance can run it: in Docker (D2), with an image and a command.
//!
//! A refusal is said, not swallowed: the bot comments on the issue with the
//! reason and `agent.run_refused` goes to the audit log. A bot with no profile
//! says so too — unless the instance has agents switched off, in which case an
//! assignment is what it always was. A run that starts records
//! `agent.run_started` and a synthetic `agent_run` pipeline
//! ([`crate::ci::CiTrigger::enqueue_agent_run`]) built from the profile, not
//! from any file in the repository.
//!
//! Not here: the run's token is minted when its job is handed out
//! (`access_token::issue_for_agent_run`, delivered by the runner side), and the
//! daily and iteration limits of D9 (card_b391e92474a0).

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use rg_db::entities::{agent_run, agent_run_attempt, issue, user};

use super::profile::{self, Preset, SavedProfile};
use crate::db::DbPools;

/// The MCP tools a run's token may call (ADR-0001 D3): reading the repository,
/// its issues, pull requests and CI, writing files, opening a pull request and
/// commenting. Deliberately absent: merging, approving a review, editing or
/// labelling issues, filing new ones, requesting reviewers, and retrying or
/// cancelling pipelines.
pub const RUN_MCP_TOOLS: &[&str] = &[
    "read_file",
    "read_dir",
    "get_issue",
    "get_pr",
    "get_pr_diff",
    "list_reviews",
    "list_review_comments",
    "search",
    "ai_repo_summary",
    "ai_list_issues",
    "ai_list_prs",
    "ai_repo_tree",
    "ai_search_code",
    "list_pipelines",
    "get_pipeline",
    "get_ci_job",
    "get_commit_status",
    "write_file",
    "create_pr",
    "comment_issue",
    "create_review_comment",
    "apply_suggestion",
];

/// The refs every run of `bot` may write under: `refs/heads/agent/<bot>/`.
pub fn branch_prefix(bot: &str) -> String {
    format!("refs/heads/agent/{bot}/")
}

/// Whether `prefix` is one a run of `bot` may be given: the bot's own
/// [`branch_prefix`], or one branch under it (an iteration on its pull request).
pub fn prefix_is_the_bots(bot: &str, prefix: &str) -> bool {
    let own = branch_prefix(bot);
    prefix == own
        || (prefix.len() > own.len() && prefix.starts_with(&own) && !prefix.ends_with('/'))
}

/// What starting a run needs from the process: the instance switch and the CI
/// wiring the pipeline is created with.
pub struct AgentRunLauncher<'a> {
    /// `[agents] enabled`.
    pub enabled: bool,
    pub repo_root: &'a Path,
    pub ci: crate::pull_request::ci::PipelineCi<'a>,
}

/// Why an assignment did not start a run. The code goes to the audit log, the
/// message to the issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: &'static str,
    pub message: String,
}

impl Refusal {
    fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

/// What an assignment to a bot came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssignmentOutcome {
    /// The assignee is a person, or agents are off and the bot has no
    /// profile: nothing about this assignment is an agent's business.
    NotAnAgent,
    /// A run was recorded and its pipeline queued.
    Started { run_id: i64, pipeline_id: i64 },
    /// A check of D6 said no; the bot has said why on the issue.
    Refused(Refusal),
    /// Every check passed, but the pipeline could not be created; the run is
    /// recorded as failed and the bot has said so on the issue.
    Failed { run_id: i64, reason: String },
}

/// The run an assignment of `issue` to `assignee_id` by `actor_id` starts, if
/// any (see the module docs).
///
/// Called by `issue::update_issue` once the new assignee is committed. Never
/// fails the assignment: an error here is logged and answered as nothing
/// started.
pub async fn start_for_assignment(
    db: &impl crate::db::DbPools,
    launcher: &AgentRunLauncher<'_>,
    issue: &issue::Model,
    assignee_id: i64,
    actor_id: Option<i64>,
) -> AssignmentOutcome {
    match try_start_for_assignment(db, launcher, issue, assignee_id, actor_id).await {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(
                issue_id = issue.id,
                assignee_id,
                error = %format!("{error:#}"),
                "an assignment to a bot could not be checked for an agent run; nothing was started"
            );
            AssignmentOutcome::NotAnAgent
        }
    }
}

async fn try_start_for_assignment(
    db: &impl crate::db::DbPools,
    launcher: &AgentRunLauncher<'_>,
    issue: &issue::Model,
    assignee_id: i64,
    actor_id: Option<i64>,
) -> Result<AssignmentOutcome> {
    let pools = db.owned();
    let read = pools.read();
    let Some(bot) = rg_db::ops::user_ops::find_by_id(read, assignee_id).await? else {
        return Ok(AssignmentOutcome::NotAnAgent);
    };
    if !bot.is_bot() {
        return Ok(AssignmentOutcome::NotAnAgent);
    }
    let saved = profile::get(read, &bot).await?;
    if !launcher.enabled && saved.is_none() {
        return Ok(AssignmentOutcome::NotAnAgent);
    }
    let repository = rg_db::ops::repo_ops::find_by_id(read, issue.repo_id)
        .await?
        .context("the issue's repository is gone")?;
    let (owner, name) = crate::repo::service::repository_identity(read, repository.id).await?;

    let checked = check_launch(read, launcher, &repository, &bot, saved.as_ref(), actor_id).await?;
    let job = match checked {
        Ok(job) => job,
        Err(refusal) => {
            refuse(&pools, &owner, &name, issue, &bot, actor_id, &refusal).await;
            return Ok(AssignmentOutcome::Refused(refusal));
        }
    };
    let actor_id = actor_id.context("a launch that passed its checks has a person behind it")?;

    // The commit the run starts from: the default branch as it is now, read
    // once, so the pipeline and the run say the same thing.
    let repo_path = launcher.repo_root.join(format!("{owner}/{name}.git"));
    let default_branch = repository.default_branch.clone();
    let head = {
        let repo_path = repo_path.clone();
        let branch = default_branch.clone();
        crate::blocking::run_blocking_git("read the default branch for an agent run", move || {
            crate::repo::service::try_get_branch_sha(&repo_path, &branch)
        })
        .await?
    };
    let Some(commit_sha) = head else {
        let refusal = Refusal::new(
            "empty_repository",
            format!("I cannot start: the default branch `{default_branch}` has no commits yet."),
        );
        refuse(&pools, &owner, &name, issue, &bot, Some(actor_id), &refusal).await;
        return Ok(AssignmentOutcome::Refused(refusal));
    };

    let write = pools.write();
    let now = Utc::now();
    let inserted = agent_run::ActiveModel {
        bot_id: Set(bot.id),
        repository_id: Set(repository.id),
        issue_id: Set(Some(issue.id)),
        pull_request_id: Set(None),
        iteration: Set(1),
        triggered_by: Set(actor_id),
        trigger_key: Set(format!(
            "issue_assigned:{}:{}:{}",
            issue.id,
            bot.id,
            issue.updated_at.timestamp_micros()
        )),
        status: Set("queued".to_string()),
        active: Set(Some(true)),
        result_pull_request_id: Set(None),
        failure_reason: Set(None),
        write_ref_prefix: Set(Some(branch_prefix(&bot.username))),
        created_at: Set(now),
        finished_at: Set(None),
        ..Default::default()
    }
    .insert(write)
    .await;
    let run = match inserted {
        Ok(run) => run,
        // The unique `(repository_id, bot_id, active)` index: another run of
        // this bot is live here, whatever the check above saw a moment ago.
        Err(error) if rg_db::is_unique_violation(&error) => {
            let refusal = already_running();
            refuse(&pools, &owner, &name, issue, &bot, Some(actor_id), &refusal).await;
            return Ok(AssignmentOutcome::Refused(refusal));
        }
        Err(error) => return Err(anyhow::Error::from(error).context("db: record an agent run")),
    };

    // From here on the run row exists and holds the bot's one active slot in
    // this repository. Whatever goes wrong below ends it as failed, so a
    // fault cannot leave an active run that no pipeline carries and that
    // refuses every later assignment.
    let attempt = agent_run_attempt::ActiveModel {
        run_id: Set(run.id),
        attempt: Set(1),
        pipeline_id: Set(None),
        created_at: Set(now),
        finished_at: Set(None),
        ..Default::default()
    }
    .insert(write)
    .await
    .context("db: record the first attempt of an agent run");
    let ci = &launcher.ci;
    let enqueued = match attempt {
        Ok(attempt) => ci
            .trigger
            .enqueue_agent_run(crate::ci::AgentRunPipelineParams {
                db: write,
                repo_root: launcher.repo_root,
                repo_path: &repo_path,
                repo_id: repository.id,
                commit_sha: &commit_sha,
                ref_name: &format!("refs/heads/{default_branch}"),
                image: &job.image,
                command: &job.command,
                runner_tags: &job.runner_tags,
                timeout_secs: job.timeout_secs,
                triggered_by: Some(actor_id),
                docker_enabled: ci.docker_enabled,
                external_runners: ci.external_runners,
                allow_host_runner: ci.allow_host_runner,
                jwt_secret: ci.jwt_secret,
                encryption_key: ci.encryption_key,
                external_url: ci.external_url,
            })
            .await
            .map(|pipeline_id| (attempt, pipeline_id)),
        Err(error) => Err(error),
    };
    let (attempt, pipeline_id) = match enqueued {
        Ok(started) => started,
        Err(error) => {
            let reason = crate::error::client_facing_message(&error)
                .unwrap_or_else(|| "the run's pipeline could not be created".to_string());
            tracing::error!(
                run_id = run.id,
                issue_id = issue.id,
                error = %format!("{error:#}"),
                "an agent run passed its checks, but its pipeline could not be created"
            );
            finish_failed(write, run.id, &reason).await;
            record(
                write,
                Some(actor_id),
                "agent.run_finished",
                repository.id,
                serde_json::json!({
                    "run_id": run.id,
                    "bot": bot.username,
                    "issue": issue.number,
                    "outcome": "failed",
                    "reason": reason,
                }),
            )
            .await;
            comment(
                &pools,
                &owner,
                &name,
                issue,
                &bot,
                format!("I could not start working on this: {reason}."),
            )
            .await;
            return Ok(AssignmentOutcome::Failed {
                run_id: run.id,
                reason,
            });
        }
    };
    // The pipeline is queued and runs whether or not the attempt can name it;
    // a failed write here costs the record its link, not the run.
    let mut linked: agent_run_attempt::ActiveModel = attempt.into();
    linked.pipeline_id = Set(Some(pipeline_id));
    if let Err(error) = linked.update(write).await {
        tracing::error!(
            run_id = run.id,
            pipeline_id,
            error = %format!("{error:#}"),
            "an agent run's pipeline is queued, but its attempt could not record it"
        );
    }

    record(
        write,
        Some(actor_id),
        "agent.run_started",
        repository.id,
        serde_json::json!({
            "run_id": run.id,
            "bot": bot.username,
            "issue": issue.number,
            "pipeline_id": pipeline_id,
            "commit_sha": commit_sha,
        }),
    )
    .await;
    Ok(AssignmentOutcome::Started {
        run_id: run.id,
        pipeline_id,
    })
}

/// What the run's single job executes, once every check has passed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RunJob {
    image: String,
    command: String,
    runner_tags: Vec<String>,
    timeout_secs: i64,
}

fn already_running() -> Refusal {
    Refusal::new(
        "run_in_progress",
        "I am already working on something in this repository; assign me again when that run has finished.",
    )
}

/// Every check of D6, in the order a person would want to hear about them.
///
/// `Ok(Err(..))` is a refusal; `Err(..)` is a check that could not run, which
/// starts nothing and is logged by the caller.
async fn check_launch(
    db: &DatabaseConnection,
    launcher: &AgentRunLauncher<'_>,
    repository: &rg_db::entities::repository::Model,
    bot: &user::Model,
    saved: Option<&SavedProfile>,
    actor_id: Option<i64>,
) -> Result<std::result::Result<RunJob, Refusal>> {
    if !launcher.enabled {
        return Ok(Err(Refusal::new(
            "agents_disabled",
            "I cannot start: agent runs are turned off on this instance.",
        )));
    }

    // 2. A person, not a bot and not a token issued for a run.
    let from_run_token = crate::auth::credential_context::current()
        .is_some_and(|credential| credential.agent_run_attempt_id.is_some());
    let actor = match actor_id {
        Some(id) => rg_db::ops::user_ops::find_by_id(db, id).await?,
        None => None,
    };
    let Some(actor) = actor.filter(|actor| actor.is_usable() && !actor.is_bot() && !from_run_token)
    else {
        return Ok(Err(Refusal::new(
            "not_a_person",
            "I start only when a person assigns me; bots and agent runs cannot.",
        )));
    };
    // 3. With write access.
    if !crate::repo::service::can_write_repo(db, repository, Some(actor.id)).await? {
        return Ok(Err(Refusal::new(
            "assigner_cannot_write",
            format!(
                "I start only for someone with write access to this repository, and @{} has none.",
                actor.username
            ),
        )));
    }

    // 4. A profile, switched on.
    let Some(saved) = saved else {
        return Ok(Err(Refusal::new(
            "no_profile",
            "I have no agent profile, so there is nothing for me to run. My owner can set one up under Settings → Agents.",
        )));
    };
    if !saved.profile.enabled {
        return Ok(Err(Refusal::new(
            "profile_disabled",
            "My agent profile is switched off; my owner can turn it on under Settings → Agents.",
        )));
    }
    // 5. In a repository the owner listed.
    if !saved.repository_ids.contains(&repository.id) {
        return Ok(Err(Refusal::new(
            "repository_not_allowed",
            "My owner has not allowed me to work in this repository.",
        )));
    }
    // 6. The bot may write here itself.
    if !crate::repo::service::can_write_repo(db, repository, Some(bot.id)).await? {
        return Ok(Err(Refusal::new(
            "bot_cannot_write",
            "I need write access to this repository to push a branch; add me as a collaborator with write access.",
        )));
    }

    // 7. Something to run it on.
    let ci = &launcher.ci;
    if !ci.docker_enabled && !ci.external_runners {
        return Ok(Err(Refusal::new(
            "no_docker",
            "I run only in a Docker container, and this instance has neither Docker for CI nor external runners.",
        )));
    }
    let preset = Preset::parse(&saved.profile.preset)?;
    let (Some(image), Some(command)) = (saved.profile.image.clone(), saved.profile.command.clone())
    else {
        return Ok(Err(Refusal::new(
            "preset_not_runnable",
            format!(
                "The {} preset cannot run on this instance yet; my owner can use the custom preset with an image and a command.",
                preset.as_str()
            ),
        )));
    };
    // One active run per bot in a repository (D9). The unique index decides it
    // for good when the run is inserted; asking first lets the common case say
    // so before anything is written.
    let active = agent_run::Entity::find()
        .filter(agent_run::Column::RepositoryId.eq(repository.id))
        .filter(agent_run::Column::BotId.eq(bot.id))
        .filter(agent_run::Column::Active.eq(true))
        .one(db)
        .await
        .context("db: look for an active agent run")?;
    if active.is_some() {
        return Ok(Err(already_running()));
    }

    Ok(Ok(RunJob {
        image,
        command,
        runner_tags: saved.runner_tags(),
        timeout_secs: saved.profile.timeout_secs,
    }))
}

async fn refuse(
    pools: &crate::db::Db,
    owner: &str,
    name: &str,
    issue: &issue::Model,
    bot: &user::Model,
    actor_id: Option<i64>,
    refusal: &Refusal,
) {
    tracing::info!(
        issue_id = issue.id,
        bot = %bot.username,
        reason = refusal.reason,
        "an assignment to a bot started no agent run"
    );
    record(
        pools.write(),
        actor_id,
        "agent.run_refused",
        issue.repo_id,
        serde_json::json!({
            "reason": refusal.reason,
            "bot": bot.username,
            "issue": issue.number,
        }),
    )
    .await;
    comment(pools, owner, name, issue, bot, refusal.message.clone()).await;
}

async fn comment(
    pools: &crate::db::Db,
    owner: &str,
    name: &str,
    issue: &issue::Model,
    bot: &user::Model,
    body: String,
) {
    if let Err(error) =
        crate::issue::service::add_comment(pools, owner, name, issue.number, bot.id, body).await
    {
        tracing::warn!(
            issue_id = issue.id,
            bot = %bot.username,
            error = %format!("{error:#}"),
            "the bot could not say on the issue why its agent run did not start"
        );
    }
}

async fn record(
    db: &DatabaseConnection,
    actor_id: Option<i64>,
    action: &str,
    repo_id: i64,
    details: serde_json::Value,
) {
    let actor = match actor_id {
        Some(id) => crate::audit::AuditActor::resolve_after_the_fact(db, id).await,
        None => crate::audit::AuditActor::none(),
    };
    crate::audit::record(
        db,
        &actor,
        action,
        Some("repo"),
        Some(repo_id),
        None,
        None,
        Some(details),
    )
    .await;
}

async fn finish_failed(db: &DatabaseConnection, run_id: i64, reason: &str) {
    let finished = agent_run::Entity::update_many()
        .col_expr(
            agent_run::Column::Status,
            sea_orm::sea_query::Expr::value("failed"),
        )
        .col_expr(
            agent_run::Column::Active,
            sea_orm::sea_query::Expr::value(Option::<bool>::None),
        )
        .col_expr(
            agent_run::Column::FailureReason,
            sea_orm::sea_query::Expr::value(reason),
        )
        .col_expr(
            agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Utc::now()),
        )
        .filter(agent_run::Column::Id.eq(run_id))
        .exec(db)
        .await;
    if let Err(error) = finished {
        tracing::error!(
            run_id,
            error = %format!("{error:#}"),
            "an agent run whose pipeline could not be created is still recorded as active"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_may_write_its_bots_branches_and_nothing_else() {
        assert!(prefix_is_the_bots("bot", "refs/heads/agent/bot/"));
        assert!(prefix_is_the_bots("bot", "refs/heads/agent/bot/7-fix"));
        for other in [
            "refs/heads/main",
            "refs/heads/agent/",
            "refs/heads/agent/bot",
            "refs/heads/agent/bot/7-fix/",
            "refs/heads/agent/bot-two/",
            "refs/heads/agent/other/1",
            "refs/tags/agent/bot/1",
            "",
        ] {
            assert!(!prefix_is_the_bots("bot", other), "{other} was admitted");
        }
    }

    #[test]
    fn the_run_tools_hold_nothing_that_merges_approves_or_edits_issues() {
        for refused in [
            "merge_pr",
            "create_review",
            "update_issue",
            "set_issue_labels",
            "create_issue",
            "request_reviewers",
            "retry_pipeline",
            "cancel_pipeline",
        ] {
            assert!(!RUN_MCP_TOOLS.contains(&refused), "{refused} is a run tool");
        }
    }
}
