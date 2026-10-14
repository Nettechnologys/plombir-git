//! How much one outbound clone may write to this server's disk.
//!
//! A mirror or an import fetches from a remote somebody other than the
//! operator chose, and the git deadline bounds time, not bytes: within two
//! minutes a hostile or merely enormous repository can fill the volume that
//! holds every other repository and the database. [`CloneBudget`] is the bound
//! in bytes, measured on the directory the operation fills.
//!
//! It is a ceiling per operation, not a quota: a first clone may write the
//! whole budget, and every later fetch into the same mirror may *grow* it by
//! the budget again — a mirror that legitimately grows over the years is not
//! refused for having grown, an upstream that balloons between two syncs is.
//! The directory is measured once a second, so a run can overshoot by what it
//! writes between two polls.

use std::path::Path;

use anyhow::Result;
use rg_git::cli_gateway::GitCliError;
use rg_process::DiskBudget;

/// The built-in ceiling, in MiB, on one outbound clone or fetch.
///
/// Generous on purpose: the budget defends against a remote that never stops
/// sending, not against large repositories, and a default below what ordinary
/// heavyweight projects need would break their imports and mirrors on the
/// first sync after an upgrade.
pub const DEFAULT_MAX_CLONE_SIZE_MB: u64 = 10_240;

/// One operation's ceiling, and the configuration key an operator raises it
/// with — the refusal names that key, because it is the only thing the
/// operator reading it can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloneBudget {
    max_bytes: Option<u64>,
    config_key: &'static str,
}

impl CloneBudget {
    /// `max_size_mb` as an operator spells it: MiB, `0` for no ceiling.
    pub const fn from_mb(config_key: &'static str, max_size_mb: u64) -> Self {
        Self {
            max_bytes: if max_size_mb == 0 {
                None
            } else {
                Some(max_size_mb.saturating_mul(1024 * 1024))
            },
            config_key,
        }
    }

    /// The built-in ceiling under `config_key`.
    pub const fn default_for(config_key: &'static str) -> Self {
        Self::from_mb(config_key, DEFAULT_MAX_CLONE_SIZE_MB)
    }

    /// The ceiling in bytes, `None` when the operator removed it.
    pub const fn max_bytes(self) -> Option<u64> {
        self.max_bytes
    }

    /// The budget for a clone into `destination`, which it creates or fills
    /// from empty. `destination` must be the directory the clone owns, never
    /// one that holds other repositories.
    pub fn for_clone(self, destination: &Path) -> Option<DiskBudget> {
        self.max_bytes
            .map(|max_bytes| DiskBudget::new(destination, max_bytes))
    }

    /// The budget for a fetch into the existing repository at `repository`:
    /// what it holds now, plus the ceiling.
    pub fn for_fetch(self, repository: &Path) -> Result<Option<DiskBudget>> {
        let Some(max_bytes) = self.max_bytes else {
            return Ok(None);
        };
        let budget = DiskBudget::new(repository, 0);
        let held = budget.measure().ok_or_else(|| {
            anyhow::anyhow!(
                "could not measure {} before fetching into it",
                repository.display()
            )
        })?;
        Ok(Some(DiskBudget::new(
            repository,
            held.saturating_add(max_bytes),
        )))
    }

    /// What is left of this budget for the LFS objects of the same pass, once
    /// its git half wrote `git_written` bytes.
    ///
    /// A pass is one budget, not two: the pointers a clone brings are ~130
    /// bytes each, so a history that fits the ceiling can name millions of
    /// objects, and each of them used to be fetched up to its own per-object
    /// limit (card_798531f99932).
    pub fn remaining_after(self, git_written: u64) -> PassAllowance {
        PassAllowance {
            remaining: self
                .max_bytes
                .map(|max_bytes| max_bytes.saturating_sub(git_written)),
            max_bytes: self.max_bytes,
            config_key: self.config_key,
        }
    }

    /// The bytes beneath `path` — what a pass's git half has left there. `0`
    /// for a directory that is not there or cannot be read: the budget is a
    /// defence against a remote that never stops sending, and a measurement
    /// that failed is no reason to refuse the objects it would have bounded.
    pub fn measure(path: &Path) -> u64 {
        DiskBudget::new(path, 0).measure().unwrap_or(0)
    }

    /// Whether `error` is this budget refusing a run.
    pub fn refused(error: &anyhow::Error) -> bool {
        matches!(
            error.downcast_ref::<GitCliError>(),
            Some(GitCliError::DiskBudgetExceeded { .. })
        )
    }

    /// Reword a refusal so it says which ceiling stopped the run and how to
    /// raise it; any other error passes through untouched.
    pub fn explain(self, error: anyhow::Error) -> anyhow::Error {
        if !Self::refused(&error) {
            return error;
        }
        let mib = self.max_bytes.unwrap_or(0) / (1024 * 1024);
        error.context(format!(
            "the remote sent more than the {mib} MiB one clone or fetch may write to this \
             server's disk, so the transfer was stopped and its partial copy removed; if the \
             repository is legitimately that large, raise `{key}` (0 removes the ceiling)",
            key = self.config_key,
        ))
    }
}

/// What one pass may still bring after its git half: the LFS objects the new
/// history points at are paid for out of the same [`CloneBudget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassAllowance {
    /// `None` when the operator removed the ceiling.
    remaining: Option<u64>,
    max_bytes: Option<u64>,
    config_key: &'static str,
}

impl PassAllowance {
    /// No ceiling at all — a caller that has no budget to share.
    pub const fn unlimited() -> Self {
        Self {
            remaining: None,
            max_bytes: None,
            config_key: "",
        }
    }

    /// Whether an object of `bytes` still fits; the refusal names the key that
    /// raises the ceiling, because it is the only thing its reader can act on.
    pub fn admit(&self, bytes: u64) -> std::result::Result<(), String> {
        match self.remaining {
            Some(remaining) if bytes > remaining => Err(format!(
                "this pass has already brought what the {} MiB one clone or fetch may write to \
                 this server's disk, its LFS objects included, so this {bytes}-byte object was \
                 not fetched; if the repository is legitimately that large, raise `{}` (0 \
                 removes the ceiling)",
                self.max_bytes.unwrap_or(0) / (1024 * 1024),
                self.config_key,
            )),
            _ => Ok(()),
        }
    }

    /// Account for an object of `bytes` that arrived.
    pub fn charge(&mut self, bytes: u64) {
        if let Some(remaining) = self.remaining.as_mut() {
            *remaining = remaining.saturating_sub(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_megabytes_removes_the_ceiling_and_anything_else_is_mebibytes() {
        assert_eq!(CloneBudget::from_mb("[mirror].x", 0).max_bytes(), None);
        assert_eq!(
            CloneBudget::from_mb("[mirror].x", 3).max_bytes(),
            Some(3 * 1024 * 1024)
        );
        assert_eq!(
            CloneBudget::default_for("[mirror].x").max_bytes(),
            Some(10_240 * 1024 * 1024)
        );
        assert_eq!(
            CloneBudget::from_mb("[mirror].x", u64::MAX).max_bytes(),
            Some(u64::MAX),
            "an absurd setting saturates instead of wrapping to a tiny ceiling"
        );
    }

    #[test]
    fn the_lfs_half_of_a_pass_gets_what_its_git_half_left() {
        let budget = CloneBudget::from_mb("[mirror].max_clone_size_mb", 3);
        let mut allowance = budget.remaining_after(2 * 1024 * 1024);
        allowance.admit(1024 * 1024).expect("one MiB is left");
        allowance.charge(1024 * 1024);
        let refused = allowance.admit(1).expect_err("nothing is left");
        assert!(
            refused.contains("`[mirror].max_clone_size_mb`"),
            "{refused}"
        );
        assert!(refused.contains("3 MiB"), "{refused}");

        let over = budget.remaining_after(5 * 1024 * 1024);
        assert!(
            over.admit(1).is_err(),
            "a git half past the ceiling leaves nothing"
        );

        let unbounded =
            CloneBudget::from_mb("[mirror].max_clone_size_mb", 0).remaining_after(u64::MAX);
        unbounded.admit(u64::MAX).expect("0 removes the ceiling");
        PassAllowance::unlimited()
            .admit(u64::MAX)
            .expect("no budget to share");
    }

    #[test]
    fn a_fetch_may_grow_the_repository_by_the_budget_not_to_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pack"), vec![0u8; 3 * 1024 * 1024]).unwrap();

        let budget = CloneBudget::from_mb("[mirror].x", 1)
            .for_fetch(dir.path())
            .unwrap()
            .expect("a ceiling is configured");
        assert_eq!(budget.max_bytes, 4 * 1024 * 1024);

        assert_eq!(
            CloneBudget::from_mb("[mirror].x", 0)
                .for_fetch(dir.path())
                .unwrap(),
            None
        );
    }

    #[test]
    fn the_refusal_names_the_key_that_raises_it() {
        let refusal = anyhow::Error::new(GitCliError::DiskBudgetExceeded {
            command: "git clone --mirror".into(),
            path: "/srv/1.mirror".into(),
            bytes: 9,
            limit_bytes: 8,
        });
        let message = format!(
            "{:#}",
            CloneBudget::from_mb("[mirror].max_clone_size_mb", 5).explain(refusal)
        );
        assert!(
            message.contains("`[mirror].max_clone_size_mb`"),
            "{message}"
        );
        assert!(message.contains("5 MiB"), "{message}");

        let other = CloneBudget::from_mb("[mirror].max_clone_size_mb", 5)
            .explain(anyhow::anyhow!("connection refused"));
        assert_eq!(format!("{other:#}"), "connection refused");
    }
}
