//! Actions jobs held until the jobs they need have finished.
//!
//! A job that lists `needs:` cannot be decided when its pipeline is written:
//! its `if:` may read `needs.<id>.outputs.*` / `needs.<id>.result`, and a job
//! whose needed job was skipped is skipped too, as on GitHub. Such a job is
//! written `pending` with `deferred_condition` set, which keeps it from every
//! runner (see `pipeline_ops::pending_jobs_query` and the claim itself). Once
//! its stage is reached, the CI engine either releases it through
//! [`release_deferred_job`] — it becomes an ordinary pending job — or skips it
//! through [`settle_held_job`].

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    QueryFilter, TransactionTrait,
};

use crate::entities::{pipeline, pipeline_job};
use crate::ops::pipeline_ops;

/// Hold a job until its stage is reached: `decision` is what decides then
/// whether it runs. Written with the graph.
pub async fn set_deferred_condition(
    db: &impl ConnectionTrait,
    job_id: i64,
    decision: &str,
) -> Result<()> {
    pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .col_expr(
            pipeline_job::Column::DeferredCondition,
            Expr::value(decision),
        )
        .exec(db)
        .await
        .context("db: write deferred condition")?;
    Ok(())
}

/// The held jobs of one repository whose stage may run now: every stage before
/// theirs succeeded (or finished, for a job that runs after a failure), and
/// their pipeline and stage would let a job of theirs be handed out.
pub async fn ready_deferred_conditions(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Vec<pipeline_job::Model>> {
    let held = pipeline_ops::active_pending_jobs_query(repo_id)
        .filter(pipeline_job::Column::DeferredCondition.is_not_null())
        .all(db)
        .await
        .context("db: find held jobs")?;
    crate::ops::pipeline_matrix_ops::in_reached_stages(db, held).await
}

/// The held jobs of one stage, for a runner that walks the stage itself.
pub async fn deferred_conditions_in_stage(
    db: &impl ConnectionTrait,
    stage_id: i64,
) -> Result<Vec<pipeline_job::Model>> {
    pipeline_job::Entity::find()
        .filter(pipeline_job::Column::StageId.eq(stage_id))
        .filter(pipeline_job::Column::Status.eq("pending"))
        .filter(pipeline_job::Column::DeferredCondition.is_not_null())
        .all(db)
        .await
        .context("db: find the stage's held jobs")
}

/// Let a held job run: clear its hold, and in the same graph transaction let
/// `on_release` write what only a job that runs gets — its environment, which
/// may put it behind an approval.
///
/// The hold is taken first — cleared only while the job is still a `pending`
/// held job — so of two pollers that reach it together exactly one releases
/// it; `Ok(false)` tells the other. `on_release` takes the transaction by value
/// and hands it back with its verdict; an `Err` from it keeps the hold.
pub async fn release_deferred_job<F, Fut>(
    db: &DatabaseConnection,
    job: &pipeline_job::Model,
    on_release: F,
) -> Result<bool>
where
    F: Fn(DatabaseTransaction) -> Fut,
    Fut: std::future::Future<Output = (DatabaseTransaction, Result<()>)>,
{
    let stage = pipeline_ops::get_stage_by_id(db, job.stage_id)
        .await?
        .with_context(|| format!("stage {} of a held job is gone", job.stage_id))?;
    crate::contention::retry_transaction("release a held job", || async {
        let tx = db.begin().await.context("db: begin held job release")?;
        let taken = async {
            pipeline_ops::lock_graph_for_stage_transition(
                &tx,
                stage.id,
                stage.pipeline_id,
                "held job release",
            )
            .await?;
            Ok::<_, anyhow::Error>(
                pipeline_job::Entity::update_many()
                    .filter(pipeline_job::Column::Id.eq(job.id))
                    .filter(pipeline_job::Column::Status.eq("pending"))
                    .filter(pipeline_job::Column::DeferredCondition.is_not_null())
                    .col_expr(
                        pipeline_job::Column::DeferredCondition,
                        Expr::value(Option::<String>::None),
                    )
                    .exec(&tx)
                    .await
                    .context("db: release the held job")?
                    .rows_affected
                    == 1,
            )
        }
        .await;
        let (tx, outcome) = match taken {
            Ok(true) => {
                let (tx, written) = on_release(tx).await;
                (tx, written.map(|()| true))
            }
            other => (tx, other),
        };
        match outcome {
            Ok(released) => {
                tx.commit().await.context("db: commit held job release")?;
                Ok(released)
            }
            Err(error) => {
                if let Err(rollback) = tx.rollback().await {
                    tracing::error!(
                        job_id = job.id,
                        error = %format!("{rollback:#}"),
                        "held job release failed and could not be rolled back"
                    );
                }
                Err(error)
            }
        }
    })
    .await
}

/// Finish a held job that is not to run, as `outcome`, with `log` saying why,
/// and roll its stage and pipeline up: `skipped` for a held job whose `if:` is
/// false or one of whose needed jobs was skipped, or a matrix placeholder whose
/// needed job was; `failed` for a condition that has no answer. Both holds are
/// cleared: a finished row is never expanded or decided again.
/// `Some(pipeline)` when that finished the pipeline, for the caller's
/// completion hooks.
pub async fn settle_held_job(
    db: &DatabaseConnection,
    job: &pipeline_job::Model,
    outcome: &str,
    log: &str,
) -> Result<Option<pipeline::Model>> {
    anyhow::ensure!(
        matches!(outcome, "skipped" | "failed"),
        "a held job finishes skipped or failed, not {outcome}"
    );
    let stage = pipeline_ops::get_stage_by_id(db, job.stage_id)
        .await?
        .with_context(|| format!("stage {} of a held job is gone", job.stage_id))?;
    crate::contention::retry_transaction("settle a held job", || async {
        let tx = db.begin().await.context("db: begin held job settle")?;
        let step = async {
            pipeline_ops::lock_graph_for_stage_transition(
                &tx,
                stage.id,
                stage.pipeline_id,
                "held job settle",
            )
            .await?;
            let held = pipeline_ops::get_job(&tx, job.id).await?.is_some_and(|row| {
                row.status == "pending"
                    && (row.deferred_condition.is_some() || row.deferred_matrix.is_some())
            });
            let now = chrono::Utc::now().naive_utc();
            if !held
                || !pipeline_ops::settle_job_if_active(
                    &tx,
                    job.id,
                    outcome,
                    None,
                    Some(log),
                    Some(now),
                )
                .await?
            {
                return Ok(None);
            }
            pipeline_job::Entity::update_many()
                .filter(pipeline_job::Column::Id.eq(job.id))
                .col_expr(
                    pipeline_job::Column::DeferredCondition,
                    Expr::value(Option::<String>::None),
                )
                .col_expr(
                    pipeline_job::Column::DeferredMatrix,
                    Expr::value(Option::<String>::None),
                )
                .exec(&tx)
                .await
                .context("db: clear the finished job's hold")?;
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
            Ok(finished) => {
                tx.commit().await.context("db: commit held job settle")?;
                Ok(finished)
            }
            Err(error) => {
                if let Err(rollback) = tx.rollback().await {
                    tracing::error!(error = %format!("{rollback:#}"), "held job settle could not be rolled back");
                }
                Err(error)
            }
        }
    })
    .await
}
