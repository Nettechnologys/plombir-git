//! Which pipelines this process is running an embedded runner for.
//!
//! The embedded runner lives in memory and is not a poller: `spawn_internal_runner`
//! walks one pipeline's stages top to bottom, and once its task ends nothing
//! comes back to that pipeline. So when that task dies early — a panic, or the
//! error `runner.run()` only logs — the job it held stays `running`, the
//! watchdog returns it to `pending` ten minutes later, and on an instance with
//! `ci.external_runners = false` `pending` has no producer at all. The build
//! waits for the next restart (card_111ac7923d7c).
//!
//! Restarting a runner mid-life is only safe if the process can tell a pipeline
//! whose runner died from one whose runner is still working, and until now it
//! could not: the startup sweep is safe precisely because it takes the instant
//! the process began as its cutoff, and nothing older than that can have a
//! runner inside it. Mid-life that argument is gone, and re-spawning beside a
//! live runner runs somebody's deploy twice.
//!
//! The database cannot answer it either. A running job's `updated_at` is
//! refreshed by the runner's own heartbeat, so a stale timestamp is *evidence*
//! the runner stopped — but it is the same evidence a runner whose heartbeat
//! writes were failing would produce, and that one is still executing. This
//! registry answers from the process itself instead: a lease exists for exactly
//! as long as the task holding it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Live embedded runners by pipeline id, counted rather than flagged: a
/// pipeline handed to a runner twice must stay claimed until *both* leases are
/// gone, or the first one to finish would unclaim the second.
static LIVE_RUNNERS: OnceLock<Mutex<HashMap<i64, usize>>> = OnceLock::new();

fn live_runners() -> &'static Mutex<HashMap<i64, usize>> {
    LIVE_RUNNERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Claim `pipeline_id` for the embedded runner about to execute it.
///
/// Hold the returned lease inside the runner's task: it is released on drop,
/// which is what makes the claim outlive an early `return` and a panic alike
/// without the runner having to remember to say so.
pub fn claim_embedded_runner(pipeline_id: i64) -> EmbeddedRunnerLease {
    let mut live = live_runners().lock().unwrap_or_else(|poisoned| {
        // A panic inside another lease's release left the map locked. Its
        // contents are still a valid claim set — the only mutation under this
        // lock is one counter — so recovering keeps the registry answering
        // instead of poisoning every future runner.
        poisoned.into_inner()
    });
    *live.entry(pipeline_id).or_insert(0) += 1;
    EmbeddedRunnerLease { pipeline_id }
}

/// Whether this process currently has an embedded runner on `pipeline_id`.
///
/// `false` is the licence to start one: nothing in this process is executing
/// that pipeline, so whatever the database still shows for it was left by a
/// task that is gone.
pub fn has_embedded_runner(pipeline_id: i64) -> bool {
    live_runners()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&pipeline_id)
}

/// Run `task` with `lease` alive for exactly as long as it runs.
///
/// The claim and the work are one value on purpose. A lease parked in its own
/// binding beside the task can be released early — by an edit, or by a `drop`
/// somebody adds to quiet a warning — and a claim that ends before the runner
/// does is worse than no claim at all: it reads as "nothing is executing this
/// pipeline" while a runner still is, which is the one answer that starts a
/// second one. Nothing observes that from outside, so it is made unsayable
/// instead of asserted.
pub async fn holding<T>(
    lease: EmbeddedRunnerLease,
    task: impl std::future::Future<Output = T>,
) -> T {
    let _lease = lease;
    task.await
}

/// A live embedded runner's claim on one pipeline, released when dropped.
#[derive(Debug)]
pub struct EmbeddedRunnerLease {
    pipeline_id: i64,
}

impl Drop for EmbeddedRunnerLease {
    fn drop(&mut self) {
        let mut live = live_runners()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let std::collections::hash_map::Entry::Occupied(mut entry) = live.entry(self.pipeline_id)
        {
            *entry.get_mut() -= 1;
            if *entry.get() == 0 {
                entry.remove();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lease_claims_its_pipeline_and_releases_it_on_drop() {
        let pipeline_id = -101;
        assert!(!has_embedded_runner(pipeline_id));
        {
            let _lease = claim_embedded_runner(pipeline_id);
            assert!(has_embedded_runner(pipeline_id));
        }
        assert!(!has_embedded_runner(pipeline_id));
    }

    #[test]
    fn a_second_lease_keeps_the_claim_until_both_are_gone() {
        let pipeline_id = -102;
        let first = claim_embedded_runner(pipeline_id);
        let second = claim_embedded_runner(pipeline_id);
        drop(first);
        assert!(
            has_embedded_runner(pipeline_id),
            "the second runner is still executing this pipeline"
        );
        drop(second);
        assert!(!has_embedded_runner(pipeline_id));
    }

    #[tokio::test]
    async fn a_lease_handed_to_holding_outlives_the_task_it_carries() {
        let pipeline_id = -104;
        let lease = claim_embedded_runner(pipeline_id);
        let carried = holding(lease, async {
            assert!(
                has_embedded_runner(pipeline_id),
                "the claim must stand while the work it covers is still running"
            );
            "done"
        })
        .await;
        assert_eq!(carried, "done");
        assert!(!has_embedded_runner(pipeline_id));
    }

    #[test]
    fn a_panicking_runner_still_releases_its_pipeline() {
        let pipeline_id = -103;
        let unwound = std::panic::catch_unwind(|| {
            let _lease = claim_embedded_runner(pipeline_id);
            panic!("the runner task died the way this registry exists for");
        });
        assert!(unwound.is_err());
        assert!(
            !has_embedded_runner(pipeline_id),
            "a claim that outlives the task holding it locks the pipeline out of recovery forever"
        );
    }
}
