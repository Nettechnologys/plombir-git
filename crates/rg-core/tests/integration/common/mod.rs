//! Fixtures shared by the integration tests in this binary.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

/// A migrated SQLite database at `db_path`, without running the migration chain
/// for it.
///
/// Every test in this binary used to run all 104 migrations against its own
/// fresh file to arrive at the identical schema. Measured on the sibling
/// `rg-http` suite, that is where most of a run's wall clock went: 761 tests
/// took 597 s with a migration each and 112 s starting from a copy. So the chain
/// runs once per test binary ([`migrated_template`]) and each test starts from a
/// copy of its result.
///
/// Not a hand-written schema dump — the template is produced by
/// `run_migrations` itself, so a migration added tomorrow is in it the moment it
/// is in the chain, with nothing to keep in sync. The bookkeeping table travels
/// with the copy, so a test that runs the migrations again gets the no-op it
/// always got.
#[allow(dead_code)]
pub async fn migrated_sqlite(db_path: &Path, max_connections: u32) -> rg_db::DatabaseConnection {
    copy_migrated_template(db_path).await;
    rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        max_connections,
    )
    .await
    .expect("connect sqlite")
}

/// The migrated schema, built once per test binary.
///
/// Held in a `static`, so the directory outlives every test and is never
/// dropped; it is a `tempfile` directory, which the OS reclaims.
async fn migrated_template() -> &'static Path {
    static TEMPLATE: tokio::sync::OnceCell<tempfile::TempDir> = tokio::sync::OnceCell::const_new();
    TEMPLATE
        .get_or_init(|| async {
            let dir = tempfile::tempdir().expect("create the template directory");
            let path = dir.path().join("template.db");
            let db = rg_db::connect_with_pool(
                &format!("sqlite://{}?mode=rwc", path.display()),
                rg_db::TEST_CONNECT_TIMEOUT_SECS,
                60,
                1,
            )
            .await
            .expect("connect to the template database");
            rg_db::run_migrations(&db)
                .await
                .expect("migrate the template database");
            // Closing the pool is what checkpoints the WAL into the file being
            // copied. Without it a test would start from a database missing
            // every table the last checkpoint did not cover.
            db.close().await.expect("close the template database");
            dir
        })
        .await
        .path()
}

async fn copy_migrated_template(db_path: &Path) {
    let template = migrated_template().await.join("template.db");
    std::fs::copy(&template, db_path).unwrap_or_else(|error| {
        panic!(
            "copy the migrated template {} -> {}: {error}",
            template.display(),
            db_path.display()
        )
    });
    // Belt and braces: the close above normally removes these, but a WAL left
    // behind belongs to the copy as much as the pages do — half of it would be
    // a database missing its most recent commits.
    for suffix in ["-wal", "-shm"] {
        let sidecar = template.with_file_name(format!("template.db{suffix}"));
        if sidecar.exists() {
            let target = db_path.with_file_name(format!(
                "{}{suffix}",
                db_path.file_name().expect("db file name").to_string_lossy()
            ));
            std::fs::copy(&sidecar, &target).expect("copy the template WAL sidecar");
        }
    }
}

/// A `CiTrigger` that reports no CI config, so a hook run under test is just
/// the DB-visible half: PR head-SHA refresh, webhooks (none registered), the
/// watch fan-out and the code-index refresh. Triggering a real pipeline would
/// drag `rg-ci` in for nothing.
pub struct NoCi;

impl rg_core::ci::CiTrigger for NoCi {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        false
    }

    /// Mirrors `has_ci_config`: this double has no workflow files to
    /// match an event against, so it answers the same for every event.
    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        false
    }

    fn trigger_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        Box::pin(async { anyhow::bail!("no CI config in this test") })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// One accepted ref move, as `receive-pack` reports it to the hooks.
#[allow(dead_code)]
pub fn accepted_push(refname: &str, new_sha: &str) -> rg_git::protocol::receive_pack::RefUpdate {
    rg_git::protocol::receive_pack::RefUpdate {
        old_sha: "0".repeat(40),
        new_sha: new_sha.to_string(),
        refname: refname.to_string(),
        status: "ok".to_string(),
        message: String::new(),
    }
}

/// Run the post-push hooks the way a transport does, then drain the detached
/// work they spawned.
///
/// The tracker is local to the fixture on purpose — closing the process-global
/// one would reach into whatever else the test binary is running in parallel
/// (card_0ce231198269).
#[allow(dead_code)]
pub async fn run_post_push_hooks(
    db: &sea_orm::DatabaseConnection,
    repo_root: &Path,
    owner: &str,
    repo_name: &str,
    pusher_id: Option<i64>,
    ref_updates: &[rg_git::protocol::receive_pack::RefUpdate],
) {
    let ci = NoCi;
    let delivery_tracker = rg_core::task_tracker::TaskTracker::new();
    rg_core::push_hooks::post_push_hooks(
        &rg_core::push_hooks::PostPushParams {
            db,
            repo_path: &repo_root.join(format!("{owner}/{repo_name}.git")),
            repo_root,
            owner,
            repo_name,
            pusher_id,
            docker_enabled: false,
            external_runners: false,
            allow_host_runner: false,
            jwt_secret: Some("test-secret"),
            encryption_key: Some("test-encryption-key"),
            notifier: None,
            smtp_config: &None,
            ci_engine: &ci,
            external_url: None,
            delivery_tracker: &delivery_tracker,
        },
        ref_updates,
    )
    .await;
    drain(&delivery_tracker).await;
}

/// Await the detached work a call produced, on the caller's own tracker.
///
/// Fan-outs are spawned rather than awaited, so "the effect is not there yet"
/// is a normal state right after the producer returns; draining is what makes
/// the assertions deterministic.
///
/// The timeout is a hang-guard, not a deadline: a tracker that never drains
/// fails at any finite bound, while a tight one turns machine load into a red
/// suite.
#[allow(dead_code)]
pub async fn drain(tracker: &rg_core::task_tracker::TaskTracker) {
    tracker.close();
    tokio::time::timeout(std::time::Duration::from_secs(120), tracker.wait())
        .await
        .expect("delivery tracker drained within timeout");
}

/// Run one `git` command through the gateway every other call site uses.
#[allow(dead_code)]
pub fn git(args: &[&str], cwd: Option<&Path>) {
    rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(args, cwd)
        .expect("run git")
        .ensure_success()
        .expect("git command succeeds");
}
