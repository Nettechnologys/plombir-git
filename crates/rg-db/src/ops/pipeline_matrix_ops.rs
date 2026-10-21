//! The variants of an Actions matrix job, beside each other.
//!
//! Every variant row carries the matrix job it belongs to (`matrix_group`) and
//! that job's `strategy`, so the two rules that span variants are read off the
//! rows themselves: `fail-fast` cancels the unfinished variants when one fails
//! (inside the failing job's own graph transaction, so the stage rolls up with
//! them), and `max-parallel` caps how many are handed to runners at once.
//!
//! A matrix that reads a needed job's outputs is one placeholder row
//! (`deferred_matrix`) until the stage it is in is reached. It is never handed
//! to a runner: the CI engine replaces it with its variants through
//! [`expand_deferred_job`], or fails it through [`fail_deferred_job`] when what
//! the needed job left does not make a matrix.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    PaginatorTrait, QueryFilter, TransactionTrait,
};

use crate::entities::{pipeline, pipeline_job, pipeline_stage};
use crate::ops::pipeline_ops;

/// Mark a job as a variant of the matrix job `group`, with that job's
/// strategy. Written with the graph.
pub async fn set_matrix_membership(
    db: &impl ConnectionTrait,
    job_id: i64,
    group: &str,
    fail_fast: bool,
    max_parallel: Option<u32>,
) -> Result<()> {
    let max_parallel = max_parallel
        .map(i32::try_from)
        .transpose()
        .context("max-parallel does not fit the column")?;
    pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .col_expr(pipeline_job::Column::MatrixGroup, Expr::value(group))
        .col_expr(pipeline_job::Column::MatrixFailFast, Expr::value(fail_fast))
        .col_expr(
            pipeline_job::Column::MatrixMaxParallel,
            Expr::value(max_parallel),
        )
        .exec(db)
        .await
        .context("db: write matrix membership")?;
    Ok(())
}

/// Make a job the placeholder of a matrix decided at run time: `spec` is what
/// its variants will be built from.
pub async fn set_deferred_matrix(db: &impl ConnectionTrait, job_id: i64, spec: &str) -> Result<()> {
    pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .col_expr(pipeline_job::Column::DeferredMatrix, Expr::value(spec))
        .exec(db)
        .await
        .context("db: write deferred matrix")?;
    Ok(())
}

/// The placeholders of one repository whose stage may run now: every stage
/// before theirs succeeded, and their pipeline and stage would let a job of
/// theirs be handed out.
pub async fn ready_deferred_jobs(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Vec<pipeline_job::Model>> {
    let placeholders = pipeline_ops::active_pending_jobs_query(repo_id)
        .filter(pipeline_job::Column::DeferredMatrix.is_not_null())
        .all(db)
        .await
        .context("db: find deferred matrix jobs")?;
    in_reached_stages(db, placeholders).await
}

/// The jobs of `jobs` whose stage was reached
/// ([`pipeline_ops::stage_reached_by`]): every stage before theirs succeeded,
/// or — for a job that runs after a failure — finished.
pub(crate) async fn in_reached_stages(
    db: &DatabaseConnection,
    jobs: Vec<pipeline_job::Model>,
) -> Result<Vec<pipeline_job::Model>> {
    if jobs.is_empty() {
        return Ok(jobs);
    }
    let mut stage_ids = jobs.iter().map(|job| job.stage_id).collect::<Vec<_>>();
    stage_ids.sort_unstable();
    stage_ids.dedup();
    let stages = pipeline_stage::Entity::find()
        .filter(pipeline_stage::Column::Id.is_in(stage_ids))
        .all(db)
        .await
        .context("db: load stages of held jobs")?;
    let mut pipeline_ids = stages
        .iter()
        .map(|stage| stage.pipeline_id)
        .collect::<Vec<_>>();
    pipeline_ids.sort_unstable();
    pipeline_ids.dedup();
    let siblings = pipeline_stage::Entity::find()
        .filter(pipeline_stage::Column::PipelineId.is_in(pipeline_ids))
        .all(db)
        .await
        .context("db: load stages of held jobs' pipelines")?;
    Ok(jobs
        .into_iter()
        .filter(|job| {
            stages
                .iter()
                .find(|stage| stage.id == job.stage_id)
                .is_some_and(|stage| pipeline_ops::stage_reached_by(job, stage, &siblings))
        })
        .collect())
}

/// The placeholders of one stage, for a runner that walks the stage itself.
pub async fn deferred_jobs_in_stage(
    db: &impl ConnectionTrait,
    stage_id: i64,
) -> Result<Vec<pipeline_job::Model>> {
    pipeline_job::Entity::find()
        .filter(pipeline_job::Column::StageId.eq(stage_id))
        .filter(pipeline_job::Column::Status.eq("pending"))
        .filter(pipeline_job::Column::DeferredMatrix.is_not_null())
        .all(db)
        .await
        .context("db: find the stage's deferred matrix jobs")
}

/// Replace a placeholder with its variants, in one graph transaction.
///
/// The placeholder is taken first — deleted only while it is still a
/// `pending` placeholder — so of two pollers that reach it together exactly
/// one writes the variants; `Ok(false)` tells the other. `write_variants`
/// takes the transaction by value and hands it back with its verdict, the
/// shape `run_embedded_graph_transaction` explains; an `Err` from it rolls the
/// placeholder back.
pub async fn expand_deferred_job<F, Fut>(
    db: &DatabaseConnection,
    placeholder: &pipeline_job::Model,
    write_variants: F,
) -> Result<bool>
where
    F: Fn(DatabaseTransaction) -> Fut,
    Fut: std::future::Future<Output = (DatabaseTransaction, Result<()>)>,
{
    let stage = pipeline_ops::get_stage_by_id(db, placeholder.stage_id)
        .await?
        .with_context(|| {
            format!(
                "stage {} of a deferred matrix job is gone",
                placeholder.stage_id
            )
        })?;
    crate::contention::retry_transaction("expand a deferred matrix job", || async {
        let tx = db
            .begin()
            .await
            .context("db: begin deferred matrix expansion")?;
        let taken = async {
            pipeline_ops::lock_graph_for_stage_transition(
                &tx,
                stage.id,
                stage.pipeline_id,
                "deferred matrix expansion",
            )
            .await?;
            Ok::<_, anyhow::Error>(
                pipeline_job::Entity::delete_many()
                    .filter(pipeline_job::Column::Id.eq(placeholder.id))
                    .filter(pipeline_job::Column::Status.eq("pending"))
                    .filter(pipeline_job::Column::DeferredMatrix.is_not_null())
                    .exec(&tx)
                    .await
                    .context("db: take the deferred matrix placeholder")?
                    .rows_affected
                    == 1,
            )
        }
        .await;
        let (tx, outcome) = match taken {
            Ok(true) => {
                let (tx, written) = write_variants(tx).await;
                (tx, written.map(|()| true))
            }
            other => (tx, other),
        };
        match outcome {
            Ok(expanded) => {
                tx.commit()
                    .await
                    .context("db: commit deferred matrix expansion")?;
                Ok(expanded)
            }
            Err(error) => {
                if let Err(rollback) = tx.rollback().await {
                    tracing::error!(
                        job_id = placeholder.id,
                        error = %format!("{rollback:#}"),
                        "deferred matrix expansion failed and could not be rolled back"
                    );
                }
                Err(error)
            }
        }
    })
    .await
}

/// Fail a placeholder whose matrix has no expansion — what the needed job
/// left is not a matrix, or one past the cap — with `log` saying why, and roll
/// its stage and pipeline up. `Some(pipeline)` when that finished the
/// pipeline, for the caller's completion hooks.
pub async fn fail_deferred_job(
    db: &DatabaseConnection,
    placeholder: &pipeline_job::Model,
    log: &str,
) -> Result<Option<pipeline::Model>> {
    let stage = pipeline_ops::get_stage_by_id(db, placeholder.stage_id)
        .await?
        .with_context(|| {
            format!(
                "stage {} of a deferred matrix job is gone",
                placeholder.stage_id
            )
        })?;
    crate::contention::retry_transaction("fail a deferred matrix job", || async {
        let tx = db
            .begin()
            .await
            .context("db: begin deferred matrix failure")?;
        let step = async {
            pipeline_ops::lock_graph_for_stage_transition(
                &tx,
                stage.id,
                stage.pipeline_id,
                "deferred matrix failure",
            )
            .await?;
            let now = chrono::Utc::now().naive_utc();
            if !pipeline_ops::settle_job_if_active(
                &tx,
                placeholder.id,
                "failed",
                None,
                Some(log),
                Some(now),
            )
            .await?
            {
                return Ok(None);
            }
            pipeline_job::Entity::update_many()
                .filter(pipeline_job::Column::Id.eq(placeholder.id))
                .col_expr(
                    pipeline_job::Column::DeferredMatrix,
                    Expr::value(Option::<String>::None),
                )
                .exec(&tx)
                .await
                .context("db: clear the failed deferred matrix")?;
            let finished = match pipeline_ops::try_update_stage(&tx, stage.id).await? {
                Some(_) => pipeline_ops::try_update_pipeline(&tx, stage.pipeline_id).await?,
                None => None,
            };
            Ok::<_, anyhow::Error>(match finished {
                Some(_) => pipeline_ops::get_pipeline(&tx, stage.pipeline_id).await?,
                None => None,
            })
        }
        .await;
        match step {
            Ok(status) => {
                tx.commit()
                    .await
                    .context("db: commit deferred matrix failure")?;
                Ok(status)
            }
            Err(error) => {
                if let Err(rollback) = tx.rollback().await {
                    tracing::error!(error = %format!("{rollback:#}"), "deferred matrix failure could not be rolled back");
                }
                Err(error)
            }
        }
    })
    .await
}

/// `strategy.fail-fast`: once a variant of a matrix job has failed, cancel
/// every variant of the same job that has not finished. Called in the failing
/// job's own transaction, after its result is written and before its stage
/// rolls up, so the roll-up sees the cancellations.
///
/// A variant that failed under `continue-on-error` does not count, as on
/// GitHub. Returns how many variants were canceled.
pub(crate) async fn cancel_fail_fast_siblings(
    db: &impl ConnectionTrait,
    failed: &pipeline_job::Model,
    status: &str,
) -> Result<u64> {
    if !matches!(status, "failed" | "failure" | "error") || failed.allow_failure {
        return Ok(0);
    }
    let (Some(group), Some(true)) = (failed.matrix_group.as_deref(), failed.matrix_fail_fast)
    else {
        return Ok(0);
    };
    let siblings = pipeline_job::Entity::find()
        .filter(pipeline_job::Column::StageId.eq(failed.stage_id))
        .filter(pipeline_job::Column::MatrixGroup.eq(group))
        .filter(pipeline_job::Column::Id.ne(failed.id))
        .filter(pipeline_job::Column::Status.is_in(pipeline_ops::ACTIVE_WORK_STATUSES))
        .all(db)
        .await
        .context("db: find the unfinished variants of a failed matrix job")?;
    let now = chrono::Utc::now().naive_utc();
    let reason = format!(
        "Canceled: `{}` failed, and the matrix job's strategy.fail-fast is on.\n",
        failed.name
    );
    for sibling in &siblings {
        pipeline_ops::update_job_result(
            db,
            sibling.id,
            "canceled",
            None,
            sibling.log.is_none().then_some(reason.as_str()),
            None,
            Some(now),
        )
        .await?;
    }
    Ok(siblings.len() as u64)
}

/// The variants of `job`'s matrix that hold a runner right now.
async fn variants_in_flight(db: &impl ConnectionTrait, job: &pipeline_job::Model) -> Result<u64> {
    let Some(group) = job.matrix_group.as_deref() else {
        return Ok(0);
    };
    pipeline_job::Entity::find()
        .filter(pipeline_job::Column::StageId.eq(job.stage_id))
        .filter(pipeline_job::Column::MatrixGroup.eq(group))
        .filter(pipeline_job::Column::Status.is_in(["assigned", "running"]))
        .count(db)
        .await
        .context("db: count the matrix variants in flight")
}

/// `strategy.max-parallel`: whether one more variant of `job`'s matrix may be
/// handed to a runner now.
pub(crate) async fn below_max_parallel(
    db: &impl ConnectionTrait,
    job: &pipeline_job::Model,
) -> Result<bool> {
    let Some(limit) = job.matrix_max_parallel else {
        return Ok(true);
    };
    Ok(variants_in_flight(db, job).await? < u64::try_from(limit.max(1)).unwrap_or(1))
}
