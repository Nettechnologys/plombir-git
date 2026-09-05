//! Reading a file that somebody committed, without letting its size choose how
//! much memory this server spends on it.
//!
//! Two steps, always in this order, shared by every reader that has to hold a
//! committed file whole in order to parse it: pin the branch to the commit that
//! answered, then take the blob's size out of the listing that had to run
//! anyway. Neither `git cat-file blob` nor the gateway collecting its output
//! caps the bytes it hands back, so a ceiling spent *after* the read costs
//! exactly the memory it was declared to save — and the size is chosen by
//! whoever can push to the repository.

use std::path::Path;

use anyhow::Result;
use rg_git::cli_gateway::GitCommandGateway;

/// The commit a branch points at, or `None` when the branch is not there.
///
/// The resolved commit id rather than the ref name, because a reader makes more
/// than one read against it: a listing per candidate path plus a blob for the
/// one it settles on. A ref that moved between two of them would answer from
/// two different trees — including the case that matters here, a size taken
/// from one blob and the bytes then read from another. A commit id cannot move.
pub(crate) fn verified_branch_commit(
    git: &GitCommandGateway,
    repository_path: &Path,
    branch: &str,
) -> Result<Option<String>> {
    let branch_ref = format!("refs/heads/{branch}");
    let output = git.run(
        &["show-ref", "--verify", "--quiet", &branch_ref],
        Some(repository_path),
    )?;
    if !output.success() {
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        output.ensure_success()?;
    }

    let commit_spec = format!("{branch_ref}^{{commit}}");
    let output = git.run(
        &["rev-parse", "--verify", "--quiet", &commit_spec],
        Some(repository_path),
    )?;
    output.ensure_success()?;
    let commit = output.stdout_str().trim().to_string();
    if commit.is_empty() {
        anyhow::bail!("`{commit_spec}` verified but named no commit");
    }
    Ok(Some(commit))
}

/// The size git records for the blob at `path`, or `None` when `git_ref` holds
/// no blob there.
///
/// `-l` replaces the `--name-only` of the listing that had to run anyway to tell
/// an absent file from a present one, so the size arrives without a second
/// process. A long record reads
/// `<mode> SP <type> SP <object> SP <padded size> TAB <path>`, and `-z`
/// terminates it with NUL rather than quoting a path that needs escaping.
pub(crate) fn blob_size(
    git: &GitCommandGateway,
    repository_path: &Path,
    git_ref: &str,
    path: &str,
) -> Result<Option<u64>> {
    let listing = git.run(
        &["ls-tree", "-lz", git_ref, "--", path],
        Some(repository_path),
    )?;
    listing.ensure_success()?;
    for record in listing.stdout.split(|byte| *byte == 0) {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        if &record[tab + 1..] != path.as_bytes() {
            continue;
        }
        // The size field is right-aligned inside its column, so the separators
        // are runs of spaces rather than single ones.
        let mut fields = record[..tab]
            .split(|byte| *byte == b' ')
            .filter(|field| !field.is_empty());
        let (Some(_mode), Some(kind), Some(_object), Some(size)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if kind != b"blob" {
            // A tree or a submodule committed at one of the candidate paths is
            // not the file being looked for. Reporting it as absent lets the
            // next candidate be tried, which is what a repository owner meant
            // by putting a file at one of the other ones.
            return Ok(None);
        }
        // A size we cannot read counts as over the ceiling rather than as a
        // reason to read the blob and measure it — that read is the one thing
        // this lookup exists to avoid. `import::service::collect_wiki_pages`
        // reads an `ls-tree -l` size under the same rule.
        return Ok(Some(
            std::str::from_utf8(size)
                .ok()
                .and_then(|size| size.parse::<u64>().ok())
                .unwrap_or(u64::MAX),
        ));
    }
    Ok(None)
}
