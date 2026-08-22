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
//!
//! ## Credentials
//!
//! [`record_credential`] is the same two rules for the other half of "access":
//! the long-lived secrets an account carries. A personal token, an SSH key and
//! a repository's CI secret each wrote nothing at all — the journal knew an
//! account had logged in and did not know that a minute later it grew a token
//! scoped `repo` (card_4a8cb474a877). An incident review of a compromised
//! account starts at exactly that question.
//!
//! What such an entry may carry is the sharp edge, and it is the reason these
//! call sites are worth reading twice: the *name* of a token and its scopes, the
//! *title* and *fingerprint* of a key, the *name* of a secret — and never the
//! secret, never its ciphertext, and never `access_tokens.token_hash`, which is
//! the value the server authenticates by. A journal operators read must not
//! become a second credential store.

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

/// Record the appearance or revocation of a long-lived credential on an account.
///
/// The account-scoped sibling of [`record_grant`]: the resource is the account
/// itself, because that is what the credential opens. Repository-scoped
/// credentials — a CI secret — go through [`record_grant`] instead, since the
/// repository is the resource there and the journal is read per repository.
///
/// `details` is the caller's, and the module header states what may go in it.
pub(crate) async fn record_credential(
    state: &AppState,
    actor: &rg_core::audit::AuditActor,
    action: &str,
    user_id: i64,
    headers: &HeaderMap,
    details: serde_json::Value,
) {
    rg_core::audit::record(
        &state.db,
        actor,
        action,
        Some("user"),
        Some(user_id),
        actor.name(),
        Some(headers),
        Some(details),
    )
    .await;
}

/// Record the appearance or revocation of a credential that belongs to the
/// **instance** rather than to one account or one repository.
///
/// The third scope, and the widest. A runner token is neither: the runner polls
/// the queue and is handed a job from any repository whose labels it covers,
/// with that repository's CI secrets decrypted into the job's environment. So
/// the resource is not the admin who pressed the button and not any single
/// repository — it is the instance (card_2e514de7eefa).
///
/// The same two rules as its siblings apply, and the second one bites harder
/// here: `register` returns the token once and stores only its hash, so the
/// response is the only copy in existence. It must not become a second one in
/// the journal.
pub(crate) async fn record_instance_credential(
    state: &AppState,
    actor: &rg_core::audit::AuditActor,
    action: &str,
    resource: InstanceResource<'_>,
    headers: &HeaderMap,
    details: serde_json::Value,
) {
    rg_core::audit::record(
        &state.db,
        actor,
        action,
        Some(resource.kind),
        Some(resource.id),
        Some(resource.name),
        Some(headers),
        Some(details),
    )
    .await;
}

/// What an instance-scoped credential belongs to, as the journal's three
/// resource columns spell it.
///
/// One argument rather than three, because the three are one fact and are read
/// as one: `("runner", 4, "build-box")` is a resource, while three loose
/// positional values next to an action and a header map are a signature nobody
/// can call correctly from memory.
pub(crate) struct InstanceResource<'a> {
    /// `audit_log.resource_type` — the kind of thing, not the kind of secret.
    pub kind: &'a str,
    pub id: i64,
    /// What identifies it to a person after the row is gone.
    pub name: &'a str,
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
