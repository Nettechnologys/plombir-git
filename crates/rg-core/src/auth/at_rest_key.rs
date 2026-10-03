//! The instance's at-rest encryption secret, published once per process.
//!
//! Every other reader of an encrypted column takes the key as a parameter —
//! `mirror::service`, `ci`, `user::service`, `push_hooks` all thread an
//! `encryption_key` down from the server that resolved it, and that is the
//! shape to prefer. This module exists for the one path where that thread does
//! not reach: webhook delivery.
//!
//! A delivery is triggered from wherever a repository event happens — an issue
//! opened, a release published, a PR merged, a branch pushed — and is then
//! *detached* into a background task ([`crate::task_tracker`]). Handing the key
//! to `webhook::service::trigger_event` would mean handing it to every one of
//! those call sites and to everything that calls *them*: dozens of service
//! signatures that have nothing to do with secrets. The dispatcher already
//! reaches for its process-wide wiring the same way (the delivery tracker, the
//! metrics observer), so the key is published here instead.
//!
//! Publishing is the server's job and happens twice on purpose: `plombir-git
//! serve` publishes right after the key preflight, before anything can
//! dispatch, and `rg_http::AppState::new` publishes as well so an embedder or a
//! test that builds a state without the full boot is covered too. Both pass the
//! same value, and [`publish`] takes the first one.
//!
//! **A process serves one instance.** Publishing a second, different key does
//! not replace the first — it warns and keeps the published value, because a
//! silent swap would make every delivery sign with the wrong secret. The test
//! suite spawns many apps per process and gives them all the same key, which is
//! the same assumption from the other side.
//!
//! When nothing was published, [`resolve`] answers `None` and the dispatcher
//! turns that into a recorded delivery error rather than an unsigned POST — see
//! `webhook::service::secret_for_delivery`.

use std::sync::OnceLock;

static AT_REST_KEY: OnceLock<String> = OnceLock::new();

/// Publish the at-rest encryption secret for this process.
///
/// Idempotent for the same value. A second, different value is refused (the
/// first one stays in force) and logged, because the paths that read it cannot
/// tell a re-publish from a mistake and a wrong key here means correctly
/// delivered webhooks carrying a signature no receiver accepts.
pub fn publish(secret: &str) {
    if let Err(_ignored) = AT_REST_KEY.set(secret.to_string()) {
        if AT_REST_KEY.get().map(String::as_str) != Some(secret) {
            tracing::warn!(
                "a second, different at-rest encryption key was published to this process; \
                 keeping the first one — webhook deliveries stay signed with the key the \
                 database was opened with"
            );
        }
    }
}

/// The published at-rest secret, or `None` when this process never published
/// one.
pub fn resolve() -> Option<&'static str> {
    AT_REST_KEY.get().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both halves in one test on purpose: the global is process-wide, so two
    /// tests asserting on it would depend on which ran first.
    #[test]
    fn the_first_published_key_is_the_one_that_stays() {
        assert_eq!(resolve(), None, "nothing published yet");

        publish("the-real-at-rest-key");
        assert_eq!(resolve(), Some("the-real-at-rest-key"));

        // Idempotent for the same value, and a different value never displaces
        // the key the database was opened with.
        publish("the-real-at-rest-key");
        publish("a-different-key");
        assert_eq!(resolve(), Some("the-real-at-rest-key"));
    }
}
