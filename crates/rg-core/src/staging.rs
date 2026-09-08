//! Where an in-flight request spools to disk, and who retires what it left.
//!
//! Four handlers stream a request body into a private file under
//! `<repo_root>/.tmp/` before they know whether the request will succeed:
//! package publishes, release assets, git request bodies and issue attachments.
//! Each of them relies on `tempfile::TempPath` — or an explicit `remove_file` —
//! to retire the spool on every path out of the handler, which covers every
//! outcome the *process* survives: success, refusal, transport error, panic.
//!
//! It does not cover the process not surviving. `SIGKILL`, the OOM killer and a
//! container restart run no destructors, so a spool caught mid-write is simply
//! left on the volume — and until this module existed nothing ever read those
//! directories again, so a 512 MiB package upload interrupted by a restart was
//! a permanent 512 MiB. The operator learns about it when `.tmp` has eaten the
//! partition the repositories themselves live on.
//!
//! [`sweep_stale_spools`] is the reverse arc: one pass at startup that retires
//! spools no longer plausibly attached to a live request. It is deliberately
//! ONE pass over a list, not a fourth copy of a cleanup loop — the directories
//! were spelled in three different modules, and a fifth staging area added
//! tomorrow has to be swept without anybody remembering to extend a sweep.
//! [`StagingArea::ALL`] is that single declaration, and
//! `staging_registry_is_the_only_producer_of_tmp_paths` is what keeps a new
//! area from being spelled past it.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a spool must have gone untouched before the startup sweep retires
/// it.
///
/// The sweep runs before this process serves anything, so its own requests
/// cannot be caught by it. The age bound is for the case it cannot rule out:
/// a second process — a rolling restart with an overlap window, or a second
/// container pointed at the same `repo_root` — with a genuinely live upload in
/// flight. Twelve hours is far above any plausible single request (the largest
/// ceiling any of these areas carries is 512 MiB) and far below "forever",
/// which is what the retention was before.
pub const STALE_SPOOL_AGE: Duration = Duration::from_secs(12 * 60 * 60);

/// One staging area: a directory under `<repo_root>/.tmp/` that a handler
/// spools an in-flight request body into.
///
/// Adding a variant is what registers a new area with the startup sweep. The
/// guard test in this module fails if a handler builds a `.tmp` path any other
/// way, so the two cannot drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingArea {
    /// `POST` of a package to any of the registries — up to
    /// `[server].package_upload_max_bytes` per file.
    PackageUploads,
    /// A release asset upload — up to `[server].package_upload_max_bytes`.
    ReleaseUploads,
    /// A git request body (`git-receive-pack` push, `git-upload-pack`
    /// negotiation) staged before it is handed to rg-git.
    GitRequests,
    /// An issue / pull-request / comment attachment, staged before it reaches
    /// the blob store.
    Attachments,
}

impl StagingArea {
    /// Every staging area this binary spools into.
    ///
    /// The startup sweep iterates exactly this slice; nothing else enumerates
    /// staging directories.
    pub const ALL: &'static [StagingArea] = &[
        StagingArea::PackageUploads,
        StagingArea::ReleaseUploads,
        StagingArea::GitRequests,
        StagingArea::Attachments,
    ];

    /// The directory name under `<repo_root>/.tmp/`.
    pub const fn directory_name(self) -> &'static str {
        match self {
            StagingArea::PackageUploads => "package-uploads",
            StagingArea::ReleaseUploads => "release-uploads",
            StagingArea::GitRequests => "git-requests",
            StagingArea::Attachments => "attachments",
        }
    }

    /// Where this area lives under `repo_root`.
    ///
    /// The `.tmp` literal is spelled here and only here on purpose: the guard
    /// test anchors on it, so a handler that builds its own `.tmp` path is a
    /// red test rather than a directory nobody sweeps.
    pub fn path_in(self, repo_root: &Path) -> PathBuf {
        repo_root.join(".tmp").join(self.directory_name())
    }
}

/// What one sweep did, so the caller can say it in a single log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Spools older than the bound that were deleted.
    pub removed: usize,
    /// Spools young enough to still belong to a live request.
    pub retained: usize,
    /// Entries the sweep could not read or could not delete.
    pub failed: usize,
}

impl SweepReport {
    fn merge(&mut self, other: SweepReport) {
        self.removed += other.removed;
        self.retained += other.retained;
        self.failed += other.failed;
    }
}

/// Retire spools left behind by a stop that ran no destructors.
///
/// Best-effort by construction: a directory that does not exist yet is not a
/// problem (no request of that kind has run), and a single unreadable or
/// undeletable entry is counted and logged rather than allowed to hold up
/// startup. Serving with an unswept spool is strictly better than not serving.
///
/// Only regular files are touched. A symlink is never followed and never
/// removed — nothing in these directories creates one, so its presence means
/// something the sweep does not understand put it there.
pub async fn sweep_stale_spools(repo_root: &Path, older_than: Duration) -> SweepReport {
    let mut report = SweepReport::default();
    for area in StagingArea::ALL {
        report.merge(sweep_one_area(&area.path_in(repo_root), older_than).await);
    }
    report
}

async fn sweep_one_area(directory: &Path, older_than: Duration) -> SweepReport {
    let mut report = SweepReport::default();

    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        // Nothing of this kind has been staged on this instance yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return report,
        Err(error) => {
            tracing::warn!(
                path = %directory.display(),
                %error,
                "failed to read a staging directory; leftover spools were not swept"
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
                    path = %directory.display(),
                    %error,
                    "failed to walk a staging directory; the remaining spools were not swept"
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
                tracing::warn!(path = %path.display(), %error, "failed to stat a staged spool");
                report.failed += 1;
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }

        // A modification time the platform cannot give, or one in the future
        // (clock skew, a restored volume), reads as "not provably stale" and
        // keeps the file. The sweep exists to reclaim space, not to be the
        // reason a live upload disappears.
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= older_than);
        if !stale {
            report.retained += 1;
            continue;
        }

        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                tracing::info!(
                    path = %path.display(),
                    bytes = metadata.len(),
                    "removed a staged upload left behind by a previous run"
                );
                report.removed += 1;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "failed to remove a staged upload left behind by a previous run"
                );
                report.failed += 1;
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::{sweep_stale_spools, StagingArea, SweepReport, STALE_SPOOL_AGE};
    use std::time::{Duration, SystemTime};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn age_file(path: &std::path::Path, by: Duration) {
        let when = SystemTime::now() - by;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open the spool to backdate it");
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("backdate the spool");
    }

    /// The whole point of the sweep, stated per area rather than in aggregate:
    /// a spool that outlived its request goes, and — the half without which
    /// this proves nothing — a spool that may still belong to one stays. A
    /// sweep that deleted everything it found would pass the first assertion
    /// and destroy a live 512 MiB upload.
    #[tokio::test]
    async fn every_area_loses_its_stale_spools_and_keeps_its_fresh_ones() {
        let root = tempfile::tempdir().expect("repo root");

        for area in StagingArea::ALL {
            let directory = area.path_in(root.path());
            std::fs::create_dir_all(&directory).expect("staging directory");
            let stale = directory.join("abandoned.upload");
            std::fs::write(&stale, b"left behind by a SIGKILL").expect("stale spool");
            age_file(&stale, STALE_SPOOL_AGE + Duration::from_secs(60));
            std::fs::write(directory.join("in-flight.upload"), b"a live request")
                .expect("fresh spool");
        }

        let report = sweep_stale_spools(root.path(), STALE_SPOOL_AGE).await;

        assert_eq!(
            report,
            SweepReport {
                removed: StagingArea::ALL.len(),
                retained: StagingArea::ALL.len(),
                failed: 0,
            },
            "one stale spool per area had to go and one fresh spool per area had to stay"
        );
        for area in StagingArea::ALL {
            let directory = area.path_in(root.path());
            assert!(
                !directory.join("abandoned.upload").exists(),
                "{} still holds the spool of a request that ended with the process",
                directory.display()
            );
            assert!(
                directory.join("in-flight.upload").exists(),
                "{} lost a spool young enough to belong to a live request",
                directory.display()
            );
        }
    }

    /// A first boot, and every boot on an instance that has never used one of
    /// the four areas: the directories do not exist, and that is not a failure
    /// anybody should have to read a warning about.
    #[tokio::test]
    async fn a_repo_root_without_any_staging_directory_sweeps_clean() {
        let root = tempfile::tempdir().expect("repo root");

        let report = sweep_stale_spools(root.path(), STALE_SPOOL_AGE).await;

        assert_eq!(report, SweepReport::default());
    }

    /// Nothing in these directories creates a symlink, so one is something the
    /// sweep does not understand. It is left alone rather than followed —
    /// following it would delete whatever it points at, outside `.tmp`
    /// entirely.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_is_neither_followed_nor_removed() {
        let root = tempfile::tempdir().expect("repo root");
        let outside = root.path().join("precious.db");
        std::fs::write(&outside, b"not a spool").expect("target");
        age_file(&outside, STALE_SPOOL_AGE + Duration::from_secs(60));

        let directory = StagingArea::Attachments.path_in(root.path());
        std::fs::create_dir_all(&directory).expect("staging directory");
        let link = directory.join("pointer.upload");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");

        let report = sweep_stale_spools(root.path(), STALE_SPOOL_AGE).await;

        assert_eq!(report, SweepReport::default(), "a symlink is not a spool");
        assert!(outside.exists(), "the sweep followed a symlink out of .tmp");
        assert!(
            link.symlink_metadata().is_ok(),
            "the sweep removed a symlink"
        );
    }

    /// Every production `.tmp` path in the workspace has to come out of
    /// [`StagingArea::path_in`], because [`StagingArea::ALL`] is what the
    /// startup sweep iterates: a fifth staging directory spelled directly in a
    /// handler is a directory nothing ever reads again.
    ///
    /// The census is a REPORTING sweep, so it reads the production view of each
    /// file — `#[cfg(test)]` items blanked, comments and literals kept — locates
    /// `join(` in the code-only twin, and decodes the argument literal out of
    /// the string-bearing one at the same byte offset. A commented-out or
    /// test-only `join(".tmp")` therefore contributes nothing, and neither does
    /// `strip_suffix(".tmp")`, which is a suffix test rather than a path built
    /// under the staging root.
    #[test]
    fn staging_registry_is_the_only_producer_of_tmp_paths() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let home = workspace
            .join("crates")
            .join("rg-core")
            .join("src")
            .join("staging.rs");

        let mut here = 0_usize;
        let mut elsewhere = Vec::new();
        for file in production_rust_files(&workspace.join("crates")) {
            let text = std::fs::read_to_string(&file).expect("read a workspace source file");
            for line in staging_root_joins(&text) {
                if file == home {
                    here += 1;
                } else {
                    elsewhere.push(format!("{}:{line}", file.display()));
                }
            }
        }

        assert!(
            here > 0,
            "no production `join(\".tmp\"…)` was found in {} — the census went blind and \
             would now stay green over any new staging directory",
            home.display()
        );
        assert!(
            elsewhere.is_empty(),
            "these build a `.tmp` path without going through `StagingArea::path_in`, so \
             `sweep_stale_spools` will never read what they leave behind: {}",
            elsewhere.join(", ")
        );
    }

    /// 1-based lines of the production `join(…)` calls whose argument literal
    /// names the staging root.
    ///
    /// Split out of the census loop so the census reads one file's bytes
    /// through one named view, rather than deciding what a `.tmp` path is
    /// inline over a directory walk.
    fn staging_root_joins(text: &str) -> Vec<usize> {
        let source = rust_source::production_rust_source(text);
        rust_source::production_call_sites(text, &["join"])
            .into_iter()
            .filter(|call| {
                literal_argument(&source, call.open_paren)
                    .is_some_and(|value| value == ".tmp" || value.starts_with(".tmp/"))
            })
            .map(|call| call.line)
            .collect()
    }

    /// The string literal opening a call's argument list, or `None` when the
    /// first argument is not one (a variable, a `format!`, a constant).
    fn literal_argument(source: &str, open_paren: usize) -> Option<String> {
        let rest = source.get(open_paren + 1..)?.trim_start();
        let body = rest.strip_prefix('"')?;
        let end = body.find('"')?;
        // An escape would make the bytes up to the first quote the wrong value;
        // no staging path needs one, so decline rather than guess.
        let value = &body[..end];
        if value.contains('\\') {
            return None;
        }
        Some(value.to_owned())
    }

    fn production_rust_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(directory) = pending.pop() {
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) => panic!("read {}: {error}", directory.display()),
            };
            for entry in entries {
                let path = entry.expect("directory entry").path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if path.is_dir() {
                    // `src/` is the shipped half of every crate; `tests/`,
                    // `target/` and fixtures are not what the sweep has to
                    // reach.
                    if name != "target" && name != "tests" && name != "benches" {
                        pending.push(path);
                    }
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }
}
