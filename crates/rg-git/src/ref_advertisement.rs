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

/// Ref namespaces the server stores nothing in but gives a meaning of its own,
/// so a client may not create refs there either.
///
/// * `refs/pull/` — `refs/pull/<n>/head` is the ref name of a pull request's
///   pipelines: their concurrency group, and what closing or merging the pull
///   request cancels. A pushed `refs/pull/7/head` would run a `push` pipeline
///   under that name, in that group.
/// * `refs/replace/` — git substitutes the object a replace ref names for the
///   one it replaces on every read. A pushed one would change what the
///   server's own checks see: the signed-commit and LFS-lock checks would read
///   the replacement, not the commit the branch actually gets.
pub const SERVER_RESERVED_NAMESPACES: &[&str] = &["refs/pull/", "refs/replace/"];

/// Whether a client's push may not write `refname`: one of the server's own
/// namespaces, written or reserved.
pub fn is_server_owned(refname: &str) -> bool {
    is_server_private(refname)
        || SERVER_RESERVED_NAMESPACES
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

/// The first of `wants` a fetch may not ask for, or `None` when every one is
/// fine (card_ad83ad72d14a).
///
/// A client asks for what an advertisement showed it. A want that is no
/// advertised tip is accepted only when it is a commit reachable from one —
/// the ref may have moved between the advertisement and the request, which is
/// how stateless HTTP behaves, and what git's own `upload-pack` accepts there.
/// Anything else is an object the client learnt some other way: the tip of a
/// server-private ref, or a commit a force push left behind. Packing it would
/// hand out exactly what [`collect_for_clients`] keeps out of the
/// advertisement.
pub fn unadvertised_want(repo_path: &Path, wants: &[String]) -> Result<Option<String>> {
    let advertisement = collect_for_clients(repo_path)?;
    let advertised: std::collections::HashSet<&str> = advertisement
        .refs
        .iter()
        .map(|(oid, _)| oid.as_str())
        .chain(advertisement.head_oid.as_deref())
        .collect();
    let candidates: Vec<&String> = wants
        .iter()
        .filter(|want| !advertised.contains(want.as_str()))
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }

    let repo = crate::repository::open(repo_path).with_context(|| {
        format!(
            "failed to open repository to check fetch wants: {}",
            repo_path.display()
        )
    })?;
    for want in &candidates {
        let id = gix::ObjectId::from_hex(want.as_bytes())
            .with_context(|| format!("fetch want {want} is not an object id"))?;
        // An object store that cannot answer is a failure of the check, not a
        // want that names nothing.
        let header = repo
            .try_find_header(id)
            .with_context(|| format!("failed to look up fetch want {want}"))?;
        if header.is_none_or(|header| header.kind() != gix::object::Kind::Commit) {
            return Ok(Some((*want).clone()));
        }
    }
    drop(repo);

    for want in candidates {
        if !reachable_from_advertised_refs(repo_path, want)? {
            return Ok(Some(want.clone()));
        }
    }
    Ok(None)
}

/// Whether `commit` is reachable from a ref [`collect_for_clients`] shows:
/// `rev-list` names nothing reachable from it once everything reachable from
/// those refs is taken away.
fn reachable_from_advertised_refs(repo_path: &Path, commit: &str) -> Result<bool> {
    let excludes: Vec<String> = SERVER_PRIVATE_NAMESPACES
        .iter()
        .map(|namespace| format!("--exclude={namespace}*"))
        .collect();
    let mut args: Vec<&str> = vec!["rev-list", "-n1", commit, "--not"];
    args.extend(excludes.iter().map(String::as_str));
    args.push("--all");
    let output = crate::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .run(&args, Some(repo_path))?;
    output.ensure_success()?;
    Ok(output.stdout_str().trim().is_empty())
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
