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
