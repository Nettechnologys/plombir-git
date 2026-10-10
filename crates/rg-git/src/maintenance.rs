//! Bounded maintenance of server-owned bare repositories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};

const PUSH_QUIET: Duration = Duration::from_secs(60);
const STALE_QUARANTINE: Duration = Duration::from_secs(12 * 60 * 60);
static PENDING: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();

/// Debounce auto-GC per repository. A failed run is logged; the next push or
/// nightly pass retries it. Git's own gc.pid coordinates separate processes.
pub fn after_push(repo_path: &Path) {
    let pending = PENDING.get_or_init(|| Mutex::new(HashMap::new()));
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = repo_path.to_owned();
    let first = pending.insert(path.clone(), Instant::now()).is_none();
    drop(pending);
    if !first {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PUSH_QUIET).await;
            let ready = {
                let mut pending = PENDING
                    .get()
                    .expect("pending map initialized")
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let Some(last_push) = pending.get(&path).copied() else {
                    return;
                };
                if last_push.elapsed() >= PUSH_QUIET {
                    pending.remove(&path);
                    true
                } else {
                    false
                }
            };
            if !ready {
                continue;
            }
            match tokio::task::spawn_blocking(move || run_auto(&path)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, "git auto-GC failed"),
                Err(error) => tracing::warn!(%error, "git auto-GC task did not complete"),
            }
            return;
        }
    });
}

/// Git decides whether the loose-object or pack threshold warrants work.
/// Lowering its stock 50-pack threshold prevents a busy repository from
/// spending most of its time opening dozens of tiny packs.
pub fn run_auto(repo_path: &Path) -> Result<()> {
    run_gc(repo_path, true)
}

/// Once per day, also retire unreachable objects past Git's standard grace
/// period. Never use `--prune=now`: a concurrent writer may still need them.
pub fn run_full(repo_path: &Path) -> Result<()> {
    if let Err(error) = sweep_stale_quarantine(repo_path) {
        tracing::warn!(repo = %repo_path.display(), error = %format!("{error:#}"),
            "stale quarantine sweep failed; proceeding with git GC");
    }
    run_gc(repo_path, false)
}

fn run_gc(repo_path: &Path, automatic: bool) -> Result<()> {
    let gateway = crate::cli_gateway::GitCommandGateway::with_timeout(Duration::from_secs(3600))?;
    let mut args = vec![
        "-c",
        "gc.autoDetach=false",
        "-c",
        "gc.pruneExpire=2.weeks.ago",
    ];
    if automatic {
        args.extend(["-c", "gc.autoPackLimit=20"]);
    }
    args.push("gc");
    if automatic {
        args.push("--auto");
    }
    gateway
        .run(&args, Some(repo_path))?
        .ensure_success()
        .with_context(|| format!("git gc failed for {}", repo_path.display()))
}

/// List only the server's `{namespace}/{name}.git` layout. No symlink or
/// arbitrary directory below repo_root is treated as a repository.
pub fn repositories_under(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut repos = Vec::new();
    for namespace in std::fs::read_dir(repo_root)? {
        let namespace = match namespace {
            Ok(namespace) => namespace,
            Err(error) => {
                tracing::warn!(%error, "could not inspect a git namespace directory");
                continue;
            }
        };
        let kind = match namespace.file_type() {
            Ok(kind) => kind,
            Err(error) => {
                tracing::warn!(namespace = %namespace.path().display(), %error,
                    "could not inspect a git namespace for maintenance");
                continue;
            }
        };
        if !kind.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(namespace.path()) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(namespace = %namespace.path().display(), %error,
                    "could not list a git namespace for maintenance");
                continue;
            }
        };
        for candidate in entries {
            let candidate = match candidate {
                Ok(candidate) => candidate,
                Err(error) => {
                    tracing::warn!(namespace = %namespace.path().display(), %error,
                        "could not inspect a repository entry for maintenance");
                    continue;
                }
            };
            let kind = match candidate.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    tracing::warn!(repo = %candidate.path().display(), %error,
                        "could not inspect a repository for maintenance");
                    continue;
                }
            };
            if !kind.is_dir() || !candidate.file_name().to_string_lossy().ends_with(".git") {
                continue;
            }
            let path = candidate.path();
            if path.join("HEAD").is_file() && path.join("objects").is_dir() {
                repos.push(path);
            }
        }
    }
    Ok(repos)
}

/// Sweep every repository once a day, including repositories that no longer
/// receive pushes. The blocking pass is serialized so it cannot launch a GC
/// storm on a large instance. Shutdown stops future passes.
pub fn spawn_daily(
    repo_root: PathBuf,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let first = tokio::time::Instant::now() + Duration::from_secs(60 * 60);
        let mut interval = tokio::time::interval_at(first, Duration::from_secs(24 * 60 * 60));
        loop {
            tokio::select! {
                _ = interval.tick() => {},
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return; }
                    continue;
                }
            }
            let root = repo_root.clone();
            match tokio::task::spawn_blocking(move || {
                let repos = repositories_under(&root)?;
                for repo in repos {
                    if let Err(error) = run_full(&repo) {
                        tracing::warn!(repo = %repo.display(), error = %format!("{error:#}"),
                            "scheduled git GC failed; the next pass retries");
                    }
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(error = %format!("{error:#}"), "git GC scan failed")
                }
                Err(error) => tracing::warn!(%error, "git GC scan task did not complete"),
            }
        }
    })
}

/// A process killed mid-push cannot drop its TempDir. The age bound leaves a
/// grace window; the file lock protects a live push on a shared repository
/// root even when its configured wall-clock budget is unusually long.
fn sweep_stale_quarantine(repo_path: &Path) -> Result<()> {
    let objects = repo_path.join("objects");
    for candidate in std::fs::read_dir(objects)? {
        let candidate = candidate?;
        if !candidate.file_type()?.is_dir()
            || !candidate
                .file_name()
                .to_string_lossy()
                .starts_with(".receive-quarantine-")
        {
            continue;
        }
        // A timestamp ahead of this host's clock is young, not a reason to
        // stop maintaining the whole repository.
        let age = SystemTime::now()
            .duration_since(candidate.metadata()?.modified()?)
            .unwrap_or_default();
        if age >= STALE_QUARANTINE {
            let lock_path = candidate.path().join("active.lock");
            let _lock = if lock_path.is_file() {
                let lock = std::fs::File::open(lock_path)?;
                match fs2::FileExt::try_lock_exclusive(&lock) {
                    Ok(()) => Some(lock),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        // Another process still owns this request.
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                }
            } else {
                None
            };
            std::fs::remove_dir_all(candidate.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_ignores_non_repositories() {
        let temp = tempfile::tempdir().unwrap();
        let owner = temp.path().join("owner");
        let bare = owner.join("repo.git");
        std::fs::create_dir_all(bare.join("objects")).unwrap();
        std::fs::write(bare.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(owner.join("lookalike.git")).unwrap();
        assert_eq!(repositories_under(temp.path()).unwrap(), vec![bare]);
    }

    #[test]
    fn fresh_quarantine_is_retained() {
        let temp = tempfile::tempdir().unwrap();
        let objects = temp.path().join("objects");
        std::fs::create_dir(&objects).unwrap();
        let live = objects.join(".receive-quarantine-live");
        std::fs::create_dir(&live).unwrap();
        sweep_stale_quarantine(temp.path()).unwrap();
        assert!(live.is_dir());
    }

    #[test]
    fn stale_quarantine_waits_for_its_owner_lock() {
        let temp = tempfile::tempdir().unwrap();
        let objects = temp.path().join("objects");
        std::fs::create_dir(&objects).unwrap();
        let stale = objects.join(".receive-quarantine-stale");
        std::fs::create_dir(&stale).unwrap();
        let active = std::fs::File::create(stale.join("active.lock")).unwrap();
        fs2::FileExt::lock_exclusive(&active).unwrap();
        let old = SystemTime::now() - STALE_QUARANTINE - Duration::from_secs(60);
        std::fs::File::open(&stale)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();

        sweep_stale_quarantine(temp.path()).unwrap();
        assert!(stale.is_dir(), "active quarantine was removed");
        drop(active);
        sweep_stale_quarantine(temp.path()).unwrap();
        assert!(!stale.exists(), "orphaned quarantine was retained");
    }
}
