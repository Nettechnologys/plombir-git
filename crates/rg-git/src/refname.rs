//! Validation for reference names that cross a ForgeKeep trust boundary.
//!
//! gix validates the on-disk ref grammar, but callers also need to distinguish
//! a fully-qualified wire ref from the short branch spelling accepted by REST
//! endpoints. Keeping that distinction here prevents each transport from
//! inventing a slightly different precondition before it reaches gix or git.

use gix::bstr::ByteSlice;

/// Why a caller-supplied Git reference cannot be used by ForgeKeep.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RefNameError {
    #[error("refname must be fully qualified under refs/")]
    NotFullyQualified,
    #[error("branch name must be unqualified")]
    QualifiedBranch,
    #[error("branch name must not start with '-'")]
    DashLeadingBranch,
    #[error("invalid refname: {0}")]
    Invalid(String),
}

/// Validate one complete Git refname received from a protocol client.
///
/// `gix::validate::reference::name` owns Git's byte-level exclusions (`..`,
/// `@{`, control characters, `.lock`, repeated slashes, and the rest). The
/// additional `refs/` requirement keeps pseudo-refs such as `HEAD` out of a
/// receive-pack update, and the branch rule matches `git check-ref-format
/// --branch`: a short branch may not begin with `-` even though the same byte is
/// legal in another component of a general refname.
pub fn validate_refname(refname: &str) -> Result<(), RefNameError> {
    if !refname.starts_with("refs/") {
        return Err(RefNameError::NotFullyQualified);
    }

    gix::validate::reference::name(refname.as_bytes().as_bstr())
        .map_err(|error| RefNameError::Invalid(error.to_string()))?;

    if let Some(branch) = refname.strip_prefix("refs/heads/") {
        if branch.starts_with('-') {
            return Err(RefNameError::DashLeadingBranch);
        }
        gix::validate::reference::branch_name(refname.as_bytes().as_bstr())
            .map_err(|error| RefNameError::Invalid(error.to_string()))?;
    }

    Ok(())
}

/// Validate the short branch spelling accepted by ForgeKeep's REST APIs.
pub fn validate_branch_name(branch: &str) -> Result<(), RefNameError> {
    if branch.starts_with("refs/") {
        return Err(RefNameError::QualifiedBranch);
    }
    validate_refname(&format!("refs/heads/{branch}"))
}

#[cfg(test)]
mod tests {
    use super::{validate_branch_name, validate_refname};

    #[test]
    fn both_ref_boundaries_reject_revspecs_and_ambiguous_branch_spellings() {
        for branch in ["main^", "@{-1}", "refs/heads/-x", "a..b"] {
            assert!(
                validate_branch_name(branch).is_err(),
                "contents branch {branch:?} must be rejected"
            );
        }

        for refname in ["main^", "@{-1}", "refs/heads/-x", "a..b"] {
            assert!(
                validate_refname(refname).is_err(),
                "receive-pack ref {refname:?} must be rejected"
            );
        }
        for refname in ["refs/heads/main^", "refs/heads/@{-1}", "refs/heads/a..b"] {
            assert!(
                validate_refname(refname).is_err(),
                "qualified hostile ref {refname:?} must be rejected"
            );
        }
    }

    #[test]
    fn ordinary_branches_and_non_branch_refs_remain_valid() {
        for branch in ["main", "feature/ref-validation", "release-2026.09"] {
            validate_branch_name(branch).unwrap();
            validate_refname(&format!("refs/heads/{branch}")).unwrap();
        }
        for refname in ["refs/tags/v1.2.3", "refs/notes/review"] {
            validate_refname(refname).unwrap();
        }
    }
}
