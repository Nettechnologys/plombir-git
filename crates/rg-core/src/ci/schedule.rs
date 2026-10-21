//! `on: schedule`: the producer of the `schedule` event.
//!
//! Once a minute the pass reads every live repository's default branch, asks
//! the CI engine which `- cron:` entries its workflows declare there, and runs
//! the workflow for every entry whose latest tick nobody has claimed yet.
//!
//! **Where a schedule comes from.** As on GitHub, the default branch's tip — a
//! cron entry on a feature branch schedules nothing. The entries are re-read
//! whenever that tip moves, so a push to the default branch that adds, changes
//! or removes a schedule takes effect at the next minute; between pushes the
//! parsed set is kept in memory by the commit it was read from.
//!
//! **Exactly one run per tick.** A tick is run by whoever moves the entry's row
//! in `ci_schedule_ticks` past it, in one conditional `UPDATE`
//! ([`rg_db::ops::ci_schedule_ops::claim_tick`]). Every replica runs the same
//! pass at the same minute and a restart replays the minute it stopped in; the
//! row is what makes both of those a no-op. The claim comes before the
//! trigger, so a process that dies between the two loses that tick rather than
//! running it twice — the half of the trade a schedule can live with.
//!
//! **No catching up.** Only the latest tick at or before now is ever claimed.
//! A server that was down through six ticks of `*/10 * * * *` runs one when it
//! comes back, not six; GitHub does the same.
//!
//! **A first sight runs nothing.** The first pass to see an entry records its
//! latest tick without running it, so a `0 3 * * *` pushed at 15:00 waits for
//! the next 03:00 instead of running at once for one that passed before the
//! file existed.
//!
//! **A floor on frequency.** No entry runs more often than every
//! `[ci] schedule_min_interval_minutes` (five by default, GitHub's own floor);
//! `* * * * *` on a default instance runs every fifth minute.
//!
//! **Not every repository.** Soft-deleted ones are not scheduled, and neither
//! are pull mirrors: their workflows are their upstream's, and a mirror pass
//! runs no `push` pipeline for the same reason. A repository with no workflow
//! declaring `schedule` costs the pass one ref read per minute.

use anyhow::{Context, Result};
use chrono::{DateTime, Timelike, Utc};
use sea_orm::DatabaseConnection;
use std::collections::HashMap;
use std::path::PathBuf;

use super::cron::CronSchedule;
use super::{ScheduledRun, TriggerPipelineParams, SCHEDULE_EVENT};
use crate::push_hooks::PostPushContext;
use rg_db::ops::ci_schedule_ops::{self, TickClaim};

/// GitHub's own floor: no scheduled workflow runs more often than every five
/// minutes.
pub const DEFAULT_MIN_INTERVAL_MINUTES: u64 = 5;

/// How many repositories one query of the pass reads.
const REPOSITORY_PAGE: u64 = 200;

/// The longest workflow file name the tick ledger keys on (`VARCHAR(255)`).
const MAX_WORKFLOW_NAME_BYTES: usize = 255;

/// The operator's knobs for the pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleConfig {
    /// `[ci] schedule_min_interval_minutes`.
    pub min_interval_minutes: u64,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            min_interval_minutes: DEFAULT_MIN_INTERVAL_MINUTES,
        }
    }
}

impl ScheduleConfig {
    /// A zero floor would let `* * * * *` run every minute *and* make the
    /// throttle meaningless; one minute is the finest a cron can say anyway.
    pub fn validated(self) -> Result<Self> {
        anyhow::ensure!(
            self.min_interval_minutes >= 1,
            "`[ci].schedule_min_interval_minutes` must be at least 1 (a schedule cannot fire \
             more than once a minute)"
        );
        Ok(self)
    }
}

/// One `- cron:` entry of one workflow file, parsed.
#[derive(Debug, Clone)]
struct Entry {
    workflow: String,
    cron: String,
    schedule: CronSchedule,
}

/// The entries of one repository's default branch, by the commit they were
/// read from.
#[derive(Debug, Clone)]
struct Declared {
    head: String,
    entries: Vec<Entry>,
}

/// What the pass remembers between minutes: the parsed schedules of every
/// repository, so a branch that did not move is not read and parsed again.
#[derive(Debug, Default)]
pub struct ScheduleCache {
    repositories: HashMap<i64, Declared>,
}

/// One pipeline a pass started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledPipeline {
    pub repo_id: i64,
    pub workflow: String,
    pub cron: String,
    /// The minute of the tick that was run.
    pub tick: DateTime<Utc>,
    /// The pipeline the tick produced, or the failed one that records why its
    /// configuration was refused. `None` when the trigger failed in a way that
    /// is not the repository's to fix (it is logged).
    pub pipeline_id: Option<i64>,
}

/// Run one pass at `now`. Returns the ticks it claimed.
///
/// `now` is a parameter so a test can hold the clock; the background loop
/// passes the wall clock.
pub async fn run_pass(
    db: &DatabaseConnection,
    context: &PostPushContext,
    config: ScheduleConfig,
    cache: &mut ScheduleCache,
    now: DateTime<Utc>,
) -> Result<Vec<ScheduledPipeline>> {
    let mut started = Vec::new();
    let mut after = 0;
    let mut live = std::collections::HashSet::new();
    loop {
        let page = rg_db::ops::repo_ops::list_schedulable_after(db, after, REPOSITORY_PAGE)
            .await
            .context("list repositories for the schedule pass")?;
        let Some(last) = page.last() else { break };
        after = last.id;
        for repository in page {
            live.insert(repository.id);
            match schedule_repository(db, context, config, cache, now, &repository).await {
                Ok(mut runs) => started.append(&mut runs),
                // One repository that cannot be read must not take the
                // schedules of every other one down with it.
                Err(error) => tracing::warn!(
                    repo_id = repository.id,
                    error = %format!("{error:#}"),
                    "schedule pass skipped a repository"
                ),
            }
        }
    }
    cache
        .repositories
        .retain(|repo_id, _| live.contains(repo_id));
    Ok(started)
}

async fn schedule_repository(
    db: &DatabaseConnection,
    context: &PostPushContext,
    config: ScheduleConfig,
    cache: &mut ScheduleCache,
    now: DateTime<Utc>,
    repository: &rg_db::entities::repository::Model,
) -> Result<Vec<ScheduledPipeline>> {
    let owner =
        crate::repo::service::repository_namespace_name(db, repository.owner_id, repository.org_id)
            .await?;
    let repo_path = context
        .repo_root
        .join(format!("{owner}/{}.git", repository.name));
    let branch = repository.default_branch.clone();
    let head = {
        let repo_path = repo_path.clone();
        tokio::task::spawn_blocking(move || {
            crate::repo::service::try_get_branch_sha(&repo_path, &branch)
        })
        .await
        .context("default branch lookup did not complete")??
    };
    let writes = context.write_pool.as_ref().unwrap_or(db);
    let Some(head) = head else {
        // No default branch, so no schedule: an emptied repository's old
        // entries must not run again the day the branch reappears.
        if cache.repositories.remove(&repository.id).is_some() {
            ci_schedule_ops::forget_undeclared(writes, repository.id, &[]).await?;
        }
        return Ok(Vec::new());
    };

    if cache
        .repositories
        .get(&repository.id)
        .is_none_or(|declared| declared.head != head)
    {
        let declared = read_declared(context, repo_path.clone(), head.clone()).await?;
        let keys = declared
            .entries
            .iter()
            .map(|entry| (entry.workflow.clone(), entry.cron.clone()))
            .collect::<Vec<_>>();
        ci_schedule_ops::forget_undeclared(writes, repository.id, &keys).await?;
        cache.repositories.insert(repository.id, declared);
    }
    let Some(declared) = cache.repositories.get(&repository.id) else {
        return Ok(Vec::new());
    };

    let min_interval = i64::try_from(config.min_interval_minutes).unwrap_or(i64::MAX);
    let mut started = Vec::new();
    for entry in &declared.entries {
        let Some(tick) = entry.schedule.latest_tick_at_or_before(now) else {
            continue;
        };
        let claim = ci_schedule_ops::claim_tick(
            writes,
            repository.id,
            &entry.workflow,
            &entry.cron,
            tick.timestamp().div_euclid(60),
            min_interval,
        )
        .await?;
        if claim != TickClaim::Claimed {
            continue;
        }
        let pipeline_id = trigger(
            db,
            context,
            repository.id,
            &repo_path,
            &declared.head,
            &format!("refs/heads/{}", repository.default_branch),
            ScheduledRun {
                workflow: &entry.workflow,
                cron: &entry.cron,
            },
        )
        .await;
        started.push(ScheduledPipeline {
            repo_id: repository.id,
            workflow: entry.workflow.clone(),
            cron: entry.cron.clone(),
            tick,
            pipeline_id,
        });
    }
    Ok(started)
}

/// Ask the engine for the entries at `head`, on a blocking thread: it reads and
/// parses every workflow blob at that commit.
async fn read_declared(
    context: &PostPushContext,
    repo_path: PathBuf,
    head: String,
) -> Result<Declared> {
    let engine = context.ci_engine.clone();
    let commit = head.clone();
    let schedules =
        tokio::task::spawn_blocking(move || engine.scheduled_workflows(&repo_path, &commit))
            .await
            .context("reading the schedules did not complete")??;
    let mut entries = Vec::new();
    for schedule in schedules {
        if schedule.workflow.len() > MAX_WORKFLOW_NAME_BYTES {
            tracing::warn!(
                workflow = %schedule.workflow,
                "a workflow file name longer than {MAX_WORKFLOW_NAME_BYTES} bytes cannot be \
                 scheduled"
            );
            continue;
        }
        for cron in schedule.crons {
            // The engine hands over entries it already accepted; a refusal here
            // means the two readers of the dialect disagree, and the entry is
            // not run rather than run on a guess.
            match CronSchedule::parse(&cron) {
                Ok(parsed) => entries.push(Entry {
                    workflow: schedule.workflow.clone(),
                    cron,
                    schedule: parsed,
                }),
                Err(reason) => tracing::warn!(
                    workflow = %schedule.workflow,
                    cron = %cron,
                    "a schedule the engine accepted does not parse: {reason}"
                ),
            }
        }
    }
    Ok(Declared { head, entries })
}

/// Start the run one claimed tick owes, or record why it cannot start.
async fn trigger(
    db: &DatabaseConnection,
    context: &PostPushContext,
    repo_id: i64,
    repo_path: &std::path::Path,
    commit_sha: &str,
    ref_name: &str,
    run: ScheduledRun<'_>,
) -> Option<i64> {
    let triggered = context
        .ci_engine
        .trigger_pipeline(TriggerPipelineParams {
            db,
            repo_root: &context.repo_root,
            repo_path,
            repo_id,
            commit_sha,
            ref_name,
            trigger_type: SCHEDULE_EVENT,
            // A scheduled run targets the branch it runs on, and has no
            // previous revision: `paths:` does not apply to it.
            base_branch: None,
            head_branch: None,
            previous_sha: None,
            inputs: None,
            schedule: Some(run),
            triggered_by: None,
            docker_enabled: context.docker_enabled,
            external_runners: context.external_runners,
            allow_host_runner: context.allow_host_runner,
            jwt_secret: context.jwt_secret.as_deref(),
            encryption_key: context.encryption_key.as_deref(),
            external_url: context.external_url.as_deref(),
        })
        .await;
    match triggered {
        Ok(pipeline_id) => {
            tracing::info!(
                repo_id,
                pipeline_id,
                workflow = run.workflow,
                cron = run.cron,
                "scheduled pipeline triggered"
            );
            Some(pipeline_id)
        }
        Err(error) if error.downcast_ref::<super::NoMatchingCiJobs>().is_some() => {
            tracing::info!(
                repo_id,
                workflow = run.workflow,
                "the scheduled workflow selected no jobs for the default branch"
            );
            None
        }
        Err(error) => {
            // The run cannot be shown to whoever reads the log, so the reason
            // is written where the repository's CI is read: a failed pipeline
            // under the `schedule` event, retryable once the file is fixed.
            match super::publish_configuration_failure(
                super::ConfigurationFailureParams {
                    db,
                    write_pool: context.write_pool.as_ref(),
                    notifier: context.notifier.as_ref(),
                    repo_id,
                    commit_sha,
                    ref_name,
                    trigger_type: SCHEDULE_EVENT,
                    triggered_by: None,
                    base_branch: None,
                    head_branch: None,
                    previous_sha: None,
                    schedule: Some(run),
                },
                &error,
            )
            .await
            {
                Ok(Some(pipeline_id)) => {
                    tracing::warn!(
                        repo_id,
                        pipeline_id,
                        error = %format!("{error:#}"),
                        "scheduled run was refused; recorded a failed pipeline"
                    );
                    Some(pipeline_id)
                }
                Ok(None) => {
                    tracing::warn!(
                        repo_id,
                        workflow = run.workflow,
                        error = %format!("{error:#}"),
                        "failed to trigger a scheduled pipeline"
                    );
                    None
                }
                Err(record_error) => {
                    tracing::warn!(
                        repo_id,
                        trigger_error = %format!("{error:#}"),
                        error = %format!("{record_error:#}"),
                        "scheduled run was refused, and its failed pipeline could not be recorded"
                    );
                    None
                }
            }
        }
    }
}

/// How long to sleep from `now` until a pass at the start of the next minute.
///
/// A second past the boundary, so a clock read a hair early still lands in the
/// minute the tick belongs to.
fn until_next_minute(now: DateTime<Utc>) -> std::time::Duration {
    let into_minute = u64::from(now.second()) * 1_000 + u64::from(now.nanosecond() / 1_000_000);
    std::time::Duration::from_millis(60_000 - into_minute.min(59_999) + 1_000)
}

/// Start the once-a-minute pass. It stops at the process's shutdown signal.
pub fn spawn_scheduler(
    db: DatabaseConnection,
    context: PostPushContext,
    config: ScheduleConfig,
    mut shutdown: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<tokio::task::JoinHandle<()>> {
    let config = config.validated()?;
    Ok(tokio::spawn(async move {
        let mut cache = ScheduleCache::default();
        loop {
            tokio::select! {
                () = tokio::time::sleep(until_next_minute(Utc::now())) => {}
                () = crate::task_tracker::wait_optional_shutdown(&mut shutdown) => break,
            }
            match run_pass(&db, &context, config, &mut cache, Utc::now()).await {
                Ok(started) if !started.is_empty() => {
                    tracing::debug!(count = started.len(), "schedule pass ran ticks")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    error = %format!("{error:#}"),
                    "schedule pass failed; the next minute tries again"
                ),
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pass_sleeps_to_just_past_the_next_minute() {
        let at = |text: &str| {
            DateTime::parse_from_rfc3339(text)
                .unwrap()
                .with_timezone(&Utc)
        };
        assert_eq!(
            until_next_minute(at("2026-10-20T10:00:00Z")),
            std::time::Duration::from_secs(61)
        );
        assert_eq!(
            until_next_minute(at("2026-10-20T10:00:59.500Z")),
            std::time::Duration::from_millis(1_500)
        );
    }

    #[test]
    fn a_zero_floor_is_refused_by_name() {
        let error = ScheduleConfig {
            min_interval_minutes: 0,
        }
        .validated()
        .expect_err("zero is refused");
        assert!(error.to_string().contains("schedule_min_interval_minutes"));
    }
}
