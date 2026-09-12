//! Runner-owned job paths and recovery of work interrupted with the process.
//!
//! A normal job removes its checkout and transfer archives before returning,
//! but `SIGKILL`, the OOM killer and a container restart run none of that code.
//! Everything lives below one directory the runner chose for itself, so a
//! later runner process can safely retire entries that are too old to belong
//! to a live job. Keeping the producers and the sweep's recognisers here is
//! deliberate: a new filename must not silently land outside recovery.

use std::path::{Path, PathBuf};
use std::time::Duration;

const RUNNER_DIRECTORY: &str = "forgekeep-runner";
const JOBS_DIRECTORY: &str = "jobs";
const WORKSPACE_DOWNLOAD_SUFFIX: &str = ".workspace.download.tar";
const ARTIFACT_SUFFIX: &str = ".artifact.tar";

/// Longest execution deadline the server can hand an external runner.
pub(crate) const MAX_EXTERNAL_JOB_TIMEOUT_SECS: i64 = 86_400;

/// Twice the longest job deadline an external runner accepts (24 hours).
///
/// A second runner process can share `TMPDIR` during a rolling restart. Dating
/// entries instead of deleting the directory wholesale keeps that process's
/// live job, and the extra day also covers workspace download and publication
/// around the bounded execution itself.
pub(crate) const STALE_JOB_ENTRY_AGE: Duration =
    Duration::from_secs(2 * MAX_EXTERNAL_JOB_TIMEOUT_SECS as u64);

/// What one startup pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    pub(crate) removed: usize,
    pub(crate) retained: usize,
    pub(crate) failed: usize,
}

fn jobs_root_in(temp_root: &Path) -> PathBuf {
    temp_root.join(RUNNER_DIRECTORY).join(JOBS_DIRECTORY)
}

fn job_workspace_path_in(temp_root: &Path, job_id: i64) -> PathBuf {
    jobs_root_in(temp_root).join(job_id.to_string())
}

pub(crate) fn job_workspace_path(job_id: i64) -> PathBuf {
    job_workspace_path_in(&std::env::temp_dir(), job_id)
}

pub(crate) fn workspace_download_spool_path(workspace: &Path) -> PathBuf {
    workspace.with_extension(WORKSPACE_DOWNLOAD_SUFFIX.trim_start_matches('.'))
}

pub(crate) fn job_artifact_path(workspace: &Path) -> PathBuf {
    workspace.with_extension(ARTIFACT_SUFFIX.trim_start_matches('.'))
}

fn is_job_id(value: &str) -> bool {
    value
        .parse::<i64>()
        .is_ok_and(|job_id| job_id.to_string() == value)
}

fn is_owned_file_name(name: &str) -> bool {
    [WORKSPACE_DOWNLOAD_SUFFIX, ARTIFACT_SUFFIX]
        .iter()
        .any(|suffix| name.strip_suffix(suffix).is_some_and(is_job_id))
}

fn is_stale(metadata: &std::fs::Metadata, older_than: Duration) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= older_than)
}

/// Retire runner-owned job entries left by a process that could not clean up.
pub(crate) async fn sweep_stale_job_entries(older_than: Duration) -> SweepReport {
    sweep_stale_job_entries_in(&std::env::temp_dir(), older_than).await
}

async fn sweep_stale_job_entries_in(temp_root: &Path, older_than: Duration) -> SweepReport {
    let jobs_root = jobs_root_in(temp_root);
    let mut report = SweepReport::default();
    let mut entries = match tokio::fs::read_dir(&jobs_root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return report,
        Err(error) => {
            tracing::warn!(
                path = %jobs_root.display(),
                %error,
                "failed to read the runner jobs directory; abandoned work was not swept"
            );
            report.failed += 1;
            return report;
        }
    };

    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    path = %jobs_root.display(),
                    %error,
                    "failed to walk the runner jobs directory; remaining work was not swept"
                );
                report.failed += 1;
                break;
            }
        };

        let path = entry.path();
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to stat runner job work");
                report.failed += 1;
                continue;
            }
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let owned_directory = metadata.is_dir() && is_job_id(name);
        let owned_file = metadata.is_file() && is_owned_file_name(name);
        if (!owned_directory && !owned_file) || !is_stale(&metadata, older_than) {
            if owned_directory || owned_file {
                report.retained += 1;
            }
            continue;
        }

        let removal = if owned_directory {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        match removal {
            Ok(()) => {
                tracing::info!(path = %path.display(), "removed runner job work left by a previous run");
                report.removed += 1;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to remove abandoned runner job work");
                report.failed += 1;
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::{
        job_artifact_path, job_workspace_path_in, sweep_stale_job_entries_in,
        workspace_download_spool_path, SweepReport, STALE_JOB_ENTRY_AGE,
    };
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn age(path: &Path, by: Duration, directory: bool) {
        let when = SystemTime::now() - by;
        let file = std::fs::OpenOptions::new()
            .read(directory)
            .write(!directory)
            .open(path)
            .expect("open runner job entry to backdate it");
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("backdate runner job entry");
    }

    #[tokio::test]
    async fn stale_job_entries_are_removed_and_fresh_ones_are_kept() {
        let temp_root = tempfile::tempdir().expect("temporary root");
        let stale_workspace = job_workspace_path_in(temp_root.path(), 1);
        let fresh_workspace = job_workspace_path_in(temp_root.path(), 2);
        std::fs::create_dir_all(&stale_workspace).expect("stale workspace");
        std::fs::create_dir_all(&fresh_workspace).expect("fresh workspace");
        std::fs::write(stale_workspace.join("checkout"), b"old").expect("stale checkout");
        std::fs::write(fresh_workspace.join("checkout"), b"live").expect("fresh checkout");
        age(
            &stale_workspace,
            STALE_JOB_ENTRY_AGE + Duration::from_secs(60),
            true,
        );

        let stale_artifact = job_artifact_path(&job_workspace_path_in(temp_root.path(), 3));
        let fresh_artifact = job_artifact_path(&job_workspace_path_in(temp_root.path(), 4));
        std::fs::write(&stale_artifact, b"old artifact").expect("stale artifact");
        std::fs::write(&fresh_artifact, b"live artifact").expect("fresh artifact");
        age(
            &stale_artifact,
            STALE_JOB_ENTRY_AGE + Duration::from_secs(60),
            false,
        );

        let stale_download =
            workspace_download_spool_path(&job_workspace_path_in(temp_root.path(), 5));
        let fresh_download =
            workspace_download_spool_path(&job_workspace_path_in(temp_root.path(), 6));
        std::fs::write(&stale_download, b"old download").expect("stale download");
        std::fs::write(&fresh_download, b"live download").expect("fresh download");
        age(
            &stale_download,
            STALE_JOB_ENTRY_AGE + Duration::from_secs(60),
            false,
        );

        let unknown = stale_workspace.parent().unwrap().join("operator-notes");
        std::fs::create_dir(&unknown).expect("unknown directory");
        age(
            &unknown,
            STALE_JOB_ENTRY_AGE + Duration::from_secs(60),
            true,
        );

        let report = sweep_stale_job_entries_in(temp_root.path(), STALE_JOB_ENTRY_AGE).await;

        assert_eq!(
            report,
            SweepReport {
                removed: 3,
                retained: 3,
                failed: 0,
            }
        );
        for stale in [&stale_workspace, &stale_artifact, &stale_download] {
            assert!(!stale.exists(), "{} was not retired", stale.display());
        }
        for live in [&fresh_workspace, &fresh_artifact, &fresh_download, &unknown] {
            assert!(live.exists(), "{} was removed", live.display());
        }
    }

    #[test]
    fn producers_do_not_spell_runner_job_paths_outside_this_module() {
        let api = include_str!("api.rs");
        let commands = include_str!("commands.rs");
        let api_code = rust_source::production_rust_code_only(api);
        let commands_code = rust_source::production_rust_code_only(commands);

        assert!(!api.contains("workspace.download.tar"));
        assert!(!api.contains(".join(\"forgekeep-runner\")"));
        assert!(!commands.contains("artifact.tar"));
        assert!(api_code.contains("job_workspace_path(job_id)"));
        assert!(api_code.contains("workspace_download_spool_path(&workspace)"));
        assert!(commands_code.contains("job_artifact_path(workspace)"));
        assert!(commands_code.contains("sweep_stale_job_entries(STALE_JOB_ENTRY_AGE)"));
    }
}
