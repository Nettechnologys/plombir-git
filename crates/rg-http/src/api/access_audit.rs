//! The journal entry every change to *who may do what in a repository* writes.
//!
//! `audit_log` is what an incident is reconstructed from afterwards, and the
//! five endpoints that hand out repository access wrote nine entries between
//! them — all nine from `api::orgs`. Adding a collaborator, editing the
//! push allow-list of a protected branch, editing a protected tag's, and
//! editing the approver list of a deployment environment wrote **nothing**
//! (card_06393f036456). An owner could put themselves on the exception list of
//! `main`, push, and take themselves off again without leaving a line anywhere.
//!
//! Two rules live here rather than at twelve call sites.
//!
//! **The actor is named before the grant is written.** A failed name lookup is
//! the server's fault, and answering it after the access change has landed
//! would leave the one record of who made it blank — which is exactly how
//! `admin.unlock_user` came to log an anonymous unlock (card_86f40189bc71).
//! Resolving first means such a failure is a `5xx` from a request that changed
//! nothing.
//!
//! **An allow-list is journalled whole, by name.** Not the delta: an entry
//! saying "added #7" forces whoever reads it to replay the whole history to
//! learn who could push at any given moment, and an id alone is the very
//! problem this phase exists for — an owner could not find out who `#3` was.
//! The list as it now stands, spelled in usernames, answers "who may do this"
//! from one row.

use axum::http::HeaderMap;

use crate::api::user_ref::AllowedUser;
use crate::error::AppError;
use crate::AppState;

/// Name the acting account before the access change is made.
///
/// See the module header: a failure here must abort the request, not follow it.
pub(crate) async fn grant_actor(
    state: &AppState,
    actor_id: i64,
) -> Result<rg_core::audit::AuditActor, AppError> {
    rg_core::audit::AuditActor::resolve(&state.db, actor_id)
        .await
        .map_err(AppError::from)
}

/// Record one change to who may do what in `repository`.
///
/// `resource_name` is `owner/name`, matching `repo.create` / `repo.transfer`:
/// a bare repository name does not identify a repository on an instance where
/// two owners may each have a `docs`.
pub(crate) async fn record_grant(
    state: &AppState,
    actor: &rg_core::audit::AuditActor,
    action: &str,
    owner: &str,
    repository: &rg_db::entities::repository::Model,
    headers: &HeaderMap,
    details: serde_json::Value,
) {
    let resource_name = format!("{owner}/{}", repository.name);
    rg_core::audit::record(
        &state.db,
        actor,
        action,
        Some("repo"),
        Some(repository.id),
        Some(&resource_name),
        Some(headers),
        Some(details),
    )
    .await;
}

/// An allow-list as the journal spells it: usernames, in the order the rule
/// stores them.
///
/// An id whose account no longer resolves keeps its place as `#7` rather than
/// vanishing — a list that answers "who may push here" must not quietly
/// shorten itself, in the journal for the same reason it must not in the API.
pub(crate) fn named_grant_list(allowed: &[AllowedUser]) -> Vec<String> {
    allowed
        .iter()
        .map(|user| match &user.username {
            Some(username) => username.clone(),
            None => format!("#{}", user.user_id),
        })
        .collect()
}
