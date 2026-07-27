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
