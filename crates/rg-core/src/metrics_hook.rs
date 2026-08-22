//! Metrics observer hooks for events that originate inside `rg-core`.
//!
//! `rg-core` sits *below* `rg-http`, where the Prometheus registry and the
//! `recorder` live, so it cannot call the recorder directly. Instead the HTTP
//! layer installs lightweight function-pointer observers here at startup, and
//! core-crate workers (e.g. the background webhook delivery task) forward their
//! events through the `record_*` helpers. When no observer is installed —
//! tests, the CLI, any binary that never boots the metrics registry — the
//! helpers are cheap no-ops.

use std::sync::OnceLock;

/// Observer invoked when a webhook delivery attempt finishes. The `bool` is
/// `true` when the endpoint returned a 2xx status, `false` for a non-2xx
/// response or a transport/SSRF error. Set once by the HTTP layer.
static WEBHOOK_DELIVERY_OBSERVER: OnceLock<fn(bool)> = OnceLock::new();

/// Install the webhook-delivery observer. Idempotent: the first installer wins,
/// later calls are ignored (a process only boots one metrics registry).
pub fn set_webhook_delivery_observer(observer: fn(bool)) {
    if WEBHOOK_DELIVERY_OBSERVER.set(observer).is_err() {
        // Idempotent installer: the first metrics registry wins.
    }
}

/// Record a completed webhook delivery. No-op when no observer is installed.
pub fn record_webhook_delivery(success: bool) {
    if let Some(observer) = WEBHOOK_DELIVERY_OBSERVER.get() {
        observer(success);
    }
}

/// Observer invoked when a pull request is merged. Recorded in the core service
/// (`merge_pr`) rather than the HTTP handler because three paths reach a merge —
/// the REST handler, auto-merge, and the merge queue — and only the service
/// function is common to all of them.
static PR_MERGED_OBSERVER: OnceLock<fn()> = OnceLock::new();

/// Install the pr-merged observer. Idempotent (first installer wins).
pub fn set_pr_merged_observer(observer: fn()) {
    if PR_MERGED_OBSERVER.set(observer).is_err() {
        // Idempotent installer: the first metrics registry wins.
    }
}

/// Record a merged pull request. No-op when no observer is installed.
pub fn record_pr_merged() {
    if let Some(observer) = PR_MERGED_OBSERVER.get() {
        observer();
    }
}

/// Observer invoked when a repository is created. Recorded in the core service
/// (`create_repo_with_opts`) rather than the HTTP handler because the import
/// subsystem also creates repositories through that funnel.
static REPO_CREATED_OBSERVER: OnceLock<fn()> = OnceLock::new();

/// Install the repo-created observer. Idempotent (first installer wins).
pub fn set_repo_created_observer(observer: fn()) {
    if REPO_CREATED_OBSERVER.set(observer).is_err() {
        // Idempotent installer: the first metrics registry wins.
    }
}

/// Record a created repository. No-op when no observer is installed.
pub fn record_repo_created() {
    if let Some(observer) = REPO_CREATED_OBSERVER.get() {
        observer();
    }
}

/// Observer invoked when a repository is deleted. Recorded in the core service
/// (`delete_repo`) rather than the HTTP handler because the handler is only one
/// of three paths that retire a repository: deleting an organization
/// (`retire_org_repositories`) and deleting an account
/// (`retire_account_repositories`) funnel through the same service function and
/// carry no handler of their own, so an organization with forty repositories
/// moved the `forgekeep_repositories` gauge by forty and
/// `forgekeep_repos_deleted_total` by nothing.
static REPO_DELETED_OBSERVER: OnceLock<fn()> = OnceLock::new();

/// Install the repo-deleted observer. Idempotent (first installer wins).
pub fn set_repo_deleted_observer(observer: fn()) {
    if REPO_DELETED_OBSERVER.set(observer).is_err() {
        // Idempotent installer: the first metrics registry wins.
    }
}

/// Record a deleted repository. No-op when no observer is installed.
pub fn record_repo_deleted() {
    if let Some(observer) = REPO_DELETED_OBSERVER.get() {
        observer();
    }
}

/// Observer invoked when a new user account is auto-provisioned by an external
/// identity source inside `rg-core` (currently LDAP first-login). The `&str` is
/// a low-cardinality provenance label (e.g. `"ldap"`). Recorded here rather than
/// in the HTTP handler because directory auto-provision happens deep in the
/// login service, below the recorder. Set once by the HTTP layer.
static USER_PROVISIONED_OBSERVER: OnceLock<fn(&str)> = OnceLock::new();

/// Install the user-provisioned observer. Idempotent (first installer wins).
pub fn set_user_provisioned_observer(observer: fn(&str)) {
    if USER_PROVISIONED_OBSERVER.set(observer).is_err() {
        // Idempotent installer: the first metrics registry wins.
    }
}

/// Record a user account auto-provisioned by an external identity source.
/// No-op when no observer is installed.
pub fn record_user_provisioned(source: &str) {
    if let Some(observer) = USER_PROVISIONED_OBSERVER.get() {
        observer(source);
    }
}

/// Observer invoked when the scheduled database backup finishes a run. The
/// `bool` is `true` for a snapshot that was written durably. Recorded here
/// rather than in the HTTP layer because the scheduler is a `rg-core` background
/// task; without it the only evidence a backup ran is a log line, and "the last
/// backup is older than N hours" is precisely the question an operator wants a
/// monitoring system — not a human reading logs — to answer.
static DB_BACKUP_OBSERVER: OnceLock<fn(bool)> = OnceLock::new();

/// Install the database-backup observer. Idempotent (first installer wins).
pub fn set_db_backup_observer(observer: fn(bool)) {
    if DB_BACKUP_OBSERVER.set(observer).is_err() {
        // Idempotent installer: the first metrics registry wins.
    }
}

/// Record a finished scheduled database backup. No-op when no observer is
/// installed.
pub fn record_db_backup(success: bool) {
    if let Some(observer) = DB_BACKUP_OBSERVER.get() {
        observer(success);
    }
}

/// Observer invoked when a CI job leaves the running set: the status the job
/// settled with, and its execution wall-clock when the runner measured one.
///
/// The external runner reports through `POST /runners/{id}/jobs/{job_id}/finish`
/// and is metered in the handler. The embedded runner has no handler to be
/// metered in — it settles the job itself, inside `rg-ci`, below the recorder —
/// so on the default configuration (`ci.external_runners = false`) nothing
/// produced `ci_jobs_total` or `ci_job_duration_seconds` at all
/// (card_e309fbb5a3fd).
static CI_JOB_FINISHED_OBSERVER: OnceLock<fn(&str, Option<std::time::Duration>)> = OnceLock::new();

/// Install the ci-job-finished observer. Idempotent (first installer wins).
pub fn set_ci_job_finished_observer(observer: fn(&str, Option<std::time::Duration>)) {
    if CI_JOB_FINISHED_OBSERVER.set(observer).is_err() {
        // Idempotent installer: the first metrics registry wins.
    }
}

/// Record a CI job that reached a terminal status. No-op when no observer is
/// installed.
pub fn record_ci_job_finished(status: &str, duration: Option<std::time::Duration>) {
    if let Some(observer) = CI_JOB_FINISHED_OBSERVER.get() {
        observer(status, duration);
    }
}

/// Observer invoked when a CI pipeline reaches a terminal status. Same split as
/// [`record_ci_job_finished`]: metered in the finish handler for an external
/// runner, and nowhere at all for the embedded one until this hook.
static CI_PIPELINE_FINISHED_OBSERVER: OnceLock<fn(&str)> = OnceLock::new();

/// Install the ci-pipeline-finished observer. Idempotent (first installer wins).
pub fn set_ci_pipeline_finished_observer(observer: fn(&str)) {
    if CI_PIPELINE_FINISHED_OBSERVER.set(observer).is_err() {
        // Idempotent installer: the first metrics registry wins.
    }
}

/// Record a CI pipeline that reached a terminal status. No-op when no observer
/// is installed.
pub fn record_ci_pipeline_finished(status: &str) {
    if let Some(observer) = CI_PIPELINE_FINISHED_OBSERVER.get() {
        observer(status);
    }
}
