//! Starting an agent run when a person assigns an issue to a bot, and ending
//! one (ADR-0001 D1, D3, D6, D9; card_b6a1860baf04, card_b391e92474a0).
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
//! 7. the instance can run it: in Docker (D2), with an image and a command;
//! 8. the bot has no other active run here, and a run left today (D9).
//!
//! **A start is never lost and never doubled.** The checks are read before
//! the assignment is written ([`plan_assignment`]); the run itself is written
//! by [`reserve`] *in the assignment's own transaction*, together with the
//! day's allowance it takes. So:
//!
//! - an assignment that committed has its run, or the refusal that says why;
//!   a fault after the commit can delay the run's pipeline, never lose it —
//!   [`recover`] queues every active run whose attempt no pipeline carries;
//! - two assignments racing for the bot's one active slot in a repository, or
//!   for its last run of the day, are decided by the unique index and the
//!   conditional allowance update, not by a check both of them passed;
//! - the attempt names its pipeline in the pipeline's own transaction, so a
//!   second enqueue of one attempt cannot run the agent twice;
//! - the trigger key is unique: the same assignment written twice is one run.
//!
//! Sending the assignee an issue already has is no assignment: the issue page
//! sends it with every milestone change, so it starts, refuses and ends
//! nothing. A bot busy with another issue in the repository refuses.
//!
//! A refusal is said, not swallowed: the bot comments on the issue with the
//! reason and `agent.run_refused` goes to the audit log. A bot with no profile
//! says so too — unless the instance has agents switched off, in which case an
//! assignment is what it always was.
//!
//! **Ending** goes through [`end_run_in`]: the one conditional transition out
//! of `active`, which also finishes the attempts and deletes their tokens in
//! the same statement batch, so of two enders only one does anything.
//! Unassigning the issue, switching the profile off and taking the repository
//! off it end the run as `cancelled`.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set, TransactionTrait,
};

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

/// How old an active run whose attempt no pipeline carries must be before a
/// [`recover`] tick queues it: long enough that the request which wrote it has
/// had every chance to queue it itself.
pub const UNQUEUED_RUN_GRACE: std::time::Duration = std::time::Duration::from_secs(120);

/// How many turns a preset's agent may take in a run of `timeout_secs`: one
/// for every half minute, between 10 and 200 — a backstop beside the timeout,
/// so an agent stuck in a loop ends before it spends the whole run.
pub fn max_turns(timeout_secs: i64) -> i64 {
    (timeout_secs / 30).clamp(10, 200)
}

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

/// The instance's ceilings over every profile (`[agents]`, ADR-0001 D9). A
/// profile may ask for less, never for more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentLimits {
    /// Most runs one bot may start in a UTC day.
    pub max_runs_per_day: i64,
    /// Longest a run may take, in seconds.
    pub max_timeout_secs: i64,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_runs_per_day: profile::MAX_RUNS_PER_DAY,
            max_timeout_secs: profile::MAX_TIMEOUT_SECS,
        }
    }
}

impl AgentLimits {
    fn runs_per_day(&self, saved: &SavedProfile) -> i64 {
        saved.profile.max_runs_per_day.min(self.max_runs_per_day)
    }

    fn timeout_secs(&self, saved: &SavedProfile) -> i64 {
        saved.profile.timeout_secs.min(self.max_timeout_secs)
    }
}

/// What starting a run needs from the process: the instance switch, its
/// ceilings and the CI wiring the pipeline is created with.
pub struct AgentRunLauncher<'a> {
    /// `[agents] enabled`.
    pub enabled: bool,
    /// `[agents]` ceilings.
    pub limits: AgentLimits,
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
    /// A check said no; the bot has said why on the issue.
    Refused(Refusal),
    /// The run was recorded, but its pipeline could not be created; the run is
    /// ended as failed and the bot has said so on the issue.
    Failed { run_id: i64, reason: String },
}

/// Who the bot speaks as, and where.
#[derive(Clone, Debug)]
pub struct Speaker {
    bot: user::Model,
    owner: String,
    name: String,
}

/// An assignment every check of D6 let through, waiting for [`reserve`].
#[derive(Clone, Debug)]
pub struct PlannedRun {
    speaker: Speaker,
    repository_id: i64,
    actor_id: i64,
    job: RunJob,
    commit_sha: String,
    default_branch: String,
    runs_per_day: i64,
    /// The task file, written now: what the issue says at the assignment is
    /// what the run works from (`super::trust`).
    task: String,
}

/// What the checks read before the assignment is written made of it.
#[derive(Clone, Debug)]
pub enum AssignmentPlan {
    NotAnAgent,
    Refuse {
        speaker: Box<Speaker>,
        actor_id: Option<i64>,
        refusal: Refusal,
    },
    Start(Box<PlannedRun>),
}

impl AssignmentPlan {
    /// The run [`reserve`] is to write, if the checks let one through.
    pub fn planned(&self) -> Option<&PlannedRun> {
        match self {
            AssignmentPlan::Start(planned) => Some(planned),
            _ => None,
        }
    }
}

/// What [`reserve`] wrote in the assignment's transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reservation {
    /// The run and its first attempt, waiting for their pipeline.
    Run { run_id: i64, attempt_id: i64 },
    /// The bot's slot or its day was taken by the time the row was written.
    Refused(Refusal),
}

/// What a run that ended in a transaction still has to do once it commits:
/// say so.
#[derive(Clone, Debug)]
pub struct EndedRun {
    run: agent_run::Model,
    status: &'static str,
    reason: String,
}

/// Read every check of D6 for an assignment of `issue` to `assignee_id` by
/// `actor_id`, before the assignment is written.
///
/// Never fails the assignment: a check that cannot run is logged and treated
/// as nothing to start.
pub async fn plan_assignment(
    db: &impl crate::db::DbPools,
    launcher: &AgentRunLauncher<'_>,
    issue: &issue::Model,
    assignee_id: i64,
    actor_id: Option<i64>,
) -> AssignmentPlan {
    match try_plan_assignment(db, launcher, issue, assignee_id, actor_id).await {
        Ok(plan) => plan,
        Err(error) => {
            tracing::error!(
                issue_id = issue.id,
                assignee_id,
                error = %format!("{error:#}"),
                "an assignment to a bot could not be checked for an agent run; nothing will start"
            );
            AssignmentPlan::NotAnAgent
        }
    }
}

async fn try_plan_assignment(
    db: &impl crate::db::DbPools,
    launcher: &AgentRunLauncher<'_>,
    issue: &issue::Model,
    assignee_id: i64,
    actor_id: Option<i64>,
) -> Result<AssignmentPlan> {
    let pools = db.owned();
    let read = pools.read();
    let Some(bot) = rg_db::ops::user_ops::find_by_id(read, assignee_id).await? else {
        return Ok(AssignmentPlan::NotAnAgent);
    };
    if !bot.is_bot() {
        return Ok(AssignmentPlan::NotAnAgent);
    }
    let saved = profile::get(read, &bot).await?;
    if !launcher.enabled && saved.is_none() {
        return Ok(AssignmentPlan::NotAnAgent);
    }
    let repository = rg_db::ops::repo_ops::find_by_id(read, issue.repo_id)
        .await?
        .context("the issue's repository is gone")?;
    let (owner, name) = crate::repo::service::repository_identity(read, repository.id).await?;
    let speaker = Speaker { bot, owner, name };

    let checked = check_launch(
        read,
        launcher,
        &repository,
        &speaker.bot,
        saved.as_ref(),
        actor_id,
    )
    .await?;
    let (job, runs_per_day) = match checked {
        Ok(checked) => checked,
        Err(refusal) => {
            return Ok(AssignmentPlan::Refuse {
                speaker: Box::new(speaker),
                actor_id,
                refusal,
            })
        }
    };
    let actor_id = actor_id.context("a launch that passed its checks has a person behind it")?;

    // The commit the run starts from: the default branch as it is now, read
    // once and recorded on the run, so the pipeline, the checkout and the task
    // file all say the same thing.
    let default_branch = repository.default_branch.clone();
    let head = {
        let repo_path = repo_path(launcher, &speaker);
        let branch = default_branch.clone();
        crate::blocking::run_blocking_git("read the default branch for an agent run", move || {
            crate::repo::service::try_get_branch_sha(&repo_path, &branch)
        })
        .await?
    };
    let Some(commit_sha) = head else {
        return Ok(AssignmentPlan::Refuse {
            speaker: Box::new(speaker),
            actor_id: Some(actor_id),
            refusal: Refusal::new(
                "empty_repository",
                format!(
                    "I cannot start: the default branch `{default_branch}` has no commits yet."
                ),
            ),
        });
    };
    let assigner = rg_db::ops::user_ops::find_by_id(read, actor_id)
        .await?
        .context("the assigner passed the checks and is gone")?;
    let task = super::trust::write_task(
        read,
        super::trust::TaskSource {
            repository: &repository,
            owner: &speaker.owner,
            name: &speaker.name,
            issue,
            bot: &speaker.bot,
            assigner: &assigner,
            commit_sha: &commit_sha,
        },
    )
    .await?;
    Ok(AssignmentPlan::Start(Box::new(PlannedRun {
        speaker,
        repository_id: repository.id,
        actor_id,
        job,
        commit_sha,
        default_branch,
        runs_per_day,
        task,
    })))
}

/// The key the assignment of `issue_id` to `bot_id`, written at
/// `assigned_at`, names its run by. Unique in `agent_runs`.
pub fn assignment_trigger_key(
    issue_id: i64,
    bot_id: i64,
    assigned_at: chrono::DateTime<Utc>,
) -> String {
    format!(
        "issue_assigned:{issue_id}:{bot_id}:{}",
        assigned_at.timestamp_micros()
    )
}

/// Write the run `planned` describes, and the day's allowance it takes, in
/// `txn` — the transaction that writes the assignment (see the module docs).
///
/// Inside a savepoint: a refusal rolls back what it wrote and leaves `txn`
/// usable, so the assignment commits either way. `Err` is a database failure
/// the caller's transaction should fail (and may retry) with.
pub async fn reserve(
    txn: &sea_orm::DatabaseTransaction,
    planned: &PlannedRun,
    issue_id: i64,
    trigger_key: &str,
) -> Result<Reservation> {
    let savepoint = txn
        .begin()
        .await
        .context("db: open a savepoint for an agent run")?;
    let written = write_run(&savepoint, planned, issue_id, trigger_key).await;
    match written {
        Ok(Ok(reserved)) => {
            savepoint
                .commit()
                .await
                .context("db: release the agent run savepoint")?;
            Ok(reserved)
        }
        Ok(Err(refused)) => {
            savepoint
                .rollback()
                .await
                .context("db: roll back a refused agent run")?;
            // The same assignment written once already (a retried commit):
            // that run is this one, not a second one.
            if let Some(existing) = agent_run::Entity::find()
                .filter(agent_run::Column::TriggerKey.eq(trigger_key))
                .one(txn)
                .await
                .context("db: look up an agent run by its trigger")?
            {
                let attempt = rg_db::ops::agent_run_ops::attempts_of_run(txn, existing.id)
                    .await?
                    .into_iter()
                    .last()
                    .context("an agent run has no attempt")?;
                return Ok(Reservation::Run {
                    run_id: existing.id,
                    attempt_id: attempt.id,
                });
            }
            Ok(refused)
        }
        Err(error) => {
            if let Err(rollback_error) = savepoint.rollback().await {
                tracing::warn!(
                    error = %format!("{rollback_error:#}"),
                    "an agent run savepoint could not be rolled back"
                );
            }
            Err(error)
        }
    }
}

/// `Ok(Err(refusal))` when the slot or the day is taken; the caller rolls the
/// savepoint back.
async fn write_run(
    db: &impl ConnectionTrait,
    planned: &PlannedRun,
    issue_id: i64,
    trigger_key: &str,
) -> Result<std::result::Result<Reservation, Reservation>> {
    let bot = &planned.speaker.bot;
    let now = Utc::now();
    let day = now.format("%Y-%m-%d").to_string();
    if !rg_db::ops::agent_run_ops::reserve_daily_run(db, bot.id, &day, planned.runs_per_day).await?
    {
        return Ok(Err(Reservation::Refused(day_spent(planned.runs_per_day))));
    }
    let inserted = agent_run::ActiveModel {
        bot_id: Set(bot.id),
        repository_id: Set(planned.repository_id),
        issue_id: Set(Some(issue_id)),
        pull_request_id: Set(None),
        iteration: Set(1),
        triggered_by: Set(planned.actor_id),
        trigger_key: Set(trigger_key.to_string()),
        status: Set("queued".to_string()),
        active: Set(Some(true)),
        result_pull_request_id: Set(None),
        failure_reason: Set(None),
        write_ref_prefix: Set(Some(branch_prefix(&bot.username))),
        commit_sha: Set(Some(planned.commit_sha.clone())),
        task: Set(Some(planned.task.clone())),
        created_at: Set(now),
        finished_at: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await;
    let run = match inserted {
        Ok(run) => run,
        // The unique `(repository_id, bot_id, active)` index: another run of
        // this bot is live here, whatever the check a moment ago saw. Or the
        // unique trigger key; the caller tells the two apart.
        Err(error) if rg_db::is_unique_violation(&error) => {
            return Ok(Err(Reservation::Refused(already_running())))
        }
        Err(error) => return Err(anyhow::Error::from(error).context("db: record an agent run")),
    };
    let attempt = agent_run_attempt::ActiveModel {
        run_id: Set(run.id),
        attempt: Set(1),
        pipeline_id: Set(None),
        created_at: Set(now),
        finished_at: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
    .context("db: record the first attempt of an agent run")?;
    Ok(Ok(Reservation::Run {
        run_id: run.id,
        attempt_id: attempt.id,
    }))
}

/// Act on an assignment once its transaction committed: queue the run
/// [`reserve`] wrote, or say why there is none.
pub async fn launch(
    db: &impl crate::db::DbPools,
    launcher: &AgentRunLauncher<'_>,
    issue: &issue::Model,
    plan: AssignmentPlan,
    reservation: Option<Reservation>,
) -> AssignmentOutcome {
    let pools = db.owned();
    match (plan, reservation) {
        (AssignmentPlan::NotAnAgent, _) => AssignmentOutcome::NotAnAgent,
        (
            AssignmentPlan::Refuse {
                speaker,
                actor_id,
                refusal,
            },
            _,
        ) => {
            refuse(&pools, &speaker, issue, actor_id, &refusal).await;
            AssignmentOutcome::Refused(refusal)
        }
        (AssignmentPlan::Start(planned), Some(Reservation::Refused(refusal))) => {
            refuse(
                &pools,
                &planned.speaker,
                issue,
                Some(planned.actor_id),
                &refusal,
            )
            .await;
            AssignmentOutcome::Refused(refusal)
        }
        (AssignmentPlan::Start(planned), Some(Reservation::Run { run_id, attempt_id })) => {
            enqueue(&pools, launcher, &planned, issue.number, run_id, attempt_id).await
        }
        (AssignmentPlan::Start(_), None) => {
            tracing::error!(
                issue_id = issue.id,
                "an agent run passed its checks but its assignment wrote no run; nothing started"
            );
            AssignmentOutcome::NotAnAgent
        }
    }
}

fn repo_path(launcher: &AgentRunLauncher<'_>, speaker: &Speaker) -> std::path::PathBuf {
    launcher
        .repo_root
        .join(format!("{}/{}.git", speaker.owner, speaker.name))
}

/// Queue the pipeline of `attempt_id`, and say how that went.
async fn enqueue(
    pools: &crate::db::Db,
    launcher: &AgentRunLauncher<'_>,
    planned: &PlannedRun,
    issue_number: i64,
    run_id: i64,
    attempt_id: i64,
) -> AssignmentOutcome {
    let write = pools.write();
    let ci = &launcher.ci;
    let repo_path = repo_path(launcher, &planned.speaker);
    let enqueued = ci
        .trigger
        .enqueue_agent_run(crate::ci::AgentRunPipelineParams {
            db: write,
            repo_root: launcher.repo_root,
            repo_path: &repo_path,
            repo_id: planned.repository_id,
            commit_sha: &planned.commit_sha,
            ref_name: &format!("refs/heads/{}", planned.default_branch),
            image: &planned.job.image,
            command: &planned.job.command,
            runner_tags: &planned.job.runner_tags,
            timeout_secs: planned.job.timeout_secs,
            triggered_by: Some(planned.actor_id),
            attempt_id,
            docker_enabled: ci.docker_enabled,
            external_runners: ci.external_runners,
            allow_host_runner: ci.allow_host_runner,
            jwt_secret: ci.jwt_secret,
            encryption_key: ci.encryption_key,
            external_url: ci.external_url,
        })
        .await;
    let bot = &planned.speaker.bot;
    match enqueued {
        Ok(pipeline_id) => {
            record(
                write,
                Some(planned.actor_id),
                "agent.run_started",
                planned.repository_id,
                serde_json::json!({
                    "run_id": run_id,
                    "bot": bot.username,
                    "issue": issue_number,
                    "pipeline_id": pipeline_id,
                    "commit_sha": planned.commit_sha,
                }),
            )
            .await;
            AssignmentOutcome::Started {
                run_id,
                pipeline_id,
            }
        }
        Err(error) => {
            let reason = crate::error::client_facing_message(&error)
                .unwrap_or_else(|| "the run's pipeline could not be created".to_string());
            tracing::error!(
                run_id,
                issue_number,
                error = %format!("{error:#}"),
                "an agent run passed its checks, but its pipeline could not be created"
            );
            match end_run(pools, run_id, "failed", &reason).await {
                Ok(Some(ended)) => {
                    after_end(
                        pools,
                        ended,
                        format!("I could not start working on this: {reason}."),
                    )
                    .await
                }
                Ok(None) => {}
                Err(error) => tracing::error!(
                    run_id,
                    error = %format!("{error:#}"),
                    "an agent run whose pipeline could not be created is still recorded as active"
                ),
            }
            AssignmentOutcome::Failed { run_id, reason }
        }
    }
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

fn day_spent(runs_per_day: i64) -> Refusal {
    Refusal::new(
        "daily_limit",
        format!(
            "I have started all {runs_per_day} of my runs for today (UTC); assign me again tomorrow, or ask my owner to raise the limit."
        ),
    )
}

/// The job a saved profile runs, under the instance's ceilings, or why it
/// cannot run here.
fn runnable_job(
    launcher: &AgentRunLauncher<'_>,
    saved: &SavedProfile,
) -> Result<std::result::Result<RunJob, Refusal>> {
    let ci = &launcher.ci;
    if !ci.docker_enabled && !ci.external_runners {
        return Ok(Err(Refusal::new(
            "no_docker",
            "I run only in a Docker container, and this instance has neither Docker for CI nor external runners.",
        )));
    }
    if ci.external_url.is_none() {
        return Ok(Err(Refusal::new(
            "no_server_url",
            "I cannot reach this instance from my container: it has no public address. An admin can set `[server] external_url`.",
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
    Ok(Ok(RunJob {
        image,
        command,
        runner_tags: saved.runner_tags(),
        timeout_secs: launcher.limits.timeout_secs(saved),
    }))
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
) -> Result<std::result::Result<(RunJob, i64), Refusal>> {
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
    let job = match runnable_job(launcher, saved)? {
        Ok(job) => job,
        Err(refusal) => return Ok(Err(refusal)),
    };
    // 8. One active run per bot in a repository (D9). The unique index decides
    // it for good when the run is written; asking first lets the common case
    // say so before anything is written. The day's allowance is only ever
    // decided by the write (see `reserve`).
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

    Ok(Ok((job, launcher.limits.runs_per_day(saved))))
}

// ── Ending a run ───────────────────────────────────────────────────────────

/// End run `run_id` as `status` in `db` (a transaction, or a connection), if
/// it is still active; `None` when something else ended it first.
///
/// One conditional write decides who ends a run; the winner finishes the
/// run's open attempts, deletes their tokens and cancels their pipelines in
/// the same `db`, so a run that is no longer active holds no token that works
/// and no job a runner will start. What needs the commit first — the bot's
/// comment, the audit row — is the returned [`EndedRun`]'s, for
/// [`after_end`].
pub async fn end_run_in(
    db: &impl ConnectionTrait,
    run_id: i64,
    status: &'static str,
    reason: &str,
) -> Result<Option<EndedRun>> {
    let Some(run) = agent_run::Entity::find_by_id(run_id)
        .one(db)
        .await
        .context("db: find the agent run to end")?
    else {
        return Ok(None);
    };
    let now = Utc::now();
    if !rg_db::ops::agent_run_ops::finish_run(db, run_id, status, Some(reason), now).await? {
        return Ok(None);
    }
    let attempts = rg_db::ops::agent_run_ops::attempts_of_run(db, run_id).await?;
    for attempt in &attempts {
        if attempt.finished_at.is_none() {
            agent_run_attempt::Entity::update_many()
                .col_expr(
                    agent_run_attempt::Column::FinishedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .filter(agent_run_attempt::Column::Id.eq(attempt.id))
                .exec(db)
                .await
                .context("db: finish an agent run attempt")?;
        }
        crate::auth::access_token::revoke_for_agent_run_attempt(db, attempt.id).await?;
        // A pipeline that already finished stays as it finished.
        if let Some(pipeline_id) = attempt.pipeline_id {
            rg_db::ops::pipeline_ops::cancel_pipeline_chain_in_transaction(db, pipeline_id).await?;
        }
    }
    Ok(Some(EndedRun {
        run,
        status,
        reason: reason.to_string(),
    }))
}

/// [`end_run_in`] in a transaction of its own, retried on the contention a
/// concurrent writer causes (an SQLite read upgraded to a write, a deadlock
/// victim) — the same budget every request write has.
async fn end_run(
    pools: &crate::db::Db,
    run_id: i64,
    status: &'static str,
    reason: &str,
) -> Result<Option<EndedRun>> {
    let write = pools.write();
    rg_db::contention::retry_transaction("end an agent run", || async move {
        let txn = write
            .begin()
            .await
            .context("db: begin ending an agent run")?;
        match end_run_in(&txn, run_id, status, reason).await {
            Ok(ended) => {
                txn.commit()
                    .await
                    .context("db: commit the end of an agent run")?;
                Ok(ended)
            }
            Err(error) => {
                if let Err(rollback_error) = txn.rollback().await {
                    tracing::warn!(
                        run_id,
                        error = %format!("{rollback_error:#}"),
                        "ending an agent run failed and could not be rolled back"
                    );
                }
                Err(error)
            }
        }
    })
    .await
}

/// What a run [`end_run_in`] ended does once that has committed: record
/// `agent.run_finished`, and have the bot say `message` on the issue.
pub async fn after_end(db: &impl crate::db::DbPools, ended: EndedRun, message: String) {
    let pools = db.owned();
    let write = pools.write();
    let bot = rg_db::ops::user_ops::find_by_id(pools.read(), ended.run.bot_id)
        .await
        .ok()
        .flatten();
    let issue = match ended.run.issue_id {
        Some(issue_id) => rg_db::ops::issue_ops::find_by_id(pools.read(), issue_id)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    record(
        write,
        None,
        "agent.run_finished",
        ended.run.repository_id,
        serde_json::json!({
            "run_id": ended.run.id,
            "bot": bot.as_ref().map(|bot| bot.username.clone()),
            "issue": issue.as_ref().map(|issue| issue.number),
            "outcome": ended.status,
            "reason": ended.reason,
        }),
    )
    .await;
    let (Some(bot), Some(issue)) = (bot, issue) else {
        return;
    };
    match crate::repo::service::repository_identity(pools.read(), ended.run.repository_id).await {
        Ok((owner, name)) => {
            comment(&pools, &Speaker { bot, owner, name }, &issue, message).await;
        }
        Err(error) => tracing::warn!(
            run_id = ended.run.id,
            error = %format!("{error:#}"),
            "an ended agent run's repository could not be named for the bot's comment"
        ),
    }
}

/// End, in `txn`, the active runs of `previous_assignee` on `issue_id`: the
/// issue was unassigned from it, or assigned to someone else.
pub async fn end_for_unassignment(
    txn: &impl ConnectionTrait,
    issue_id: i64,
    previous_assignee: i64,
) -> Result<Vec<EndedRun>> {
    let mut ended = Vec::new();
    for run in rg_db::ops::agent_run_ops::active_runs_of_issue(txn, issue_id).await? {
        if run.bot_id != previous_assignee {
            continue;
        }
        if let Some(run) = end_run_in(
            txn,
            run.id,
            "cancelled",
            "the issue was unassigned from the bot",
        )
        .await?
        {
            ended.push(run);
        }
    }
    Ok(ended)
}

/// The bot's comment on an issue a run of it was stopped on by unassigning.
pub const UNASSIGNED_MESSAGE: &str =
    "I stopped working on this: the issue is no longer assigned to me.";

/// Why the profile of `bot` no longer allows a run in `repository_id`, if it
/// does not.
fn withdrawn_by_profile(saved: Option<&SavedProfile>, repository_id: i64) -> Option<&'static str> {
    match saved {
        None => Some("the bot's agent profile was deleted"),
        Some(saved) if !saved.profile.enabled => Some("the bot's agent profile was switched off"),
        Some(saved) if !saved.repository_ids.contains(&repository_id) => {
            Some("the bot's agent profile no longer lists this repository")
        }
        Some(_) => None,
    }
}

/// End every active run of `bot` that its profile, as saved now, no longer
/// allows: the profile was deleted or switched off, or its repository was
/// taken off the list. Called after the profile is written; [`recover`] asks
/// the same question, so a run is stopped even if this call never ran.
pub async fn end_runs_withdrawn_by_profile(
    db: &impl crate::db::DbPools,
    bot: &user::Model,
) -> Result<usize> {
    let pools = db.owned();
    let saved = profile::get(pools.read(), bot).await?;
    let mut stopped = 0;
    for run in rg_db::ops::agent_run_ops::active_runs_of_bot(pools.read(), bot.id).await? {
        let Some(reason) = withdrawn_by_profile(saved.as_ref(), run.repository_id) else {
            continue;
        };
        if let Some(ended) = end_run(&pools, run.id, "cancelled", reason).await? {
            after_end(
                &pools,
                ended,
                format!("I stopped working on this: {reason}."),
            )
            .await;
            stopped += 1;
        }
    }
    Ok(stopped)
}

/// What one [`recover`] pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recovered {
    /// Runs whose pipeline was queued by this pass.
    pub queued: usize,
    /// Runs this pass ended: withdrawn by the profile, or past saving.
    pub ended: usize,
}

/// Bring every agent run back to what its row says (card_b391e92474a0).
///
/// - An active run its bot's profile no longer allows ends `cancelled`.
/// - An active `queued` run, older than `min_age`, whose latest attempt no
///   pipeline carries is queued now: its assignment committed, and the request
///   that should have queued it did not get that far (a crash, a dropped
///   connection). A run already `running` that lost its pipeline ends
///   `failed`: restarting an agent halfway is not this pass's call.
///
/// Safe to run beside live requests: queueing is conditional on the attempt
/// naming no pipeline, in the pipeline's own transaction, and ending on the
/// run being active.
pub async fn recover(
    db: &impl crate::db::DbPools,
    launcher: &AgentRunLauncher<'_>,
    min_age: std::time::Duration,
) -> Result<Recovered> {
    let pools = db.owned();
    let mut recovered = Recovered::default();
    let cutoff = Utc::now() - chrono::Duration::from_std(min_age).unwrap_or_default();
    for run in rg_db::ops::agent_run_ops::active_runs(pools.read()).await? {
        match recover_run(&pools, launcher, &run, cutoff).await {
            Ok(RunRecovery::Queued) => recovered.queued += 1,
            Ok(RunRecovery::Ended) => recovered.ended += 1,
            Ok(RunRecovery::Untouched) => {}
            Err(error) => tracing::error!(
                run_id = run.id,
                error = %format!("{error:#}"),
                "an agent run could not be recovered; the next tick tries again"
            ),
        }
    }
    Ok(recovered)
}

enum RunRecovery {
    Queued,
    Ended,
    Untouched,
}

async fn recover_run(
    pools: &crate::db::Db,
    launcher: &AgentRunLauncher<'_>,
    run: &agent_run::Model,
    cutoff: chrono::DateTime<Utc>,
) -> Result<RunRecovery> {
    let read = pools.read();
    let bot = rg_db::ops::user_ops::find_by_id(read, run.bot_id).await?;
    let saved = match &bot {
        Some(bot) => profile::get(read, bot).await?,
        None => None,
    };
    if let Some(reason) = withdrawn_by_profile(saved.as_ref(), run.repository_id) {
        if let Some(ended) = end_run(pools, run.id, "cancelled", reason).await? {
            after_end(
                pools,
                ended,
                format!("I stopped working on this: {reason}."),
            )
            .await;
            return Ok(RunRecovery::Ended);
        }
        return Ok(RunRecovery::Untouched);
    }
    let (Some(bot), Some(saved)) = (bot, saved) else {
        return Ok(RunRecovery::Untouched);
    };
    let Some(attempt) = rg_db::ops::agent_run_ops::attempts_of_run(read, run.id)
        .await?
        .into_iter()
        .last()
    else {
        return Ok(RunRecovery::Untouched);
    };
    if attempt.pipeline_id.is_some() || run.created_at > cutoff {
        return Ok(RunRecovery::Untouched);
    }
    let give_up = |reason: &'static str| async move {
        if let Some(ended) = end_run(pools, run.id, "failed", reason).await? {
            after_end(
                pools,
                ended,
                format!("I could not finish working on this: {reason}."),
            )
            .await;
            return Ok(RunRecovery::Ended);
        }
        Ok::<_, anyhow::Error>(RunRecovery::Untouched)
    };
    if run.status != "queued" {
        return give_up("the run's pipeline is gone").await;
    }
    let (Some(commit_sha), Some(issue_id)) = (run.commit_sha.clone(), run.issue_id) else {
        return give_up("the run's starting commit was not recorded").await;
    };
    let Some(issue) = rg_db::ops::issue_ops::find_by_id(read, issue_id).await? else {
        return give_up("its issue is gone").await;
    };
    let repository = rg_db::ops::repo_ops::find_by_id(read, run.repository_id)
        .await?
        .context("an agent run's repository is gone")?;
    let (owner, name) = crate::repo::service::repository_identity(read, repository.id).await?;
    let job = match runnable_job(launcher, &saved)? {
        Ok(job) => job,
        Err(refusal) => {
            let reason = refusal.message.clone();
            if let Some(ended) = end_run(pools, run.id, "failed", &reason).await? {
                after_end(pools, ended, reason).await;
                return Ok(RunRecovery::Ended);
            }
            return Ok(RunRecovery::Untouched);
        }
    };
    let planned = PlannedRun {
        speaker: Speaker { bot, owner, name },
        repository_id: repository.id,
        actor_id: run.triggered_by,
        job,
        commit_sha,
        default_branch: repository.default_branch.clone(),
        runs_per_day: launcher.limits.runs_per_day(&saved),
        // Already on the row; the queue does not read it.
        task: String::new(),
    };
    tracing::info!(
        run_id = run.id,
        attempt_id = attempt.id,
        "queueing an agent run whose assignment committed but whose pipeline was never created"
    );
    Ok(
        match enqueue(pools, launcher, &planned, issue.number, run.id, attempt.id).await {
            AssignmentOutcome::Started { .. } => RunRecovery::Queued,
            _ => RunRecovery::Ended,
        },
    )
}

// ── Saying it ──────────────────────────────────────────────────────────────

async fn refuse(
    pools: &crate::db::Db,
    speaker: &Speaker,
    issue: &issue::Model,
    actor_id: Option<i64>,
    refusal: &Refusal,
) {
    tracing::info!(
        issue_id = issue.id,
        bot = %speaker.bot.username,
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
            "bot": speaker.bot.username,
            "issue": issue.number,
        }),
    )
    .await;
    comment(pools, speaker, issue, refusal.message.clone()).await;
}

async fn comment(pools: &crate::db::Db, speaker: &Speaker, issue: &issue::Model, body: String) {
    if let Err(error) = crate::issue::service::add_comment(
        pools,
        &speaker.owner,
        &speaker.name,
        issue.number,
        speaker.bot.id,
        body,
    )
    .await
    {
        tracing::warn!(
            issue_id = issue.id,
            bot = %speaker.bot.username,
            error = %format!("{error:#}"),
            "the bot could not say on the issue what became of its agent run"
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

    #[test]
    fn the_instance_ceilings_cap_what_a_profile_asks_for() {
        let saved = SavedProfile {
            profile: rg_db::entities::agent_profile::Model {
                id: 1,
                bot_id: 2,
                enabled: true,
                preset: "custom".into(),
                image: Some("img".into()),
                command: Some("cmd".into()),
                runner_tags: None,
                model_key_encrypted: None,
                timeout_secs: 3600,
                max_runs_per_day: 20,
                max_iterations: 5,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            repository_ids: vec![3],
        };
        let generous = AgentLimits::default();
        assert_eq!(generous.runs_per_day(&saved), 20);
        assert_eq!(generous.timeout_secs(&saved), 3600);
        let tight = AgentLimits {
            max_runs_per_day: 3,
            max_timeout_secs: 600,
        };
        assert_eq!(tight.runs_per_day(&saved), 3);
        assert_eq!(tight.timeout_secs(&saved), 600);
    }

    #[test]
    fn a_profile_withdraws_a_run_it_no_longer_allows() {
        let mut saved = SavedProfile {
            profile: rg_db::entities::agent_profile::Model {
                id: 1,
                bot_id: 2,
                enabled: true,
                preset: "custom".into(),
                image: None,
                command: None,
                runner_tags: None,
                model_key_encrypted: None,
                timeout_secs: 60,
                max_runs_per_day: 1,
                max_iterations: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            repository_ids: vec![3],
        };
        assert_eq!(withdrawn_by_profile(Some(&saved), 3), None);
        assert!(withdrawn_by_profile(Some(&saved), 4).is_some());
        assert!(withdrawn_by_profile(None, 3).is_some());
        saved.profile.enabled = false;
        assert!(withdrawn_by_profile(Some(&saved), 3).is_some());
    }
}
