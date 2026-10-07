//! Ownership boundaries for synchronous work called from async services.

use anyhow::{Context, Result};

/// Run one complete synchronous Git phase without occupying a Tokio worker.
///
/// Callers move every path, guarded remote and credential needed by the phase
/// into `operation`. Inner Git errors pass through unchanged; a panic or a
/// cancelled blocking task gains the name of the operation that was lost.
pub(crate) async fn run_blocking_git<T, F>(what: &'static str, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .with_context(|| format!("{what} blocking task failed"))?
}

#[cfg(test)]
mod tests {
    use super::run_blocking_git;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// Filling every async worker with a Git wait must still leave a worker for
    /// a cheap request. Running `operation` directly makes the timing tooth fail
    /// while both workers wait at the barrier.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn synchronous_git_work_does_not_occupy_async_workers() {
        use std::sync::{Arc, Barrier};
        use std::time::{Duration, Instant};

        const PARALLEL_GIT_PHASES: usize = 2;
        let release = Arc::new(Barrier::new(PARALLEL_GIT_PHASES + 1));
        let release_thread = {
            let release = Arc::clone(&release);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(250));
                release.wait();
            })
        };

        let started_at = Instant::now();
        let (operations, entered): (Vec<_>, Vec<_>) = (0..PARALLEL_GIT_PHASES)
            .map(|marker| {
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let release = Arc::clone(&release);
                let operation = tokio::spawn(async move {
                    run_blocking_git("test Git phase", move || {
                        entered_tx.send(()).expect("test still awaits start signal");
                        release.wait();
                        Ok(marker)
                    })
                    .await
                });
                (operation, entered_rx)
            })
            .unzip();

        for entered in entered {
            entered.await.expect("Git phase starts");
        }
        let cheap_task = tokio::spawn(async { tokio::task::yield_now().await });
        tokio::time::timeout(Duration::from_millis(100), cheap_task)
            .await
            .expect("a cheap async task was delayed by synchronous Git work")
            .expect("cheap async task joins");
        assert!(
            started_at.elapsed() < Duration::from_millis(200),
            "synchronous Git work occupied every async worker"
        );

        for (expected, operation) in operations.into_iter().enumerate() {
            assert_eq!(
                operation
                    .await
                    .expect("Git phase task joins")
                    .expect("Git phase succeeds"),
                expected
            );
        }
        release_thread.join().expect("release thread joins");
    }

    #[tokio::test]
    async fn a_panicked_git_task_keeps_its_operation_context() {
        let error = run_blocking_git("fetching a fork", || -> anyhow::Result<()> {
            panic!("injected Git task panic")
        })
        .await
        .expect_err("a panicked blocking task must fail the caller");

        assert!(
            error
                .to_string()
                .contains("fetching a fork blocking task failed"),
            "JoinError lost the Git operation context: {error:#}"
        );
    }

    /// The helper probes scheduling. This guard proves that each production
    /// network Git sink is lexically inside the helper, so a later gix-only
    /// `spawn_blocking` cannot make the contract pass by itself.
    #[test]
    fn production_network_git_sinks_use_the_blocking_boundary() {
        let import = include_str!("import/service.rs");
        let mirror = include_str!("mirror/service.rs");
        let pulls = include_str!("pull_request/service.rs");

        for (source, function, blocking_calls) in [
            (import, "clone_repo", &["run"] as &[&str]),
            (
                mirror,
                "run_sync_pass",
                &["run_git_clone_mirror", "run_git_remote_update"],
            ),
            // The fork fetch is one helper both share, so `--no-tags` cannot
            // drift between them.
            (pulls, "compute_diff", &["run_fork_fetch"]),
            (pulls, "merge_claimed_pr", &["run_fork_fetch"]),
        ] {
            let boundary = rust_source::production_function_call_sites(
                source,
                function,
                &["run_blocking_git"],
            );
            assert_eq!(
                boundary.len(),
                1,
                "{function} must have one network Git blocking boundary, found {boundary:?}"
            );

            for blocking_call in blocking_calls {
                let calls = rust_source::production_function_call_sites(
                    source,
                    function,
                    &[*blocking_call],
                );
                assert_eq!(
                    calls.len(),
                    1,
                    "{function} must make one `{blocking_call}` call, found {calls:?}"
                );
                assert!(
                    rust_source::call_site_contains(source, boundary[0], calls[0]),
                    "{function}'s `{blocking_call}` call is outside its blocking boundary"
                );
            }
        }

        let wiki_boundaries = rust_source::production_function_call_sites(
            import,
            "import_wiki_pages_from_destination",
            &["run_blocking_git"],
        );
        assert_eq!(
            wiki_boundaries.len(),
            1,
            "wiki import must have one network Git blocking boundary, found {wiki_boundaries:?}"
        );
        let wiki_git_calls = rust_source::production_function_call_sites(
            import,
            "import_wiki_pages_from_destination",
            &["run"],
        );
        assert_eq!(
            wiki_git_calls.len(),
            2,
            "wiki import must keep both clone and ls-remote Git calls, found {wiki_git_calls:?}"
        );
        assert!(
            wiki_git_calls
                .iter()
                .all(|call| rust_source::call_site_contains(import, wiki_boundaries[0], *call)),
            "wiki import's clone and ls-remote calls must stay inside its blocking boundary"
        );
    }

    /// Local Git and filesystem phases are just as capable of exhausting the
    /// runtime as network Git. Keep every size-dependent sink lexically inside
    /// one of the blocking boundaries owned by its async service function.
    #[test]
    fn production_local_git_and_fs_sinks_use_the_blocking_boundary() {
        let review = include_str!("review/service.rs");
        let repositories = include_str!("repo/service.rs");
        let merge_queue = include_str!("pull_request/merge_queue.rs");

        for (source, function, expected_boundaries, blocking_calls) in [
            (
                review,
                "apply_suggestions",
                1,
                &["blob_size", "run", "update_files_in_commit"] as &[&str],
            ),
            (
                repositories,
                "create_repo_with_post_commit",
                2,
                &[
                    "create_dir_all",
                    "create_dir",
                    "into",
                    "set_bare_repo_head_to_branch",
                    "auto_init_repo",
                ],
            ),
            (
                repositories,
                "create_or_update_file",
                1,
                &[
                    "exists",
                    "try_get_branch_sha",
                    "read_path_entry",
                    "StagedWorktree::new",
                    "create_dir_all",
                    "write",
                    "run",
                    "run_with_env",
                    "verify_created_commit",
                    "push_branch_with_lease",
                ],
            ),
            (
                repositories,
                "delete_file",
                1,
                &[
                    "exists",
                    "try_get_branch_sha",
                    "read_path_entry",
                    "StagedWorktree::new",
                    "run",
                    "run_with_env",
                    "verify_created_commit",
                    "push_branch_with_lease",
                ],
            ),
            (
                repositories,
                "fork_repo",
                1,
                &[
                    "UncommittedRepositoryStorage::new",
                    "create_dir_all",
                    "path_to_git_url",
                    "path_for_new_entry",
                    "run",
                ],
            ),
            (merge_queue, "cleanup_merge_group_ref", 2, &["run"]),
            (
                merge_queue,
                "retire_losing_merge_group_pipeline",
                1,
                &["run"],
            ),
            (
                merge_queue,
                "ensure_merge_group_ci",
                4,
                &["run", "run_with_env", "merge_group_tree", "publish"],
            ),
        ] {
            let boundaries = rust_source::production_function_call_sites(
                source,
                function,
                &["run_blocking_git"],
            );
            assert_eq!(
                boundaries.len(),
                expected_boundaries,
                "{function} must keep {expected_boundaries} local Git/FS blocking boundaries, found {boundaries:?}"
            );

            for blocking_call in blocking_calls {
                let calls = rust_source::production_function_call_sites(
                    source,
                    function,
                    &[*blocking_call],
                );
                assert!(
                    !calls.is_empty(),
                    "{function} must keep at least one `{blocking_call}` call"
                );
                assert!(
                    calls.iter().all(|call| boundaries
                        .iter()
                        .any(|boundary| rust_source::call_site_contains(source, *boundary, *call))),
                    "{function}'s `{blocking_call}` calls must stay inside a blocking boundary: {calls:?}"
                );
            }
        }

        let fork_journal = rust_source::production_function_call_sites(
            repositories,
            "fork_repo",
            &["open_repository_creation"],
        );
        let fork_boundary = rust_source::production_function_call_sites(
            repositories,
            "fork_repo",
            &["run_blocking_git"],
        );
        assert_eq!(
            fork_journal.len(),
            1,
            "fork_repo must open exactly one recovery entry before it can clone"
        );
        assert_eq!(
            fork_boundary.len(),
            1,
            "fork_repo must own exactly one blocking clone boundary"
        );
        assert!(
            fork_journal[0].open_paren < fork_boundary[0].open_paren,
            "fork_repo must journal the target before blocking clone work can claim it"
        );

        let contents_api = include_str!("../../rg-http/src/api/repo_content.rs");
        for handler in ["create_or_update_file", "delete_file"] {
            for blocking_read in ["previous_branch_sha", "latest_commit_sha_or_log"] {
                let calls = rust_source::production_function_call_sites(
                    contents_api,
                    handler,
                    &[blocking_read],
                );
                assert!(
                    calls.is_empty(),
                    "the Contents {handler} handler must receive ref SHAs from its blocking service outcome, found `{blocking_read}` at {calls:?}"
                );
            }
        }
    }
}
