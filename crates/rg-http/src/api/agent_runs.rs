//! Agent runs of a repository (ADR-0001 D10, card_231f9eb28ee3).
//!
//! A run is cancelled through the CI cancel of its pipeline, the door the
//! pipeline page already calls; a route of its own comes with the run list
//! that hands a page the run's id (card_f07e83bd6bf4).

use axum::response::IntoResponse;

use crate::error::AppError;
use crate::AppState;

/// Cancel run `run_id` for `actor_id`, answering `ok` when this request
/// cancelled it; every door to a cancel (this route, the CI cancel of the
/// run's pipeline) refuses the same way.
pub(crate) async fn cancel_response(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    run_id: i64,
    actor_id: i64,
    ok: impl FnOnce() -> axum::response::Response,
) -> axum::response::Response {
    let repo_admin = match crate::api::repo_access::administers(state, repo, actor_id).await {
        Ok(admin) => admin,
        Err(error) => return error.into_response(),
    };
    match rg_core::agent::run::cancel(&state.db_pools(), run_id, actor_id, repo_admin).await {
        Ok(rg_core::agent::run::CancelOutcome::Cancelled) => ok(),
        Ok(rg_core::agent::run::CancelOutcome::AlreadyEnded) => {
            AppError::conflict("the agent run has already ended").into_response()
        }
        Ok(rg_core::agent::run::CancelOutcome::NotAllowed) => not_allowed().into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// The refusal for a caller who may not cancel a run.
pub(crate) fn not_allowed() -> AppError {
    AppError::forbidden(
        "only the person who started this agent run, the bot's owner or a repository admin may cancel it",
    )
}
