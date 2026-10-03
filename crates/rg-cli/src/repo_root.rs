//! The one place a `plombir-git` subcommand decides what a missing repository
//! storage root means.
//!
//! [`crate::config::DEFAULT_REPO_ROOT`] is *relative* (`./repos`), and the
//! missing directory is created rather than reported: `create_dir_all` exists
//! to do exactly that. Together that means any subcommand started from the
//! wrong directory — `docker exec` without `-w`, a cron entry, another shell —
//! silently builds a second, empty repository root beside itself instead of
//! addressing the one the server serves from, and nothing downstream can tell
//! the difference: the directory is there because it was just made.
//!
//! `import` is where that becomes data loss rather than a stray directory. It
//! writes the repository's row into the *real* database (`--db-url` or
//! `--config` was right; `--repo-root` was forgotten) and clones the git
//! repository into a root `plombir-git serve` never looks at. The instance then
//! lists a repository whose git directory nobody can open, and no step on the
//! way reported anything (card_cc8259eba428).
//!
//! This is the second half of the class `crate::dbconn` closed for
//! `[database].url` (card_8baddb74fa82), and it cannot be closed the same way:
//! refusing every root that is not there would break the clean install, where
//! creating it is the correct answer. So the question is asked of the database
//! that was opened alongside it — an instance that already has repositories has
//! a root somewhere, and one that is not there is a mistake about *which* root.

use std::path::Path;

use rg_db::DatabaseConnection;

/// What a subcommand does when the repository storage root it resolved is not
/// there.
#[derive(Clone, Copy)]
pub(crate) enum MissingRepoRoot {
    /// Create it, after saying so — but only on an instance that stores no
    /// repositories yet. Reserved for the commands whose job includes bringing
    /// a root into existence on a clean install.
    CreateOnACleanInstance,
    /// Refuse whatever the database holds. For the commands that only ever
    /// *read* repositories out of the root: one that is not there cannot become
    /// the right one by being created.
    Refuse,
}

/// Refuse — or, for the commands allowed to create one, announce — a repository
/// storage root that does not exist yet.
///
/// Call this before the `create_dir_all` (or the read) that would otherwise
/// answer the question by acting on it. `db` is the connection the command has
/// already opened, and it is what makes the two cases separable at all: see the
/// module doc.
pub(crate) async fn check_repo_root_presence(
    db: &DatabaseConnection,
    repo_root: &Path,
    operation: &str,
    missing: MissingRepoRoot,
) -> anyhow::Result<()> {
    match repo_root.try_exists() {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            // The probe failed, so nothing was established either way. The
            // command is about to touch the same path and will report the same
            // reason in the filesystem's own words; inventing a "does not
            // exist" here would send the operator after the wrong thing.
            tracing::debug!(
                repo_root = %repo_root.display(),
                %error,
                "could not establish whether the repository storage root exists; leaving the \
                 command's own path handling to report it"
            );
            return Ok(());
        }
    }

    let repositories = rg_db::ops::repo_ops::count_non_deleted(db)
        .await
        .map_err(|error| {
            error.context(
                "count the repositories this instance already has, to tell a clean install \
                 from a wrong repository root",
            )
        })?;
    let resolved = crate::config::absolute_path(repo_root);

    if repositories == 0 {
        if let MissingRepoRoot::CreateOnACleanInstance = missing {
            announce_a_new_repo_root(&resolved);
            return Ok(());
        }
    }

    // Both halves are worth saying. A populated instance names the count,
    // because that is the evidence that a root exists somewhere and this is not
    // it; an empty one says so too, because "no repositories at all" is itself
    // an answer about which database the command reached.
    let instance = if repositories == 1 {
        "the database it opened already describes 1 repository".to_string()
    } else if repositories > 1 {
        format!("the database it opened already describes {repositories} repositories")
    } else {
        "the database it opened describes no repositories at all, so there is nothing under any \
         root for this command to read"
            .to_string()
    };

    anyhow::bail!(
        "`{operation}` was pointed at the repository storage root `{}`, which does not exist \
         (resolved to `{}`), while {instance}.\n  A relative repository root resolves against \
         the current directory, so a command started somewhere else — `docker exec` without \
         `-w`, a cron entry, another shell — addresses a root that is not the one this instance \
         serves from. Nothing was created: a repository written there would have its row in the \
         database and its git directory where `plombir-git serve` never looks.\n  hint: pass \
         `--repo-root` or `--config`, or run from the data directory \
         (`docker exec -w /data ...`)",
        repo_root.display(),
        resolved.display()
    )
}

/// Say out loud, in absolute terms, that a repository root is about to be
/// brought into existence.
///
/// The relative spelling is the same on the machine where this is the first
/// start of a clean install and on the one where it is a mistake, so the
/// absolute path is the whole of what this line adds.
pub(crate) fn announce_a_new_repo_root(resolved: &Path) {
    tracing::warn!(
        repo_root = %resolved.display(),
        "creating a NEW, empty repository storage root — if this instance already keeps \
         repositories elsewhere, stop now and point `--repo-root` / `--config` at it"
    );
}
