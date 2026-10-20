//! The steps of a job that runs as steps.
//!
//! The rows are written by three hands that do not know about each other: the
//! trigger that publishes the graph (`pending`), the runner that runs the job
//! (`running`, then the outcome) and a retry that runs it again (back to
//! `pending`). A runner's write lands only while the job it belongs to is
//! `running` — a step reported after the job was canceled or reclaimed is a
//! report about work nobody owns any more.

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, Query};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

use crate::entities::{pipeline_job, pipeline_job_step};

/// The statuses a step row can carry. `pending` and `running` are the
/// runner's progress; the rest are outcomes.
pub const STEP_STATUSES: [&str; 6] = [
    "pending",
    "running",
    "success",
    "failure",
    "skipped",
    "cancelled",
];

/// Write the job's steps — the JSON a runner executes, and one `pending` row
/// per step for the job page — in the graph's transaction.
pub async fn create_job_steps(
    db: &impl ConnectionTrait,
    job_id: i64,
    steps_json: &str,
    names: &[String],
) -> Result<()> {
    pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .col_expr(pipeline_job::Column::Steps, Expr::value(steps_json))
        .exec(db)
        .await
        .context("db: write job steps")?;
    if names.is_empty() {
        return Ok(());
    }
    pipeline_job_step::Entity::insert_many(names.iter().enumerate().map(|(index, name)| {
        pipeline_job_step::ActiveModel {
            job_id: Set(job_id),
            number: Set(i32::try_from(index + 1).unwrap_or(i32::MAX)),
            name: Set(name.clone()),
            status: Set("pending".to_string()),
            conclusion: Set(None),
            exit_code: Set(None),
            log: Set(None),
            started_at: Set(None),
            finished_at: Set(None),
            ..Default::default()
        }
    }))
    .exec(db)
    .await
    .context("db: create job step rows")?;
    Ok(())
}

/// Put every step of a job back to `pending`, for the run that is about to
/// start: what the previous run of the same job reported is not this run's.
pub async fn reset_job_steps(db: &impl ConnectionTrait, job_id: i64) -> Result<()> {
    pipeline_job_step::Entity::update_many()
        .filter(pipeline_job_step::Column::JobId.eq(job_id))
        .col_expr(pipeline_job_step::Column::Status, Expr::value("pending"))
        .col_expr(
            pipeline_job_step::Column::Conclusion,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            pipeline_job_step::Column::ExitCode,
            Expr::value(Option::<i32>::None),
        )
        .col_expr(
            pipeline_job_step::Column::Log,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            pipeline_job_step::Column::StartedAt,
            Expr::value(Option::<chrono::NaiveDateTime>::None),
        )
        .col_expr(
            pipeline_job_step::Column::FinishedAt,
            Expr::value(Option::<chrono::NaiveDateTime>::None),
        )
        .exec(db)
        .await
        .context("db: reset job steps")?;
    Ok(())
}

/// How one step ended, as a runner reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport<'a> {
    pub number: i32,
    /// `running`, or one of the outcomes.
    pub status: &'a str,
    pub conclusion: Option<&'a str>,
    pub exit_code: Option<i32>,
    pub log: Option<&'a str>,
}

/// Record a runner's report of one step, only while its job is `running`.
///
/// `running` stamps `started_at`; an outcome stamps `finished_at`. The server
/// keeps the clock, so a runner on another machine cannot skew the durations
/// the job page shows. Returns whether a row moved.
pub async fn report_job_step(
    db: &impl ConnectionTrait,
    job_id: i64,
    report: &StepReport<'_>,
) -> Result<bool> {
    let now = chrono::Utc::now().naive_utc();
    let mut update = pipeline_job_step::Entity::update_many()
        .filter(pipeline_job_step::Column::JobId.eq(job_id))
        .filter(pipeline_job_step::Column::Number.eq(report.number))
        .filter(
            pipeline_job_step::Column::JobId.in_subquery(
                Query::select()
                    .column(pipeline_job::Column::Id)
                    .from(pipeline_job::Entity)
                    .and_where(pipeline_job::Column::Id.eq(job_id))
                    .and_where(pipeline_job::Column::Status.eq("running"))
                    .to_owned(),
            ),
        )
        .col_expr(
            pipeline_job_step::Column::Status,
            Expr::value(report.status),
        );
    if report.status == "running" {
        update = update.col_expr(pipeline_job_step::Column::StartedAt, Expr::value(now));
    } else {
        update = update
            .col_expr(
                pipeline_job_step::Column::Conclusion,
                Expr::value(report.conclusion.map(str::to_string)),
            )
            .col_expr(
                pipeline_job_step::Column::ExitCode,
                Expr::value(report.exit_code),
            )
            .col_expr(
                pipeline_job_step::Column::Log,
                Expr::value(report.log.map(str::to_string)),
            )
            .col_expr(pipeline_job_step::Column::FinishedAt, Expr::value(now));
    }
    Ok(update
        .exec(db)
        .await
        .context("db: record a job step")?
        .rows_affected
        == 1)
}

/// One step as the job page lists it — everything but the log.
#[derive(Debug, Clone, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct StepSummary {
    pub job_id: i64,
    pub number: i32,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: Option<chrono::NaiveDateTime>,
    pub finished_at: Option<chrono::NaiveDateTime>,
    pub has_log: bool,
}

/// The steps of `job_ids`, in job and step order, without their logs.
pub async fn list_step_summaries(
    db: &impl ConnectionTrait,
    job_ids: &[i64],
) -> Result<Vec<StepSummary>> {
    if job_ids.is_empty() {
        return Ok(Vec::new());
    }
    pipeline_job_step::Entity::find()
        .select_only()
        .column(pipeline_job_step::Column::JobId)
        .column(pipeline_job_step::Column::Number)
        .column(pipeline_job_step::Column::Name)
        .column(pipeline_job_step::Column::Status)
        .column(pipeline_job_step::Column::Conclusion)
        .column(pipeline_job_step::Column::ExitCode)
        .column(pipeline_job_step::Column::StartedAt)
        .column(pipeline_job_step::Column::FinishedAt)
        .column_as(Expr::cust("log IS NOT NULL"), "has_log")
        .filter(pipeline_job_step::Column::JobId.is_in(job_ids.iter().copied()))
        .order_by_asc(pipeline_job_step::Column::JobId)
        .order_by_asc(pipeline_job_step::Column::Number)
        .into_model::<StepSummary>()
        .all(db)
        .await
        .context("db: list job steps")
}

/// One step of one job, log included.
pub async fn find_job_step(
    db: &impl ConnectionTrait,
    job_id: i64,
    number: i32,
) -> Result<Option<pipeline_job_step::Model>> {
    pipeline_job_step::Entity::find()
        .filter(pipeline_job_step::Column::JobId.eq(job_id))
        .filter(pipeline_job_step::Column::Number.eq(number))
        .one(db)
        .await
        .context("db: find job step")
}
