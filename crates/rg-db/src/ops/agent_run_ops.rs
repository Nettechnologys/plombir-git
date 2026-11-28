//! Database operations for agent runs (ADR-0001 D4, D12).
//!
//! The run row is the durable record that a pull request was written by an
//! agent. The CI gate asks it for every head of every pull request, long after
//! the run's token is gone, so the questions here read the run and never the
//! token.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::agent_run::{self, Entity as RunEntity, Model as AgentRun};
use crate::entities::agent_run_attempt::Entity as AttemptEntity;

/// The run attempt `attempt_id` belongs to, if the attempt still exists.
pub async fn run_of_attempt(db: &DatabaseConnection, attempt_id: i64) -> Result<Option<AgentRun>> {
    let Some(attempt) = AttemptEntity::find_by_id(attempt_id)
        .one(db)
        .await
        .context("db: find agent run attempt")?
    else {
        return Ok(None);
    };
    RunEntity::find_by_id(attempt.run_id)
        .one(db)
        .await
        .context("db: find agent run of attempt")
}

/// Whether an agent run opened pull request `pr_id`, or iterates on it.
pub async fn is_agent_pull_request(db: &DatabaseConnection, pr_id: i64) -> Result<bool> {
    let runs = RunEntity::find()
        .filter(
            Condition::any()
                .add(agent_run::Column::ResultPullRequestId.eq(pr_id))
                .add(agent_run::Column::PullRequestId.eq(pr_id)),
        )
        .count(db)
        .await
        .context("db: count agent runs of pull request")?;
    Ok(runs > 0)
}

/// Every ref prefix a run in `repository_id` was given to write under, once
/// each.
///
/// The CI gate asks it for a pull request that belongs to no run: one a person
/// opened from a branch the agent pushed to. A handful of rows — one per bot
/// for first runs, one per iterated pull request — however many runs there
/// were.
pub async fn write_ref_prefixes(
    db: &DatabaseConnection,
    repository_id: i64,
) -> Result<Vec<String>> {
    let prefixes: Vec<Option<String>> = RunEntity::find()
        .select_only()
        .column(agent_run::Column::WriteRefPrefix)
        .distinct()
        .filter(agent_run::Column::RepositoryId.eq(repository_id))
        .filter(agent_run::Column::WriteRefPrefix.is_not_null())
        .into_tuple()
        .all(db)
        .await
        .context("db: list the ref prefixes agent runs wrote under")?;
    Ok(prefixes.into_iter().flatten().collect())
}

/// Record that run `run_id` opened pull request `pr_id`.
///
/// Conditional on the run having opened none yet, so two pull requests racing
/// out of one run cannot both claim it; `false` means another one already did.
pub async fn record_opened_pull_request(
    db: &DatabaseConnection,
    run_id: i64,
    pr_id: i64,
) -> Result<bool> {
    let updated = RunEntity::update_many()
        .col_expr(
            agent_run::Column::ResultPullRequestId,
            sea_query::Expr::value(pr_id),
        )
        .filter(agent_run::Column::Id.eq(run_id))
        .filter(agent_run::Column::ResultPullRequestId.is_null())
        .exec(db)
        .await
        .context("db: record the pull request an agent run opened")?;
    Ok(updated.rows_affected == 1)
}

/// Take one run of `bot_id`'s daily allowance for `day` (`YYYY-MM-DD`, UTC),
/// if fewer than `limit` are taken; `false` when the day is spent.
///
/// Run inside the transaction that records the run (card_b391e92474a0). The
/// decision is the conditional `UPDATE ... WHERE used < limit`, which every
/// backend evaluates against the row as the last committed writer left it,
/// so two assignments racing for the last run of the day take it once: the
/// loser waits on the winner's row lock and then finds `used = limit`. A
/// count of today's runs read first could be passed by both.
pub async fn reserve_daily_run(
    db: &impl ConnectionTrait,
    bot_id: i64,
    day: &str,
    limit: i64,
) -> Result<bool> {
    use crate::entities::agent_run_quota::{ActiveModel, Column, Entity};
    if limit <= 0 {
        return Ok(false);
    }
    let inserted = Entity::insert(ActiveModel {
        bot_id: Set(bot_id),
        day: Set(day.to_string()),
        used: Set(1),
        ..Default::default()
    })
    .on_conflict(
        sea_query::OnConflict::columns([Column::BotId, Column::Day])
            // MySQL has no `DO NOTHING`; a no-op update of the key is its
            // spelling, and it reports the row as not inserted all the same.
            .do_nothing_on([Column::Id])
            .to_owned(),
    )
    .do_nothing()
    .exec(db)
    .await
    .context("db: open a bot's daily agent run allowance")?;
    match inserted {
        TryInsertResult::Inserted(_) => return Ok(true),
        TryInsertResult::Conflicted => {}
        TryInsertResult::Empty => {
            anyhow::bail!("db: a daily agent run allowance insert unexpectedly carried no values")
        }
    }
    let taken = Entity::update_many()
        .col_expr(Column::Used, sea_query::Expr::col(Column::Used).add(1_i64))
        .filter(Column::BotId.eq(bot_id))
        .filter(Column::Day.eq(day))
        .filter(Column::Used.lt(limit))
        .exec(db)
        .await
        .context("db: take a run of a bot's daily agent run allowance")?;
    Ok(taken.rows_affected == 1)
}

/// Point attempt `attempt_id` at the pipeline that carries it, if it points at
/// none yet; `false` when another pipeline already does.
///
/// Called in the transaction that writes the pipeline, so an attempt that has
/// a pipeline always says so, and a second enqueue of the same attempt (the
/// startup catch-up racing the request that started it) rolls its own
/// pipeline back instead of running the agent twice.
pub async fn link_attempt_pipeline(
    db: &impl ConnectionTrait,
    attempt_id: i64,
    pipeline_id: i64,
) -> Result<bool> {
    use crate::entities::agent_run_attempt::Column;
    let linked = AttemptEntity::update_many()
        .col_expr(Column::PipelineId, sea_query::Expr::value(pipeline_id))
        .filter(Column::Id.eq(attempt_id))
        .filter(Column::PipelineId.is_null())
        .exec(db)
        .await
        .context("db: point an agent run attempt at its pipeline")?;
    Ok(linked.rows_affected == 1)
}

/// End run `run_id` with `status`, if it is still active; `false` when it had
/// already ended.
///
/// The one transition into a terminal status: conditional on `active`, so of
/// two enders racing (a cancel and the job's own finish) exactly one sees
/// `true` and does what an ending does — and the other does nothing.
pub async fn finish_run(
    db: &impl ConnectionTrait,
    run_id: i64,
    status: &str,
    failure_reason: Option<&str>,
    finished_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let finished = RunEntity::update_many()
        .col_expr(agent_run::Column::Status, sea_query::Expr::value(status))
        .col_expr(
            agent_run::Column::Active,
            sea_query::Expr::value(Option::<bool>::None),
        )
        .col_expr(
            agent_run::Column::FailureReason,
            sea_query::Expr::value(failure_reason.map(str::to_string)),
        )
        .col_expr(
            agent_run::Column::FinishedAt,
            sea_query::Expr::value(finished_at),
        )
        .filter(agent_run::Column::Id.eq(run_id))
        .filter(agent_run::Column::Active.eq(true))
        .exec(db)
        .await
        .context("db: end an agent run")?;
    Ok(finished.rows_affected == 1)
}

/// The attempts of run `run_id`, oldest first.
pub async fn attempts_of_run(
    db: &impl ConnectionTrait,
    run_id: i64,
) -> Result<Vec<crate::entities::agent_run_attempt::Model>> {
    use crate::entities::agent_run_attempt::Column;
    AttemptEntity::find()
        .filter(Column::RunId.eq(run_id))
        .order_by_asc(Column::Attempt)
        .all(db)
        .await
        .context("db: list the attempts of an agent run")
}

/// The active runs of `bot_id`, in any repository.
pub async fn active_runs_of_bot(db: &impl ConnectionTrait, bot_id: i64) -> Result<Vec<AgentRun>> {
    RunEntity::find()
        .filter(agent_run::Column::BotId.eq(bot_id))
        .filter(agent_run::Column::Active.eq(true))
        .all(db)
        .await
        .context("db: list the active agent runs of a bot")
}

/// The active runs working on issue `issue_id`.
pub async fn active_runs_of_issue(
    db: &impl ConnectionTrait,
    issue_id: i64,
) -> Result<Vec<AgentRun>> {
    RunEntity::find()
        .filter(agent_run::Column::IssueId.eq(issue_id))
        .filter(agent_run::Column::Active.eq(true))
        .all(db)
        .await
        .context("db: list the active agent runs of an issue")
}

/// Every active run, for the startup catch-up.
pub async fn active_runs(db: &impl ConnectionTrait) -> Result<Vec<AgentRun>> {
    RunEntity::find()
        .filter(agent_run::Column::Active.eq(true))
        .order_by_asc(agent_run::Column::Id)
        .all(db)
        .await
        .context("db: list the active agent runs")
}

/// The run whose attempt pipeline `pipeline_id` carries, with that attempt.
pub async fn attempt_of_pipeline(
    db: &impl ConnectionTrait,
    pipeline_id: i64,
) -> Result<Option<(AgentRun, crate::entities::agent_run_attempt::Model)>> {
    use crate::entities::agent_run_attempt::Column;
    let Some(attempt) = AttemptEntity::find()
        .filter(Column::PipelineId.eq(pipeline_id))
        .one(db)
        .await
        .context("db: find the agent run attempt of a pipeline")?
    else {
        return Ok(None);
    };
    let run = RunEntity::find_by_id(attempt.run_id)
        .one(db)
        .await
        .context("db: find the agent run of an attempt")?;
    Ok(run.map(|run| (run, attempt)))
}
