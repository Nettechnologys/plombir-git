//! One fail-closed view of the refs exposed by Git protocol advertisements.

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
    use super::collect;

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
