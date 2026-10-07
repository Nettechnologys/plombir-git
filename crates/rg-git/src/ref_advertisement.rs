//! One fail-closed view of the refs exposed by Git protocol advertisements.
//!
//! [`collect`] is the server's own view, every ref the repository holds.
//! [`collect_for_clients`] is what a protocol advertisement may show: the same
//! minus the namespaces the server writes for its own work. Those refs used to
//! go out to every `git ls-remote` and `git clone --mirror` — the branch names
//! of other people's forks among them — and a mirror client then kept their
//! objects alive as well (card_18044eadb6d6).

use std::path::Path;

use anyhow::{Context, Result};

/// The repository state needed by v0/v1 advertisements and protocol v2
/// `ls-refs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefAdvertisement {
    /// All refs below `refs/`, with symbolic refs resolved to their object id.
    pub refs: Vec<(String, String)>,
    /// The peeled object id of `HEAD`, or `None` only for a legitimate unborn
    /// repository.
    pub head_oid: Option<String>,
    /// The symbolic target of `HEAD`, when it is not detached.
    pub head_target: Option<String>,
}

/// Ref namespaces the server writes for itself and shows no client.
///
/// * `refs/forks/` — the scratch copy of a fork pull request's head, fetched
///   into the base repository to diff or merge it. Removed when that operation
///   ends; older versions left one per pull request behind.
/// * `refs/merge-queue/` — the merge-group commit of a queued pull request,
///   kept so the commit outlives the pass that built it. Runners receive the
///   workspace as an archive, not by fetching this ref.
pub const SERVER_PRIVATE_NAMESPACES: &[&str] = &["refs/forks/", "refs/merge-queue/"];

/// Whether `refname` lies in one of [`SERVER_PRIVATE_NAMESPACES`].
pub fn is_server_private(refname: &str) -> bool {
    SERVER_PRIVATE_NAMESPACES
        .iter()
        .any(|namespace| refname.starts_with(namespace))
}

/// The refs a protocol advertisement shows a client: [`collect`] without the
/// server's private namespaces.
pub fn collect_for_clients(repo_path: &Path) -> Result<RefAdvertisement> {
    let mut advertisement = collect(repo_path)?;
    advertisement
        .refs
        .retain(|(_, refname)| !is_server_private(refname));
    Ok(advertisement)
}

/// Read every ref required to build a repository advertisement.
///
/// An advertisement is a snapshot clients cache and act on. Returning a
/// partial snapshot is therefore worse than failing the operation: a corrupt
/// ref must not look like a branch or tag was deleted. The only empty state
/// accepted here is the explicit `gix::head::Kind::Unborn` state represented by
/// `Head::try_into_peeled_id()` as `Ok(None)`.
pub fn collect(repo_path: &Path) -> Result<RefAdvertisement> {
    let repo = crate::repository::open(repo_path).with_context(|| {
        format!(
            "failed to open repository for ref advertisement: {}",
            repo_path.display()
        )
    })?;

    let head = repo
        .head()
        .with_context(|| format!("failed to read HEAD in {}", repo_path.display()))?;
    let head_target = head.referent_name().map(|name| name.as_bstr().to_string());
    let head_oid = head
        .try_into_peeled_id()
        .with_context(|| format!("failed to resolve HEAD in {}", repo_path.display()))?
        .map(|id| id.to_string());

    let references = repo
        .references()
        .with_context(|| format!("failed to open references in {}", repo_path.display()))?;
    let all_refs = references
        .all()
        .with_context(|| format!("failed to list references in {}", repo_path.display()))?;
    let mut refs = Vec::new();

    for reference in all_refs {
        let mut reference = reference
            .map_err(anyhow::Error::from_boxed)
            .with_context(|| format!("failed to read a reference in {}", repo_path.display()))?;
        let refname = reference.name().as_bstr().to_string();

        // HEAD is read above so its unborn state and symbolic target remain
        // explicit. `references().all()` normally enumerates only `refs/`, but
        // keep the invariant if a backend ever includes pseudo-refs too.
        if refname == "HEAD" {
            continue;
        }

        let oid = match reference.try_id().map(|id| id.to_string()) {
            Some(oid) => oid,
            None => reference
                .peel_to_id()
                .with_context(|| {
                    format!(
                        "failed to resolve symbolic reference `{refname}` in {}",
                        repo_path.display()
                    )
                })?
                .to_string(),
        };
        refs.push((oid, refname));
    }

    Ok(RefAdvertisement {
        refs,
        head_oid,
        head_target,
    })
}

#[cfg(test)]
mod tests {
    use super::{collect, collect_for_clients};

    #[test]
    fn clients_are_not_shown_the_server_private_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = crate::test_support::repository_with_server_private_refs(dir.path());
        // A name that merely starts like a private namespace is an ordinary ref.
        let main = repo_path.join("refs/heads/main");
        std::fs::create_dir_all(repo_path.join("refs/forksmith")).unwrap();
        std::fs::copy(&main, repo_path.join("refs/forksmith/kept")).unwrap();

        let names = |refs: Vec<(String, String)>| {
            let mut names: Vec<String> = refs.into_iter().map(|(_, name)| name).collect();
            names.sort();
            names
        };
        assert_eq!(
            names(collect_for_clients(&repo_path).unwrap().refs),
            ["refs/forksmith/kept", "refs/heads/main", "refs/tags/v1"]
        );
        assert_eq!(
            collect(&repo_path).unwrap().refs.len(),
            5,
            "the server's own view keeps every ref"
        );
    }

    #[test]
    fn unborn_repository_is_the_only_successful_empty_head() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("unborn.git");
        gix::init_bare(&repo_path).unwrap();

        let advertisement = collect(&repo_path).unwrap();

        assert!(advertisement.refs.is_empty());
        assert!(advertisement.head_oid.is_none());
        assert!(advertisement.head_target.is_some());
    }

    #[test]
    fn malformed_head_is_not_reported_as_unborn() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken-head.git");
        gix::init_bare(&repo_path).unwrap();
        std::fs::write(repo_path.join("HEAD"), "not a ref at all\n").unwrap();

        let error = collect(&repo_path).unwrap_err();

        assert!(format!("{error:#}").contains("failed to read HEAD"));
    }

    #[test]
    fn malformed_ref_cannot_disappear_from_a_partial_advertisement() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken-ref.git");
        gix::init_bare(&repo_path).unwrap();
        std::fs::create_dir_all(repo_path.join("refs/heads")).unwrap();
        std::fs::write(repo_path.join("refs/heads/broken"), "not-an-object-id\n").unwrap();

        let error = collect(&repo_path).unwrap_err();

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("failed to read a reference"),
            "{rendered}"
        );
        assert!(rendered.contains("broken"), "{rendered}");
    }
}
