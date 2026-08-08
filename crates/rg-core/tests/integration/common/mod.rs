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

/// The migrated schema, built once per *binary build* and shared by every
/// process that runs it.
///
/// Not a hand-written schema dump: it is produced by `run_migrations` itself, so
/// what a test starts from is exactly what the chain produces — a migration
/// added tomorrow is in the template the moment it is in the chain, with nothing
/// to keep in sync. The bookkeeping table travels with it, so a test that runs
/// the migrations again gets the same no-op it always did.
///
/// It used to be a per-process `OnceCell`, which was right for `cargo test`
/// (one process per binary, so the chain ran once for hundreds of tests) and
/// wrong for the runner this suite is actually gated by: nextest gives **every
/// test its own process**, so the cache hit exactly once and every test paid the
/// full chain again — 0.46 s measured, times 2045 tests.
///
/// The cache key is this executable's own modification time. Any change to a
/// migration rebuilds `rg-db`, which relinks this binary, so a binary newer than
/// its template is exactly the condition under which the template is stale.
/// Nothing here can hand a test the schema of an older chain.
///
/// Publication is a directory rename, which is atomic and fails when the
/// destination exists — so concurrent first-starters race harmlessly: the loser
/// throws its copy away and reads the winner's. A lock would only save the few
/// duplicate builds in the first wave of a cold run.
async fn migrated_template() -> &'static Path {
    static TEMPLATE: tokio::sync::OnceCell<std::path::PathBuf> = tokio::sync::OnceCell::const_new();
    TEMPLATE
        .get_or_init(|| async {
            let published = template_dir_for_this_build();
            if published.join("template.db").exists() {
                return published;
            }

            let staging = published.with_file_name(format!(
                "{}.building-{}",
                published
                    .file_name()
                    .expect("template directory name")
                    .to_string_lossy(),
                std::process::id()
            ));
            discard_directory(&staging);
            std::fs::create_dir_all(&staging).expect("create the template staging directory");

            let path = staging.join("template.db");
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

            if std::fs::rename(&staging, &published).is_err() {
                // Somebody published first. Theirs is the same schema by
                // construction, so drop ours rather than racing to replace it.
                discard_directory(&staging);
            }
            assert!(
                published.join("template.db").exists(),
                "no migrated template at {} after publication",
                published.display()
            );
            published
        })
        .await
        .as_path()
}

/// Remove a directory if it is there, and stay quiet when it is not.
///
/// `let _ =` on a `Result` is denied workspace-wide, and rightly — but here the
/// *absent* case is the normal one and carries no information, while a removal
/// that failed for any other reason is worth a line, because the next thing
/// this code does is try to create that same path.
fn discard_directory(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_dir_all(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "could not clear the template staging directory {}: {error}",
                path.display()
            );
        }
    }
}

/// Where this build's template lives: beside the test binary, under a name
/// carrying the binary's modification time.
///
/// Under `target/`, so `cargo clean` takes it and nothing outside the build
/// directory is written. Per binary *and* per build, so two binaries — or the
/// same binary before and after a migration — never share a file.
fn template_dir_for_this_build() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("the test executable's own path");
    let stamp = exe
        .metadata()
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        // No mtime (an exotic filesystem): fall back to a per-process directory,
        // which is the old behaviour rather than a shared file we cannot date.
        .unwrap_or_else(|| u128::from(std::process::id()));
    let name = exe
        .file_name()
        .expect("the test executable's file name")
        .to_string_lossy()
        .into_owned();
    let base = exe
        .parent()
        .expect("the test executable's directory")
        .join(".forgekeep-test-schema");
    std::fs::create_dir_all(&base).expect("create the shared template directory");
    base.join(format!("{name}-{stamp}"))
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
